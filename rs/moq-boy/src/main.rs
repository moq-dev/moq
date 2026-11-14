//! MoQ Boy: a crowd-controlled Game Boy Color emulator that streams over MoQ.
//!
//! Architecture:
//! - **Emulator thread** (blocking): runs the Game Boy at ~59.73fps, captures
//!   framebuffers and audio samples, publishes status JSON.
//! - **Video encoder thread**: receives RGBA frames, converts to H.264, publishes.
//! - **Audio encoder** (on emulator thread): resamples and encodes to Opus.
//! - **Monitor tasks** (async): watch video/audio track subscriptions to
//!   pause/resume the emulator when no viewers are watching.
//! - **Viewer handler** (async): accepts each viewer's socket session on the
//!   game's endpoint, relays its button commands to the emulator, and carries
//!   its latency back.
//!
//! Pause/resume state machine:
//! ```text
//!   Wanted { video, audio, video_closed, audio_closed } under the condvar's mutex
//!     neither wanted and some track still open → paused (emulation stops, condvar blocks)
//!     anything wanted or both closed → resumed (condvar notified)
//!
//!   On resume → force video keyframe, reanchor audio
//! ```
//!
//! Emulator state is preserved across pauses: a new viewer joining after a
//! break picks up mid-playthrough rather than seeing a fresh boot.

use anyhow::{Context, Result};
use bytes::Bytes;
use kio::clock::Instant;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: moq_tokio::jemalloc::Allocator = moq_tokio::jemalloc::Allocator;

mod audio;
mod emulator;
mod input;
mod stats;
mod status;
mod video;

#[derive(usage_rs::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq-boy")]
#[usage(completion, settings, unknown_env(prefix = "MOQ_"))]
pub struct Config {
	/// Path to the Game Boy ROM file.
	#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("gb", "gbc"))]
	pub rom: PathBuf,

	/// Session name (used in broadcast path). Defaults to ROM filename.
	#[usage(long)]
	pub name: Option<String>,

	/// Base path prefix. Used to derive the --prefix-game default.
	#[usage(long, default = "boy")]
	pub prefix: String,

	/// Path prefix for game broadcasts ("{prefix-game}/{name}"), the game's server endpoint
	/// ("{prefix-game}/.server/{name}"), which viewers dial, and the viewers' client endpoints
	/// ("{prefix-game}/.client/{id}"). Defaults to "{prefix}/game".
	#[usage(long)]
	pub prefix_game: Option<String>,

	/// Released spelling of the viewer prefix, refused at startup.
	#[usage(long, env = "MOQ_PREFIX_VIEWER", hide = true)]
	prefix_viewer: Option<String>,

	/// Location label shown in viewer stats (e.g. "Dallas, TX").
	#[usage(long)]
	pub location: Option<String>,

	/// The MoQ client configuration.
	#[usage(flatten)]
	pub client: moq_tokio::connect::Config,

	/// QUIC transport tuning (`--quic-*`).
	#[usage(flatten)]
	pub quic: moq_tokio::quic::Config,

	/// The log configuration.
	#[usage(flatten)]
	pub log: moq_tokio::Log,
}

impl Config {
	/// Refuse a config parsed from released spellings, naming what replaced each.
	///
	/// Checked before anything reads the config: those spellings land on hidden
	/// fields that nothing honors, so continuing would dial with settings the
	/// command line never asked for.
	fn check_deprecated(&self) -> Result<()> {
		let mut deprecated = self.client.deprecated();
		deprecated.extend(self.quic.deprecated());
		if self.prefix_viewer.is_some() {
			deprecated.removed(
				"--prefix-viewer",
				Some("MOQ_PREFIX_VIEWER"),
				"viewers dial the game's socket endpoint under --prefix-game",
			);
		}
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		Ok(())
	}

	/// The game prefix, which holds both the game broadcast and the sockets of the game and
	/// its viewers.
	///
	/// Each piece is parsed on its own and joined, so an empty (root) prefix adds no segment.
	fn game_prefix(&self) -> Result<moq_net::Path> {
		Ok(match &self.prefix_game {
			Some(game) => moq_net::Path::new(game).context("invalid --prefix-game")?,
			None => moq_net::Path::new(&self.prefix)
				.context("invalid --prefix")?
				.join(&moq_net::Path::from_static("game"))?,
		})
	}

	/// Sockets need moq-lite-07 or moq-transport on every hop, so offer moq-lite-07 unless
	/// the command line picks versions.
	fn client(&self) -> moq_tokio::connect::Config {
		let mut client = self.client.clone();
		if client.version.is_empty() {
			client.version = vec!["moq-lite-07-wip".parse().expect("a known version")];
		}
		client
	}
}

/// Refuse a name that can't be the game's endpoint id: one path segment.
fn check_name(name: &str) -> Result<()> {
	anyhow::ensure!(
		!name.is_empty() && !name.contains(['/', '\\']),
		"invalid --name {name:?}: a name is one path segment"
	);
	Ok(())
}

/// Shared state for a game session, accessible from multiple threads/tasks.
///
/// Everything here is either atomic, behind a mutex, or immutable -
/// safe to share via `Arc<Session>` between the emulator thread,
/// track monitors, and async tasks.
struct Session {
	video_encoder: video::VideoEncoder,
	video_track: moq_net::track::Demand,
	audio_track: moq_net::track::Demand,

	/// Which tracks viewers want, guarded by the mutex the emulator waits on.
	pause: Pause,

	/// Location label for status reporting.
	location: Option<String>,
}

/// Which tracks some viewer wants. Paused means neither is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Wanted {
	video: bool,
	audio: bool,
	/// The video track's demand ended: it counts as unwanted from now on.
	video_closed: bool,
	/// The audio track's demand ended: it counts as unwanted from now on.
	audio_closed: bool,
}

impl Wanted {
	fn paused(&self) -> bool {
		!(self.video || self.audio || (self.video_closed && self.audio_closed))
	}
}

/// The demand, guarded by the mutex the emulator waits on.
#[derive(Default)]
struct Pause {
	wanted: Mutex<Wanted>,
	cvar: Condvar,
}

impl Pause {
	/// Change the demand under the lock, then wake the emulator.
	fn update(&self, f: impl FnOnce(&mut Wanted)) {
		f(&mut self.wanted.lock().unwrap());
		self.cvar.notify_all();
	}

	/// Current demand, read under the lock.
	fn get(&self) -> Wanted {
		*self.wanted.lock().unwrap()
	}

	/// Block until some track is wanted (or demand closed).
	fn wait(&self) {
		drop(
			self.cvar
				.wait_while(self.wanted.lock().unwrap(), |d| d.paused())
				.unwrap(),
		);
	}
}

/// Which track a monitor updates.
#[derive(Clone, Copy, Debug)]
enum Kind {
	Video,
	Audio,
}

/// Wait until some viewer wants the track, or none does.
async fn until_wanted(demand: &moq_net::track::Demand, wanted: bool) -> moq_net::Result<()> {
	let mut demand = demand.clone();
	while demand.wanted_changed().await?.is_some() != wanted {}
	Ok(())
}

impl Session {
	/// Mirror one track's subscription state into the shared demand.
	async fn run_track_monitor(&self, kind: Kind) {
		let track = match kind {
			Kind::Video => &self.video_track,
			Kind::Audio => &self.audio_track,
		};
		let set = |wanted: bool| {
			self.pause.update(|d| match kind {
				Kind::Video => d.video = wanted,
				Kind::Audio => d.audio = wanted,
			})
		};
		loop {
			if until_wanted(track, true).await.is_err() {
				break;
			}
			tracing::info!(?kind, "viewer subscribed");
			set(true);

			if until_wanted(track, false).await.is_err() {
				break;
			}
			tracing::info!(?kind, "no viewers");
			set(false);
		}
		// A closed track is unwanted; once both close, the emulator never blocks again.
		self.pause.update(|d| match kind {
			Kind::Video => (d.video, d.video_closed) = (false, true),
			Kind::Audio => (d.audio, d.audio_closed) = (false, true),
		});
	}

	/// Block the emulator thread until viewers connect.
	///
	/// Emulator state is preserved across pauses so that a new viewer
	/// joins mid-playthrough rather than a fresh boot.
	fn wait_for_resume(&self) {
		tracing::info!("pausing encoding");
		self.pause.wait();
		tracing::info!("resuming encoding");
	}

	/// Publish status if it changed since last frame.
	fn publish_status(
		&self,
		emu: &emulator::Emulator,
		players: usize,
		game_stats: &stats::Stats,
		publisher: &mut status::StatusPublisher,
	) {
		let held: Vec<_> = emu.pressed_buttons().iter().copied().collect();

		let status = status::Status {
			buttons: held,
			players,
			stats: game_stats.report(),
			location: self.location.clone(),
		};

		publisher.publish(&status);
	}
}

async fn run(config: &Config) -> Result<()> {
	let rom_path = config.rom.clone();

	// Default name to ROM filename without extension.
	let name = config.name.clone().unwrap_or_else(|| {
		rom_path
			.file_stem()
			.and_then(|s| s.to_str())
			.unwrap_or("unknown")
			.to_string()
	});

	tracing::info!(rom = %rom_path.display(), %name, "starting Game Boy emulator");
	check_name(&name)?;

	let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<input::Command>(64);
	let url = config.client.url.clone().context("--connect is required")?;
	let client = config.client().init(config.quic.clone())?;

	// One origin, both directions: the game broadcast and the game's endpoint are published
	// from it, and the viewers' endpoints arrive in it.
	let origin = moq_tokio::origin::spawn();
	let game_prefix = config.game_prefix()?;
	let broadcast_path = game_prefix.join(&moq_net::Path::new(&name).context("invalid --name")?)?;

	// Announce the game session's route; it owns the broadcast.
	let route = origin
		.route(&broadcast_path, moq_net::origin::RouteOptions::default())
		.context("failed to announce broadcast")?;
	tracing::info!(broadcast = %broadcast_path, instance = %route.instance(), "announced");

	// The server endpoint viewers dial, at "{prefix-game}/.server/{name}" from its first accept.
	let endpoint = moq_net::socket::Endpoint::new(&origin, game_prefix, &name).context("failed to open endpoint")?;

	tracing::info!(url = %moq_tokio::RedactedUrl::new(&url), %name, broadcast = %broadcast_path, "connecting to relay");

	let reconnect = client.with_publisher(&origin).with_subscriber(origin).connect(url);

	// Set up catalog and encoders.
	let catalog = moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), moq_mux::catalog::Config::default())?;
	let video_encoder = video::VideoEncoder::spawn(route.broadcast().clone(), catalog.clone()).await;

	let audio_encoder = audio::AudioEncoder::new(route.broadcast().clone(), catalog.clone(), 44100)?;

	let video_track = video_encoder.demand.clone();
	let audio_track = audio_encoder.demand();

	let status_publisher = status::StatusPublisher::new(&mut route.broadcast().clone())?;

	let session = Arc::new(Session {
		video_encoder,
		video_track,
		audio_track,
		pause: Pause::default(), // Paused until the first viewer.
		location: config.location.clone(),
	});

	// Monitor track subscriptions.
	let s = session.clone();
	tokio::spawn(async move { s.run_track_monitor(Kind::Video).await });

	let s = session.clone();
	tokio::spawn(async move { s.run_track_monitor(Kind::Audio).await });

	// Run the emulator on a blocking thread.
	let emulator_handle = tokio::task::spawn_blocking({
		let session = session.clone();
		move || run_emulator(session, &rom_path, audio_encoder, status_publisher, cmd_rx)
	});

	// Keep the result out of the `select!` arms (a `?` there would return before
	// the close below runs), so the broadcast is always closed on shutdown.
	let result = tokio::select! {
		res = emulator_handle => match res {
			Ok(inner) => inner.context("emulator error"),
			Err(join) => Err(join.into()),
		},
		res = reconnect.closed() => res.map_err(Into::into),
		res = input::handle_viewers(&endpoint, &cmd_tx) => res.context("viewer endpoint failed"),
	};

	// Close the broadcast now, even if the emulator thread still holds a clone.
	route.broadcast().close();
	endpoint.close();
	result
}

/// The main emulator loop, running on a blocking thread.
///
/// Ticks the Game Boy at ~59.73fps (the real hardware rate), captures
/// video/audio, publishes status, and handles pause/resume.
fn run_emulator(
	session: Arc<Session>,
	rom_path: &std::path::Path,
	mut audio_encoder: audio::AudioEncoder,
	mut status_publisher: status::StatusPublisher,
	mut cmd_rx: tokio::sync::mpsc::Receiver<input::Command>,
) -> Result<()> {
	let mut emu = emulator::Emulator::new(rom_path)?;
	let start = Instant::now();

	// Game Boy runs at exactly 59.727 Hz (4194304 Hz CPU / 70224 cycles per frame).
	// 1/59.727 ≈ 16742 microseconds per frame.
	let frame_duration = Duration::from_micros(16_742);
	let mut next_frame = Instant::now();
	// Where each viewer with an open session gets its replies.
	let mut viewers: HashMap<input::ViewerId, input::Replies> = HashMap::new();
	let mut game_stats = stats::Stats::new();
	let mut was_audio_active = false;

	loop {
		// Block when no viewers are watching. See state diagram in module docs.
		if session.pause.get().paused() {
			// Mark the break BEFORE parking, not after resuming. Our PTS is the emulator
			// clock, which keeps running while we sleep, so the next frame lands however
			// long the pause was into the future. Published as a marker group that gap
			// reads as a discontinuity rather than one enormous frame duration, and the
			// marker becomes the live edge -- so a viewer arriving mid-pause waits for real
			// media instead of being served the pre-pause group as if it were live.
			session.video_encoder.discontinuity();
			audio_encoder.discontinuity()?;

			session.wait_for_resume();

			// Don't try to catch up after a pause.
			next_frame = Instant::now();
			// Reset tick timer so pause duration isn't counted.
			game_stats.reset_tick();
			// Force a keyframe so new viewers can start decoding.
			session.video_encoder.force_keyframe();
			// Re-anchor audio timestamps so the pause gap appears in PTS.
			audio_encoder.reanchor();
		}

		// Drain pending viewer commands before sleeping, so input that
		// arrived during the previous frame's work is applied immediately.
		{
			let elapsed = start.elapsed();
			let encode_ms = u32::try_from(session.video_encoder.encode_duration().as_millis()).unwrap_or(u32::MAX);

			while let Ok(cmd) = cmd_rx.try_recv() {
				match cmd {
					input::Command::Joined { viewer, replies } => {
						viewers.insert(viewer, replies);
					}
					input::Command::Buttons {
						buttons,
						viewer,
						timestamps,
					} => {
						emu.set_buttons(viewer, buttons.into_iter().collect());

						let mut breakdown = Vec::new();
						let entry = |label: &str, ms: u32| status::LatencyEntry {
							label: label.to_string(),
							ms,
						};

						breakdown.push(entry("encode", encode_ms));

						let ms_saturating = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);

						// Compute latency for each viewer-reported timestamp.
						for t in &timestamps {
							let latency = elapsed.saturating_sub(t.ts);
							breakdown.push(entry(&t.label, ms_saturating(latency)));
						}

						// Input: gap between what the viewer sees and what the server
						// is currently emulating. Uses the oldest viewer timestamp
						// (the rendered frame the user reacted to).
						if let Some(min_ts) = timestamps.iter().map(|t| t.ts).min() {
							let latency = elapsed.saturating_sub(min_ts);
							breakdown.push(entry("input", ms_saturating(latency)));
						}

						if let Some(replies) = viewers.get_mut(&viewer) {
							replies.latency(&breakdown);
						}
					}
					input::Command::Left { viewer } => {
						emu.viewer_left(viewer);
						viewers.remove(&viewer);
					}
					input::Command::Reset => {
						tracing::info!("resetting emulator (viewer request)");
						emu.reset()?;
						game_stats = stats::Stats::new();
					}
				}
			}
		}

		// Wait for next frame.
		let now = Instant::now();
		if now < next_frame {
			#[expect(
				clippy::disallowed_methods,
				reason = "the emulator thread runs outside any runtime, so it blocks on the host clock"
			)]
			std::thread::sleep(next_frame - now);
		}
		next_frame = next_frame.saturating_add(frame_duration);

		// Capture a single reference timestamp for this tick.
		// Used for both video and audio PTS so they stay aligned.
		let elapsed = start.elapsed();

		// Accumulate stats for this frame.
		let Wanted {
			video: is_video,
			audio: is_audio,
			..
		} = session.pause.get();
		game_stats.tick(is_video, is_audio);

		// Tick the emulator.
		emu.tick();

		// Publish status (only if changed).
		session.publish_status(&emu, viewers.len(), &game_stats, &mut status_publisher);

		// Encode and publish video frame.
		if is_video {
			let rgba = Bytes::from(emu.framebuffer());
			let ts =
				hang::container::Timestamp::from_micros(elapsed.as_micros() as u64).context("timestamp overflow")?;
			session.video_encoder.try_frame(rgba, ts);
		}

		// Encode and publish audio.
		if is_audio {
			// Re-anchor audio PTS when audio resumes after being inactive.
			if !was_audio_active {
				audio_encoder.reanchor();
			}
			let samples = emu.audio_samples();
			if !samples.is_empty()
				&& let Err(e) = audio_encoder.push_samples(&samples, elapsed)
			{
				tracing::warn!(error = %e, "audio encode error");
			}
		} else {
			// Drain audio buffer even when not encoding to prevent buildup.
			emu.audio_samples();
		}
		was_audio_active = is_audio;
	}
}

#[tokio::main]
async fn main() -> Result<()> {
	let config = Config::parse();
	config.log.init()?;
	config.check_deprecated()?;

	#[cfg(feature = "jemalloc")]
	let jemalloc = moq_tokio::jemalloc::run();
	#[cfg(not(feature = "jemalloc"))]
	let jemalloc = std::future::pending::<anyhow::Result<()>>();

	let result = tokio::select! {
		res = run(&config) => res,
		Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
		res = tokio::signal::ctrl_c() => res.context("failed to listen for ctrl-c"),
	};

	// run() owns a spawn_blocking emulator thread that loops forever. Returning from main
	// drops the tokio runtime, whose drop blocks indefinitely joining that thread, so the
	// process would hang instead of exiting (defeating systemd Restart=always when the
	// reconnect loop gives up). Exit explicitly so a real exit code reaches the supervisor.
	match result {
		Ok(()) => std::process::exit(0),
		Err(err) => {
			tracing::error!(err = %format!("{err:#}"), "exiting");
			std::process::exit(1);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A viewer subscribing while the emulator parks always wakes it: the demand
	/// changes under the lock the emulator waits on, so no notify is lost between
	/// its check and its wait.
	#[test]
	fn subscribe_always_wakes() {
		for _ in 0..2_000 {
			let pause = Arc::new(Pause::default());
			let waiter = std::thread::spawn({
				let pause = pause.clone();
				move || pause.wait()
			});
			pause.update(|d| d.video = true);
			waiter.join().unwrap();
		}
	}

	/// Paused only while neither track is wanted and some track's demand is open.
	#[test]
	fn paused_is_derived() {
		let mut d = Wanted::default();
		assert!(d.paused());
		d.audio = true;
		assert!(!d.paused());

		// Video's encoder exits while audio plays: audio leaving must still pause.
		d.video_closed = true;
		assert!(!d.paused());
		d.audio = false;
		assert!(d.paused());

		// Both closed: never block again.
		d.audio_closed = true;
		assert!(!d.paused());
	}

	/// `--connect` is the only way to reach the relay: it must parse without
	/// a connect flag of our own alongside it, and the URL must land where `run`
	/// reads it. A second required flag would leave `--connect` inert.
	///
	/// The argv is a copy of `demo/boy/justfile`, not a read of it, so this pins the
	/// binary's flag surface rather than the two staying in sync.
	/// `--connect` is the only spelling that reaches the dial, and the URL has to
	/// land where `run` reads it. A second required flag would leave it inert.
	#[test]
	fn matches_demo_invocation() {
		let config = Config::parse_from(&[
			std::ffi::OsStr::new("--connect"),
			std::ffi::OsStr::new("http://localhost:4443"),
			std::ffi::OsStr::new("--rom"),
			std::ffi::OsStr::new("rom/big2small.gb"),
			std::ffi::OsStr::new("--location"),
			std::ffi::OsStr::new("localhost"),
		])
		.expect("demo/boy/justfile invocation should parse");
		config.check_deprecated().expect("the demo uses current spellings");

		let connect = config.client.url.expect("the connect URL should reach the dial config");
		assert_eq!(connect.scheme(), "http");
		assert_eq!(connect.host_str(), Some("localhost"));
		assert_eq!(connect.port(), Some(4443));
	}

	/// An empty prefix is the root, so it adds no segment to the paths built under it.
	#[test]
	fn empty_prefixes_are_the_root() {
		let parse = |args: &[&str]| {
			let mut argv = vec!["--rom", "rom/big2small.gb"];
			argv.extend_from_slice(args);
			Config::parse_from(&argv.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()).unwrap()
		};
		let prefix = |config: Config| config.game_prefix().unwrap().to_string();

		assert_eq!(prefix(parse(&[])), "boy/game");
		assert_eq!(prefix(parse(&["--prefix", ""])), "game");
		assert_eq!(prefix(parse(&["--prefix-game", ""])), "");
		assert!(parse(&["--prefix", "/boy"]).game_prefix().is_err());
	}

	/// A game's endpoint id is its name, so a name is one segment. Viewers are clients,
	/// so even a name like theirs can't collide with one.
	#[test]
	fn names_are_game_endpoint_ids() {
		for name in ["tetris", "viewer-1"] {
			check_name(name).unwrap();
		}
		for name in ["", "a/b", "a\\b"] {
			check_name(name).expect_err(name);
		}
	}

	/// Sockets need moq-lite-07, so it is offered unless the command line picks versions.
	#[test]
	fn offers_lite_07_by_default() {
		let parse = |args: &[&str]| {
			let mut argv = vec!["--rom", "rom/big2small.gb"];
			argv.extend_from_slice(args);
			Config::parse_from(&argv.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()).unwrap()
		};
		let versions = |config: Config| {
			config
				.client()
				.version
				.iter()
				.map(|v| v.to_string())
				.collect::<Vec<_>>()
		};
		assert_eq!(versions(parse(&[])), ["moq-lite-07-wip"]);
		assert_eq!(
			versions(parse(&["--connect-version", "moq-transport-22"])),
			["moq-transport-22"]
		);
	}

	/// The spelling the demo used to pass. It still parses, so the process can name
	/// `--connect` rather than leave the parser reporting an unexpected argument, but it
	/// configures nothing and must stop the run.
	#[test]
	fn the_released_connect_spelling_is_refused() {
		let config = Config::parse_from(&[
			std::ffi::OsStr::new("--client-connect"),
			std::ffi::OsStr::new("http://localhost:4443"),
			std::ffi::OsStr::new("--rom"),
			std::ffi::OsStr::new("rom/big2small.gb"),
		])
		.expect("the released spelling still parses");
		assert_eq!(config.client.url, None);

		let err = config.check_deprecated().expect_err("must refuse").to_string();
		assert!(
			err.contains("--client-connect / MOQ_CLIENT_CONNECT -> --connect / MOQ_CONNECT"),
			"{err}"
		);
	}

	/// The removed viewer prefix still parses, so startup can say what replaced it.
	#[test]
	fn the_removed_viewer_prefix_is_refused() {
		let config = Config::parse_from(&[
			std::ffi::OsStr::new("--prefix-viewer"),
			std::ffi::OsStr::new("boy/viewer"),
			std::ffi::OsStr::new("--rom"),
			std::ffi::OsStr::new("rom/big2small.gb"),
		])
		.expect("the released spelling still parses");

		let err = config.check_deprecated().expect_err("must refuse").to_string();
		assert!(err.contains("--prefix-viewer / MOQ_PREFIX_VIEWER -> removed"), "{err}");
	}
}

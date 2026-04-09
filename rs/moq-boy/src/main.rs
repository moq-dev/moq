//! MoQ Boy: a crowd-controlled Game Boy Color emulator that streams over MoQ.
//!
//! Architecture:
//! - **Emulator thread** (blocking): runs the Game Boy at ~59.73fps, captures
//!   framebuffers and audio samples, publishes status JSON.
//! - **Video encoder thread**: receives RGBA frames, converts to H.264, publishes.
//! - **Audio encoder** (on emulator thread): resamples and encodes to Opus.
//! - **Monitor tasks** (async): watch video/audio track subscriptions to
//!   pause/resume the emulator when no viewers are watching.
//! - **Viewer handler** (async): discovers viewer broadcasts, relays button
//!   commands to the emulator.
//!
//! Pause/resume state machine:
//! ```text
//!   video_active ─┐
//!                  ├─ both false → paused (emulation stops, condvar blocks)
//!   audio_active ─┘
//!                    either true → resumed (condvar notified)
//!
//!   While paused:
//!     - After `timeout` seconds → auto-reset emulator
//!     - On resume → force video keyframe, re-anchor audio epoch
//! ```

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use url::Url;

mod audio;
mod emulator;
mod input;
mod stats;
mod status;
mod video;

#[derive(Parser, Clone)]
pub struct Config {
	/// Connect to the given relay URL.
	#[arg(long)]
	pub url: Url,

	/// Path to the Game Boy ROM file.
	#[arg(long)]
	pub rom: PathBuf,

	/// Session name (used in broadcast path). Defaults to ROM filename.
	#[arg(long)]
	pub name: Option<String>,

	/// Base path prefix. Used to derive --prefix-game and --prefix-viewer defaults.
	#[arg(long, default_value = "boy")]
	pub prefix: String,

	/// Path prefix for game broadcasts ("{prefix-game}/{name}"). Defaults to "{prefix}/game".
	#[arg(long)]
	pub prefix_game: Option<String>,

	/// Path prefix for viewer broadcasts ("{prefix-viewer}/{name}"). Defaults to "{prefix}/viewer".
	#[arg(long)]
	pub prefix_viewer: Option<String>,

	/// Inactivity timeout in seconds before auto-reset.
	#[arg(long, default_value_t = 300)]
	pub timeout: u64,

	/// Location label shown in viewer stats (e.g. "Dallas, TX").
	#[arg(long)]
	pub location: Option<String>,

	/// The MoQ client configuration.
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// The log configuration.
	#[command(flatten)]
	pub log: moq_native::Log,
}

/// Shared state for a game session, accessible from multiple threads/tasks.
///
/// Everything here is either atomic, behind a mutex, or immutable —
/// safe to share via `Arc<Session>` between the emulator thread,
/// track monitors, and async tasks.
struct Session {
	video_encoder: video::VideoEncoder,
	video_track: moq_lite::TrackProducer,
	audio_track: moq_lite::TrackProducer,

	/// Whether anyone is subscribed to the video/audio tracks.
	video_active: AtomicBool,
	audio_active: AtomicBool,

	/// True when no viewers are watching (both tracks unused).
	paused: AtomicBool,
	/// Condvar to wake the emulator thread on resume.
	resume: (Mutex<()>, Condvar),

	/// Auto-reset timeout when paused.
	timeout: Duration,
	/// Location label for status reporting.
	location: Option<String>,
}

impl Session {
	/// Monitor a single track's subscription state.
	/// Sets the flag when a viewer subscribes, clears it when all unsubscribe.
	async fn run_track_monitor(&self, name: &str, track: &moq_lite::TrackProducer, flag: &AtomicBool) {
		loop {
			if track.used().await.is_err() {
				break;
			}
			tracing::info!("resuming {name}: viewer subscribed");
			flag.store(true, Ordering::Release);
			self.paused.store(false, Ordering::Release);
			self.resume.1.notify_all();

			if track.unused().await.is_err() {
				break;
			}
			tracing::info!("pausing {name}: no viewers");
			flag.store(false, Ordering::Release);
		}
	}

	/// Monitor overall pause state.
	/// Pauses when BOTH tracks are unused, resumes when EITHER becomes used.
	async fn run_pause_monitor(&self) {
		loop {
			// Wait for BOTH tracks to become unused.
			let (v, a) = tokio::join!(self.video_track.unused(), self.audio_track.unused());
			if v.is_err() || a.is_err() {
				break;
			}
			tracing::info!("pausing emulation: no viewers");
			self.paused.store(true, Ordering::Release);

			// Wait for EITHER track to become used.
			tokio::select! {
				Err(_) = self.video_track.used() => break,
				Err(_) = self.audio_track.used() => break,
				else => {},
			}
			tracing::info!("resuming emulation: viewer connected");
			self.paused.store(false, Ordering::Release);
			self.resume.1.notify_all();
		}
		// Ensure emulator thread isn't stuck waiting on resume.
		self.paused.store(false, Ordering::Release);
		self.resume.1.notify_all();
	}

	/// Block the emulator thread until viewers connect.
	/// Auto-resets the emulator if paused longer than `timeout`.
	fn wait_for_resume(&self, emu: &mut emulator::Emulator, game_stats: &mut stats::Stats) -> Result<()> {
		tracing::info!("pausing encoding");
		let (lock, cvar) = &self.resume;
		let mut guard = lock.lock().unwrap();
		let pause_start = Instant::now();

		let mut reset_done = false;
		while self.paused.load(Ordering::Acquire) {
			if !reset_done && pause_start.elapsed() >= self.timeout {
				tracing::info!("resetting emulator (paused too long)");
				emu.reset()?;
				*game_stats = stats::Stats::new();
				reset_done = true;
			}

			if reset_done {
				guard = cvar.wait(guard).unwrap();
			} else {
				let remaining = self.timeout.saturating_sub(pause_start.elapsed());
				let (g, _) = cvar.wait_timeout(guard, remaining).unwrap();
				guard = g;
			}
		}

		tracing::info!("resuming encoding");
		Ok(())
	}

	/// Publish status if it changed since last frame.
	fn publish_status(
		&self,
		emu: &emulator::Emulator,
		viewer_latency: &HashMap<String, Vec<status::LatencyEntry>>,
		game_stats: &stats::Stats,
		publisher: &mut status::StatusPublisher,
	) {
		let held: Vec<_> = emu.pressed_buttons().iter().copied().collect();
		let latency_map: BTreeMap<String, Vec<status::LatencyEntry>> =
			viewer_latency.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

		let status = status::Status {
			buttons: held,
			latency: latency_map,
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

	let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<input::Command>(64);
	let client = config.client.clone().init()?;

	// Create the broadcast producer.
	let mut broadcast = moq_lite::BroadcastProducer::default();

	// Publish origin: the game session broadcast.
	let publish_origin = moq_lite::Origin::produce();
	let default_game_prefix = format!("{}/game", config.prefix);
	let default_viewer_prefix = format!("{}/viewer", config.prefix);
	let game_prefix = config.prefix_game.as_deref().unwrap_or(&default_game_prefix);
	let viewer_prefix = config.prefix_viewer.as_deref().unwrap_or(&default_viewer_prefix);

	let broadcast_path = format!("{game_prefix}/{name}");
	publish_origin.publish_broadcast(&broadcast_path, broadcast.consume());

	// Consume origin: viewer broadcasts under the viewer prefix.
	// JS publishes viewer feedback at "{viewer_prefix}/{name}/{viewerId}"
	let viewer_path = format!("{viewer_prefix}/{name}");
	let consume_origin = moq_lite::Origin::produce();
	let mut viewer_consumer = consume_origin
		.with_root(&viewer_path)
		.expect("viewer prefix should be valid")
		.consume();

	tracing::info!(url = %config.url, %name, broadcast = %broadcast_path, "connecting to relay");

	let reconnect = client
		.with_publish(publish_origin.consume())
		.with_consume(consume_origin)
		.reconnect(config.url.clone());

	// Set up catalog and encoders.
	let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
	let video_encoder = video::VideoEncoder::spawn(broadcast.clone(), catalog.clone());

	ffmpeg_next::init().context("failed to init ffmpeg")?;
	let audio_encoder = audio::AudioEncoder::new(broadcast.clone(), catalog.clone(), 44100)?;

	let video_track = video_encoder.track.clone();
	let audio_track = audio_encoder.track().clone();

	let status_publisher = status::StatusPublisher::new(&mut broadcast)?;

	let session = Arc::new(Session {
		video_encoder,
		video_track,
		audio_track,
		video_active: AtomicBool::new(false),
		audio_active: AtomicBool::new(false),
		paused: AtomicBool::new(true), // Start paused until first viewer.
		resume: (Mutex::new(()), Condvar::new()),
		timeout: Duration::from_secs(config.timeout),
		location: config.location.clone(),
	});

	// Monitor track subscriptions.
	let s = session.clone();
	tokio::spawn(async move { s.run_track_monitor("video", &s.video_track, &s.video_active).await });

	let s = session.clone();
	tokio::spawn(async move { s.run_track_monitor("audio", &s.audio_track, &s.audio_active).await });

	let s = session.clone();
	tokio::spawn(async move { s.run_pause_monitor().await });

	// Run the emulator on a blocking thread.
	let emulator_handle = tokio::task::spawn_blocking({
		let session = session.clone();
		move || run_emulator(session, &rom_path, audio_encoder, status_publisher, cmd_rx)
	});

	tokio::select! {
		res = emulator_handle => res?.context("emulator error"),
		res = reconnect.closed() => res,
		res = input::handle_viewers(&mut viewer_consumer, &cmd_tx) => res,
	}
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

	// Run a single tick so the encoders get initial data and publish
	// codec config, even before any viewer subscribes.
	emu.tick();
	let elapsed = start.elapsed();
	let rgba = Bytes::from(emu.framebuffer());
	let ts = hang::container::Timestamp::from_micros(elapsed.as_micros() as u64).context("timestamp overflow")?;
	session.video_encoder.try_frame(rgba, ts);
	let samples = emu.audio_samples();
	if !samples.is_empty() {
		audio_encoder.push_samples(&samples, elapsed)?;
	}

	// Game Boy runs at exactly 59.727 Hz (4194304 Hz CPU / 70224 cycles per frame).
	// 1/59.727 ≈ 16742 microseconds per frame.
	let frame_duration = Duration::from_micros(16_742);
	let mut next_frame = Instant::now();
	let mut viewer_latency: HashMap<String, Vec<status::LatencyEntry>> = HashMap::new();
	let mut game_stats = stats::Stats::new();
	let mut was_audio_active = false;

	loop {
		// Block when no viewers are watching. See state diagram in module docs.
		if session.paused.load(Ordering::Acquire) {
			session.wait_for_resume(&mut emu, &mut game_stats)?;

			// Don't try to catch up after a pause.
			next_frame = Instant::now();
			// Reset tick timer so pause duration isn't counted.
			game_stats.reset_tick();
			// Force a keyframe so new viewers can start decoding.
			session.video_encoder.force_keyframe();
			// Re-anchor audio timestamps so the pause gap appears in PTS.
			audio_encoder.reset_epoch();
		}

		// Drain pending viewer commands before sleeping, so input that
		// arrived during the previous frame's work is applied immediately.
		{
			let elapsed = start.elapsed();
			let encode_ms = u32::try_from(session.video_encoder.encode_duration().as_millis()).unwrap_or(u32::MAX);

			while let Ok(cmd) = cmd_rx.try_recv() {
				match cmd {
					input::Command::Buttons {
						buttons,
						viewer_id,
						timestamps,
					} => {
						emu.set_buttons(&viewer_id, buttons.into_iter().collect());

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

						viewer_latency.insert(viewer_id, breakdown);
					}
					input::Command::ViewerLeft { viewer_id } => {
						emu.viewer_left(&viewer_id);
						viewer_latency.remove(&viewer_id);
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
			std::thread::sleep(next_frame - now);
		}
		next_frame += frame_duration;

		// Capture a single reference timestamp for this tick.
		// Used for both video and audio PTS so they stay aligned.
		let elapsed = start.elapsed();

		// Accumulate stats for this frame.
		let is_video = session.video_active.load(Ordering::Relaxed);
		let is_audio = session.audio_active.load(Ordering::Relaxed);
		game_stats.tick(is_video, is_audio);

		// Tick the emulator.
		emu.tick();

		// Publish status (only if changed).
		session.publish_status(&emu, &viewer_latency, &game_stats, &mut status_publisher);

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
				audio_encoder.reset_epoch();
			}
			let samples = emu.audio_samples();
			if !samples.is_empty() {
				if let Err(e) = audio_encoder.push_samples(&samples, elapsed) {
					tracing::warn!(error = %e, "audio encode error");
				}
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
	config.log.init();

	tokio::select! {
		res = run(&config) => res,
		_ = tokio::signal::ctrl_c() => std::process::exit(0),
	}
}

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use url::Url;

mod audio;
mod emulator;
mod input;
mod video;

/// Cumulative stats since last emulator reset.
struct Stats {
	start: Instant,
	emulation: Duration,
	video: Duration,
	audio: Duration,
	last_tick: Instant,
}

impl Stats {
	fn new() -> Self {
		let now = Instant::now();
		Self {
			start: now,
			emulation: Duration::ZERO,
			video: Duration::ZERO,
			audio: Duration::ZERO,
			last_tick: now,
		}
	}

	/// Accumulate one frame's worth of time.
	fn tick(&mut self, video_active: bool, audio_active: bool) {
		let now = Instant::now();
		let elapsed = now - self.last_tick;
		self.last_tick = now;

		self.emulation += elapsed;
		if video_active {
			self.video += elapsed;
		}
		if audio_active {
			self.audio += elapsed;
		}
	}

	fn report(&self) -> StatsReport {
		let to_secs = |d: Duration| d.as_secs_f64().round() as u64;
		StatsReport {
			video_secs: to_secs(self.video),
			audio_secs: to_secs(self.audio),
			emulation_secs: to_secs(self.emulation),
			wall_secs: to_secs(self.start.elapsed()),
		}
	}
}

#[derive(Serialize, PartialEq, Eq)]
struct StatsReport {
	video_secs: u64,
	audio_secs: u64,
	emulation_secs: u64,
	wall_secs: u64,
}

#[derive(Serialize)]
struct Status {
	buttons: Vec<emulator::Button>,
	latency: BTreeMap<String, u32>,
	stats: StatsReport,
	#[serde(skip_serializing_if = "Option::is_none")]
	location: Option<String>,
}

#[derive(Parser, Clone)]
pub struct Config {
	/// Connect to the given relay URL.
	#[arg(long)]
	pub url: Url,

	/// Path to the Game Boy ROM file.
	#[arg(long)]
	pub rom: PathBuf,

	/// Session name (used in broadcast path: boy/{name}). Defaults to ROM filename.
	#[arg(long)]
	pub name: Option<String>,

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

	let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<input::Command>(64);
	let client = config.client.clone().init()?;

	// Create the broadcast producer.
	let mut broadcast = moq_lite::BroadcastProducer::default();

	// Publish origin: the GB session broadcast.
	let publish_origin = moq_lite::Origin::produce();
	let broadcast_path = format!("boy/{}", name);
	publish_origin.publish_broadcast(&broadcast_path, broadcast.consume());

	// Consume origin: viewer broadcasts under boy/{name}/viewer/.
	let viewer_prefix = format!("boy/{}/viewer", name);
	let consume_origin = moq_lite::Origin::produce();
	let mut viewer_consumer = consume_origin
		.with_root(&viewer_prefix)
		.expect("viewer prefix should be valid")
		.consume();

	tracing::info!(url = %config.url, name = %name, "connecting to relay");

	let session = client
		.with_publish(publish_origin.consume())
		.with_consume(consume_origin)
		.connect(config.url.clone())
		.await?;

	// Set up catalog and encoders.
	let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
	let video_encoder = video::VideoEncoder::spawn(broadcast.clone(), catalog.clone());

	// Init ffmpeg and create audio encoder before the blocking thread
	// so we can clone its track producer for monitoring.
	ffmpeg_next::init().context("failed to init ffmpeg")?;
	let mut audio_encoder = audio::AudioEncoder::new(broadcast.clone(), catalog.clone(), 44100)?;

	// Clone track producers for the monitoring task.
	let video_track = video_encoder.track.clone();
	let audio_track = audio_encoder.track().clone();

	// Create status track.
	let status_track = moq_lite::Track {
		name: "status".to_string(),
		priority: 10,
	};
	let mut status_producer = broadcast.create_track(status_track)?;

	// Per-track and overall pause signaling.
	let video_active = Arc::new(AtomicBool::new(false));
	let audio_active = Arc::new(AtomicBool::new(false));
	let paused = Arc::new(AtomicBool::new(true)); // Start paused until first viewer.
	let resume_notify = Arc::new((Mutex::new(()), Condvar::new()));

	// Monitor video track.
	let flag = video_active.clone();
	let all_paused = paused.clone();
	let resume = resume_notify.clone();
	let vt = video_track.clone();
	tokio::spawn(async move {
		loop {
			if vt.used().await.is_err() {
				break;
			}
			tracing::info!("resuming video: viewer subscribed");
			flag.store(true, Ordering::Release);
			all_paused.store(false, Ordering::Release);
			resume.1.notify_all();

			if vt.unused().await.is_err() {
				break;
			}
			tracing::info!("pausing video: no viewers");
			flag.store(false, Ordering::Release);
		}
	});

	// Monitor audio track.
	let flag = audio_active.clone();
	let all_paused = paused.clone();
	let resume = resume_notify.clone();
	let at = audio_track.clone();
	tokio::spawn(async move {
		loop {
			if at.used().await.is_err() {
				break;
			}
			tracing::info!("resuming audio: viewer subscribed");
			flag.store(true, Ordering::Release);
			all_paused.store(false, Ordering::Release);
			resume.1.notify_all();

			if at.unused().await.is_err() {
				break;
			}
			tracing::info!("pausing audio: no viewers");
			flag.store(false, Ordering::Release);
		}
	});

	// Monitor overall pause state (both unused = pause emulation).
	{
		let paused = paused.clone();
		let resume = resume_notify.clone();
		tokio::spawn(async move {
			loop {
				// Wait for BOTH tracks to become unused.
				let (v, a) = tokio::join!(video_track.unused(), audio_track.unused());
				if v.is_err() || a.is_err() {
					break;
				}
				tracing::info!("pausing emulation: no viewers");
				paused.store(true, Ordering::Release);

				// Wait for EITHER track to become used.
				tokio::select! {
					Err(_) = video_track.used() => break,
					Err(_) = audio_track.used() => break,
					else => {},
				}
				tracing::info!("resuming emulation: viewer connected");
				paused.store(false, Ordering::Release);
				resume.1.notify_all();
			}
			// Ensure emulator thread isn't stuck waiting on resume.
			paused.store(false, Ordering::Release);
			resume.1.notify_all();
		});
	}

	// Run the emulator on a blocking thread.
	let timeout_secs = config.timeout;
	let location = config.location.clone();
	let emulator_handle = tokio::task::spawn_blocking(move || -> Result<()> {
		let mut emu = emulator::Emulator::new(&rom_path)?;
		let start = std::time::Instant::now();

		// Bootstrap: tick once and encode a frame so the video codec config
		// is inserted into the catalog before any viewer connects.
		{
			emu.tick();
			let rgba = Bytes::from(emu.framebuffer());
			let pts_micros = start.elapsed().as_micros() as u64;
			let ts = hang::container::Timestamp::from_micros(pts_micros).context("timestamp overflow")?;
			video_encoder.try_frame(rgba, ts);
			// Give the encoder thread time to process.
			std::thread::sleep(std::time::Duration::from_millis(100));
		}

		let frame_duration = Duration::from_micros(16_742); // ~59.73fps
		let mut next_frame = Instant::now();
		let mut last_status = String::new();
		let timeout = Duration::from_secs(timeout_secs);
		let mut viewer_latency: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
		let mut stats = Stats::new();
		let mut was_audio_active = false;

		loop {
			// Pause emulation when no viewers are watching.
			if paused.load(Ordering::Acquire) {
				tracing::info!("pausing encoding");
				let (lock, cvar) = &*resume_notify;
				let mut guard = lock.lock().unwrap();
				let pause_start = std::time::Instant::now();

				let mut reset_done = false;
				while paused.load(Ordering::Acquire) {
					if !reset_done && pause_start.elapsed() >= timeout {
						tracing::info!("resetting emulator (paused too long)");
						emu.reset()?;
						stats = Stats::new();
						reset_done = true;
					}

					if reset_done {
						guard = cvar.wait(guard).unwrap();
					} else {
						let remaining = timeout - pause_start.elapsed();
						let (g, _) = cvar.wait_timeout(guard, remaining).unwrap();
						guard = g;
					}
				}

				tracing::info!("resuming encoding");
				// Don't try to catch up after a pause.
				next_frame = Instant::now();
				// Reset tick timer so pause duration isn't counted.
				stats.last_tick = Instant::now();
				// Force a keyframe so new viewers can start decoding.
				video_encoder.force_keyframe();
				// Re-anchor audio timestamps so the pause gap appears in PTS.
				audio_encoder.reset_epoch();
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
			let current_ts_ms = elapsed.as_secs_f64() * 1000.0;

			// Accumulate stats for this frame.
			let is_video = video_active.load(Ordering::Relaxed);
			let is_audio = audio_active.load(Ordering::Relaxed);
			stats.tick(is_video, is_audio);

			// Drain pending commands.
			while let Ok(cmd) = cmd_rx.try_recv() {
				match cmd {
					input::Command::Buttons {
						buttons,
						viewer_id,
						ts_ms,
					} => {
						emu.set_buttons(&viewer_id, buttons.into_iter().collect());

						let latency = current_ts_ms - ts_ms;
						if latency >= 0.0 {
							viewer_latency.insert(viewer_id, latency);
						}
					}
					input::Command::ViewerLeft { viewer_id } => {
						emu.viewer_left(&viewer_id);
						viewer_latency.remove(&viewer_id);
					}
					input::Command::Reset => {
						tracing::info!("resetting emulator (viewer request)");
						emu.reset()?;
						stats = Stats::new();
					}
				}
			}

			// Tick the emulator.
			emu.tick();

			// Publish status.
			let held: Vec<_> = emu.pressed_buttons().iter().copied().collect();

			let latency_map: BTreeMap<String, u32> =
				viewer_latency.iter().map(|(k, ms)| (k.clone(), *ms as u32)).collect();

			let status = Status {
				buttons: held,
				latency: latency_map,
				stats: stats.report(),
				location: location.clone(),
			};

			let new_status_str = serde_json::to_string(&status).unwrap();

			if new_status_str != last_status {
				last_status = new_status_str.clone();
				if let Ok(mut group) = status_producer.append_group() {
					let _ = group.write_frame(new_status_str.into_bytes());
					let _ = group.finish();
				}
			}

			// Grab and publish video frame (skip if no video viewers).
			if is_video {
				let rgba = Bytes::from(emu.framebuffer());
				let ts = hang::container::Timestamp::from_micros(elapsed.as_micros() as u64)
					.context("timestamp overflow")?;
				video_encoder.try_frame(rgba, ts);
			}

			// Grab and encode audio (skip if no audio viewers).
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
	});

	tokio::select! {
		res = emulator_handle => res?.context("emulator error"),
		res = session.closed() => res.map_err(Into::into),
		res = input::handle_viewers(&mut viewer_consumer, &cmd_tx) => res,
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

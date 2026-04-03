use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use url::Url;

mod audio;
mod emulator;
mod input;
mod video;

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
	let emulator_handle = tokio::task::spawn_blocking(move || -> Result<()> {
		let mut emu = emulator::Emulator::new(&rom_path)?;

		let frame_duration = std::time::Duration::from_micros(16_742); // ~59.73fps
		let mut next_frame = std::time::Instant::now();
		let start = std::time::Instant::now();
		let mut last_input = std::time::Instant::now();
		let mut last_status = String::new();
		let timeout = std::time::Duration::from_secs(timeout_secs);
		let mut viewer_latency: std::collections::HashMap<String, (f64, std::time::Instant)> =
			std::collections::HashMap::new();

		loop {
			// Pause emulation when no viewers are watching.
			if paused.load(Ordering::Acquire) {
				tracing::info!("pausing encoding");
				let (lock, cvar) = &*resume_notify;
				let mut guard = lock.lock().unwrap();
				while paused.load(Ordering::Acquire) {
					guard = cvar.wait(guard).unwrap();
				}
				tracing::info!("resuming encoding");
				// Don't try to catch up after a pause.
				next_frame = std::time::Instant::now();
				// Force a keyframe so new viewers can start decoding.
				video_encoder.force_keyframe();
			}

			// Wait for next frame.
			let now = std::time::Instant::now();
			if now < next_frame {
				std::thread::sleep(next_frame - now);
			}
			next_frame += frame_duration;

			// Current media timestamp in milliseconds.
			let current_ts_ms = start.elapsed().as_secs_f64() * 1000.0;

			// Drain pending commands.
			while let Ok(cmd) = cmd_rx.try_recv() {
				match cmd {
					input::Command::Buttons {
						buttons,
						viewer_id,
						ts_ms,
					} => {
						emu.set_buttons(&viewer_id, buttons.into_iter().collect());
						last_input = std::time::Instant::now();

						let latency = current_ts_ms - ts_ms;
						if latency >= 0.0 {
							viewer_latency.insert(viewer_id, (latency, std::time::Instant::now()));
						}
					}
					input::Command::ViewerLeft { viewer_id } => {
						emu.viewer_left(&viewer_id);
						viewer_latency.remove(&viewer_id);
					}
					input::Command::Reset => {
						tracing::info!("resetting emulator (viewer request)");
						emu.reset()?;
						last_input = std::time::Instant::now();
					}
				}
			}

			// Check inactivity timeout.
			let idle_time = std::time::Instant::now() - last_input;
			if idle_time > timeout {
				tracing::info!("resetting emulator (inactivity timeout)");
				emu.reset()?;
				last_input = std::time::Instant::now();
			}

			// Tick the emulator.
			emu.tick();

			// Expire stale viewer latency entries (no input for 30s).
			let stale = std::time::Duration::from_secs(30);
			viewer_latency.retain(|_, (_, last_seen)| last_seen.elapsed() < stale);

			// Publish status.
			let held: Vec<_> = emu.pressed_buttons().iter().copied().collect();
			let idle_secs = idle_time.as_secs();
			let remaining = timeout_secs.saturating_sub(idle_secs);

			let latency_map: serde_json::Map<String, serde_json::Value> = viewer_latency
				.iter()
				.map(|(k, (ms, _))| (k.clone(), serde_json::json!((*ms as u32))))
				.collect();

			let new_status = serde_json::json!({
				"buttons": held,
				"reset_in": remaining,
				"latency": latency_map,
			});
			let new_status_str = new_status.to_string();

			if new_status_str != last_status {
				last_status = new_status_str.clone();
				if let Ok(mut group) = status_producer.append_group() {
					let _ = group.write_frame(new_status_str.into_bytes());
					let _ = group.finish();
				}
			}

			// Grab and publish video frame (skip if no video viewers).
			if video_active.load(Ordering::Relaxed) {
				let rgba = Bytes::from(emu.framebuffer());
				let pts_micros = start.elapsed().as_micros() as u64;
				let ts = hang::container::Timestamp::from_micros(pts_micros).context("timestamp overflow")?;
				video_encoder.try_frame(rgba, ts);
			}

			// Grab and encode audio (skip if no audio viewers).
			if audio_active.load(Ordering::Relaxed) {
				let samples = emu.audio_samples();
				if !samples.is_empty() {
					if let Err(e) = audio_encoder.push_samples(&samples) {
						tracing::warn!(error = %e, "audio encode error");
					}
				}
			} else {
				// Drain audio buffer even when not encoding to prevent buildup.
				emu.audio_samples();
			}
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

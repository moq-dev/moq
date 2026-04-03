use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use std::path::PathBuf;
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

    // Set up catalog and video encoder.
    let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
    let video_encoder = video::VideoEncoder::spawn(broadcast.clone(), catalog.clone());

    // Create status track.
    let status_track = moq_lite::Track {
        name: "status".to_string(),
        priority: 10,
    };
    let mut status_producer = broadcast.create_track(status_track)?;

    // Run the emulator on a blocking thread.
    let timeout_secs = config.timeout;
    let emulator_handle = tokio::task::spawn_blocking(move || -> Result<()> {
        ffmpeg_next::init().context("failed to init ffmpeg")?;

        let mut emu = emulator::Emulator::new(&rom_path)?;

        // Set up audio encoder (runs on this thread since Opus encoding is fast).
        // GB APU typically outputs at ~44100Hz but we'll check.
        let mut audio_encoder =
            audio::AudioEncoder::new(broadcast.clone(), catalog.clone(), 44100)?;

        let frame_duration = std::time::Duration::from_micros(16_742); // ~59.73fps
        let mut next_frame = std::time::Instant::now();
        let start = std::time::Instant::now();
        let mut last_input = std::time::Instant::now();
        let mut last_status = String::new();
        let timeout = std::time::Duration::from_secs(timeout_secs);
        // Per-viewer latency: viewer_id -> (latency_ms, last_seen).
        let mut viewer_latency: std::collections::HashMap<String, (f64, std::time::Instant)> =
            std::collections::HashMap::new();

        loop {
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

                        // Calculate end-to-end latency: current time - viewer's displayed time.
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

            // Publish status with held buttons, idle countdown, and per-viewer latency.
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

            // Grab and publish video frame.
            let rgba = Bytes::from(emu.framebuffer());
            let pts_micros = start.elapsed().as_micros() as u64;
            let ts = hang::container::Timestamp::from_micros(pts_micros)
                .context("timestamp overflow")?;

            // Send to video encoder thread (non-blocking, drop frame if behind).
            video_encoder.try_frame(rgba, ts);

            // Grab and encode audio.
            let samples = emu.audio_samples();
            if !samples.is_empty() {
                if let Err(e) = audio_encoder.push_samples(&samples) {
                    tracing::warn!(error = %e, "audio encode error");
                }
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

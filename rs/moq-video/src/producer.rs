//! Encode decoded video frames and publish them as an H.264 moq track.
//!
//! Encoding is strictly on demand: the avc3 track and catalog entry are
//! advertised immediately, but the camera stays closed (LED off, no CPU)
//! until a subscriber appears. When the last viewer leaves, the camera is
//! released again. This mirrors `moq-boy`, which pauses its emulator on
//! `TrackProducer::used()` / `unused()`.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use moq_mux::container::Timestamp;

use crate::Error;
use crate::capture::{Camera, CameraConfig};
use crate::encoder::{Encoder, EncoderConfig, EncoderKind};

/// Default capture/encode framerate when the camera config doesn't pin one.
const DEFAULT_FRAMERATE: u32 = 30;

/// Publishes encoded H.264 frames as an avc3 moq track.
///
/// Built on the async side so the track is advertised (and the catalog
/// registered) before the camera opens; this is what lets a subscriber
/// trigger capture on demand. `moq_mux::codec::h264::Import` handles
/// catalog registration and framing.
pub struct VideoProducer {
	import: moq_mux::codec::h264::Import,
}

impl VideoProducer {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self, Error> {
		let import =
			moq_mux::codec::h264::Import::new(broadcast, catalog).with_mode(moq_mux::codec::h264::Mode::Avc3)?;
		Ok(Self { import })
	}

	/// The underlying track producer, eagerly created by avc3 mode. Clone it
	/// to watch subscription state via [`used`](moq_net::TrackProducer::used) /
	/// [`unused`](moq_net::TrackProducer::unused).
	pub fn track(&self) -> Option<&moq_net::TrackProducer> {
		self.import.track()
	}

	/// Publish already-encoded Annex-B packets at the given timestamp.
	pub fn publish(&mut self, packets: Vec<bytes::Bytes>, timestamp: Timestamp) -> Result<(), Error> {
		for mut packet in packets {
			self.import.decode_frame(&mut packet, Some(timestamp))?;
		}
		Ok(())
	}

	/// Finalize the track.
	pub fn finish(&mut self) -> Result<(), Error> {
		self.import.finish()?;
		Ok(())
	}
}

/// High-level webcam publish configuration.
#[derive(Clone, Debug, Default)]
pub struct CameraPublishConfig {
	pub camera: CameraConfig,
	/// Target bitrate in bits per second; `None` derives from resolution.
	pub bitrate: Option<u64>,
	/// Encoder implementation preference.
	pub kind: EncoderKind,
}

/// Capture a webcam and publish it as on-demand H.264.
///
/// Returns when the broadcast is dropped (the track stops being announced)
/// or the capture loop fails. The camera is opened only while at least one
/// subscriber is watching; presentation timestamps track real elapsed time,
/// so the timeline stays continuous across idle gaps.
pub async fn publish_camera(
	broadcast: moq_net::BroadcastProducer,
	catalog: moq_mux::catalog::Producer,
	config: CameraPublishConfig,
) -> Result<(), Error> {
	let framerate = config.camera.framerate.unwrap_or(DEFAULT_FRAMERATE);

	let producer = VideoProducer::new(broadcast, catalog)?;
	let track = producer
		.track()
		.cloned()
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("avc3 track was not created")))?;

	let gate = Gate::new();

	// ffmpeg capture + encode is blocking; keep it off the async runtime.
	let worker_gate = gate.clone();
	let mut worker = tokio::task::spawn_blocking(move || capture_loop(producer, config, framerate, worker_gate));

	tokio::select! {
		// Surface a capture/encode failure (e.g. camera open) promptly.
		res = &mut worker => res.map_err(|e| Error::Codec(anyhow::anyhow!("capture task: {e}")))?,
		// The broadcast was dropped: stop the worker and wait for it to flush.
		() = monitor_demand(&track, &gate) => {
			gate.close();
			worker
				.await
				.map_err(|e| Error::Codec(anyhow::anyhow!("capture task: {e}")))?
		}
	}
}

/// Toggle the gate as viewers subscribe and unsubscribe. Returns once the
/// track stops being announced (broadcast dropped / aborted).
async fn monitor_demand(track: &moq_net::TrackProducer, gate: &Gate) {
	loop {
		match track.used().await {
			Ok(()) => gate.set_active(true),
			Err(_) => return,
		}
		match track.unused().await {
			Ok(()) => gate.set_active(false),
			Err(_) => return,
		}
	}
}

/// Blocking capture/encode loop. Opens the camera lazily on the first
/// watched frame and releases it whenever the gate goes idle.
fn capture_loop(
	mut producer: VideoProducer,
	config: CameraPublishConfig,
	framerate: u32,
	gate: Arc<Gate>,
) -> Result<(), Error> {
	let mut camera: Option<Camera> = None;
	let mut encoder: Option<Encoder> = None;
	let mut start: Option<Instant> = None;
	let mut last_ts = Timestamp::from_micros(0)?;

	loop {
		if !gate.is_active() {
			// No viewers: drop the camera so its LED turns off and it stops
			// consuming CPU, then block until someone subscribes.
			if camera.take().is_some() {
				encoder = None;
				tracing::info!("no viewers: released camera");
			}
			if !gate.wait_active() {
				break; // closed
			}
			continue;
		}

		// Open the camera (and an encoder sized to its negotiated mode) the
		// first time we're watched after being idle.
		if camera.is_none() {
			let cam = Camera::open(&config.camera)?;
			let mut encoder_config = EncoderConfig::new(cam.width(), cam.height(), framerate);
			encoder_config.bitrate = config.bitrate;
			encoder_config.kind = config.kind.clone();
			let enc = Encoder::new(&encoder_config)?;
			tracing::info!(
				encoder = enc.name(),
				device = cam.device(),
				"viewer subscribed: capturing"
			);
			camera = Some(cam);
			encoder = Some(enc);
		}

		let frame = match camera.as_mut().expect("camera open above").read()? {
			Some(frame) => frame,
			None => break, // device stopped producing frames
		};

		let now = Instant::now();
		let elapsed = now.duration_since(*start.get_or_insert(now));
		let ts = Timestamp::from_micros(elapsed.as_micros() as u64)?;
		last_ts = ts;

		let packets = encoder.as_mut().expect("encoder built above").encode(&frame)?;
		producer.publish(packets, ts)?;
	}

	// Flush whatever the encoder still holds, then close the track.
	if let Some(enc) = encoder.as_mut()
		&& let Ok(packets) = enc.finish()
	{
		let _ = producer.publish(packets, last_ts);
	}
	producer.finish()?;
	Ok(())
}

/// Bridges the async demand monitor to the blocking capture thread: the
/// monitor flips `active`, the capture loop waits on it.
struct Gate {
	state: Mutex<GateState>,
	cond: Condvar,
}

#[derive(Default)]
struct GateState {
	active: bool,
	closed: bool,
}

impl Gate {
	fn new() -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(GateState::default()),
			cond: Condvar::new(),
		})
	}

	fn set_active(&self, active: bool) {
		let mut state = self.state.lock().unwrap();
		state.active = active;
		self.cond.notify_all();
	}

	fn close(&self) {
		let mut state = self.state.lock().unwrap();
		state.closed = true;
		self.cond.notify_all();
	}

	fn is_active(&self) -> bool {
		self.state.lock().unwrap().active
	}

	/// Block until active or closed. Returns `false` if closed.
	fn wait_active(&self) -> bool {
		let mut state = self.state.lock().unwrap();
		while !state.active && !state.closed {
			state = self.cond.wait(state).unwrap();
		}
		!state.closed
	}
}

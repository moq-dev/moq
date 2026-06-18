//! Encode decoded video frames and publish them as an H.264 moq track.
//!
//! Encoding is strictly on demand: the avc3 track and catalog entry are
//! advertised immediately, but the camera stays closed (LED off, no CPU)
//! until a subscriber appears. When the last viewer leaves, the camera is
//! released again. This mirrors `moq-boy`, which pauses its emulator on
//! `TrackProducer::used()` / `unused()`.

use std::sync::{Arc, Condvar, Mutex};

use moq_net::Timestamp;

use crate::Error;
use crate::capture::{self, FrameSource};

use super::encoder::{self, Encoder};

/// Last-resort framerate when neither the caller nor the camera reports one.
const DEFAULT_FRAMERATE: u32 = 30;

/// Publishes encoded H.264 frames as an avc3 moq track.
///
/// Built on the async side so the track is advertised (and the catalog
/// registered) before the camera opens; this is what lets a subscriber
/// trigger capture on demand. `moq_mux::codec::h264::Import` handles
/// catalog registration and framing.
pub struct Producer {
	split: moq_mux::codec::h264::Split,
	import: moq_mux::codec::h264::Import,
}

impl Producer {
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self, Error> {
		let track = moq_mux::import::unique_track(&mut broadcast, ".avc3")?;
		let import = moq_mux::codec::h264::Import::new(track, catalog);
		let split = moq_mux::codec::h264::Split::new();
		Ok(Self { split, import })
	}

	/// A watch-only handle to the track's subscriber demand, created eagerly so
	/// subscription state is observable before any frames arrive. Watch it via
	/// [`used`](moq_net::TrackDemand::used) / [`unused`](moq_net::TrackDemand::unused).
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.import.demand()
	}

	/// Publish already-encoded Annex-B packets at the given timestamp.
	pub fn publish(&mut self, packets: Vec<bytes::Bytes>, timestamp: Timestamp) -> Result<(), Error> {
		for packet in packets {
			// The encoder emits one whole access unit per packet, so flush to emit it.
			let mut frames = self.split.decode(&packet, Some(timestamp))?;
			frames.extend(self.split.flush(Some(timestamp))?);
			self.import.decode(frames)?;
		}
		Ok(())
	}

	/// Finalize the track.
	pub fn finish(&mut self) -> Result<(), Error> {
		self.import.finish()?;
		Ok(())
	}
}

/// Source-agnostic encode knobs for [`publish_capture`], where the geometry
/// (width / height / framerate) comes from the capture source, not the caller.
/// For the bring-your-own-frames [`Encoder`](super::Encoder) path, where you
/// must specify geometry, use [`Config`](super::Config) instead.
///
/// `#[non_exhaustive]`: construct via [`Options::default`] and set fields, so
/// new knobs can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
	/// Target bitrate in bits per second; `None` derives from resolution.
	pub bitrate: Option<u64>,
	/// Encoder implementation preference.
	pub kind: encoder::Kind,
}

/// Capture a webcam and publish it as on-demand H.264.
///
/// Returns when the broadcast is dropped (the track stops being announced)
/// or the capture loop fails. The camera is opened only while at least one
/// subscriber is watching; frames are stamped from `clock`, so passing the
/// same [`Clock`](moq_mux::Clock) to a concurrent audio publish keeps the two
/// tracks aligned.
pub async fn publish_capture(
	broadcast: moq_net::BroadcastProducer,
	catalog: moq_mux::catalog::Producer,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
	// A caller asking for exactly zero is an error; omitting it (None) is
	// fine and resolves to the camera's reported rate once it's open.
	if capture.framerate == Some(0) {
		return Err(Error::InvalidFramerate(0));
	}

	let producer = Producer::new(broadcast, catalog)?;
	let demand = producer.demand();

	let gate = Gate::new();

	// Camera capture + encode is blocking; keep it off the async runtime.
	let worker_gate = gate.clone();
	let mut worker = tokio::task::spawn_blocking(move || capture_loop(producer, capture, encode, worker_gate, clock));

	tokio::select! {
		// Surface a capture/encode failure (e.g. camera open) promptly.
		res = &mut worker => res.map_err(|e| Error::Codec(anyhow::anyhow!("capture task: {e}")))?,
		// The broadcast was dropped: stop the worker and wait for it to flush.
		() = monitor_demand(&demand, &gate) => {
			gate.close();
			worker
				.await
				.map_err(|e| Error::Codec(anyhow::anyhow!("capture task: {e}")))?
		}
	}
}

/// Toggle the gate as viewers subscribe and unsubscribe. Returns once the
/// track stops being announced (broadcast dropped / aborted).
async fn monitor_demand(demand: &moq_net::TrackDemand, gate: &Gate) {
	loop {
		match demand.used().await {
			Ok(()) => gate.set_active(true),
			Err(err) => return log_track_ended(err),
		}
		match demand.unused().await {
			Ok(()) => gate.set_active(false),
			Err(err) => return log_track_ended(err),
		}
	}
}

/// A dropped or closed track is the normal end of a publish; any other cause is
/// a real abort (e.g. a transport reset) worth surfacing rather than treating as
/// a clean exit.
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("video track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "video track aborted; stopping capture");
	}
}

/// Blocking capture/encode loop. Captures one frame up front to populate the
/// catalog (the codec/resolution only exist once the encoder has produced an
/// SPS), then releases the camera whenever the gate goes idle.
fn capture_loop(
	mut producer: Producer,
	capture: capture::Config,
	encode: Options,
	gate: Arc<Gate>,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
	let mut camera: Option<Box<dyn FrameSource>> = None;
	let mut encoder: Option<Encoder> = None;
	// Force an IDR on the first frame of each (re)open so a viewer subscribing
	// after an idle gap can start decoding immediately, rather than waiting for
	// the next GOP boundary.
	let mut force_keyframe = false;
	let mut last_ts = Timestamp::from_micros(0)?;
	// The catalog video rendition only appears once a frame has been encoded
	// (the importer reads the SPS). Until then we keep capturing regardless of
	// the gate, so a catalog-driven subscriber can discover the track and
	// trigger `used()`. After that we release the camera while unwatched.
	let mut catalog_ready = false;

	loop {
		if catalog_ready && !gate.is_active() {
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
			let cam = capture::open(&capture)?;
			// Prefer an explicit --fps, otherwise use the camera's reported
			// rate, falling back only if the backend doesn't expose one.
			let framerate = capture
				.framerate
				.or_else(|| cam.framerate())
				.unwrap_or(DEFAULT_FRAMERATE);
			let mut encoder_config = encoder::Config::new(cam.width(), cam.height(), framerate);
			encoder_config.bitrate = encode.bitrate;
			encoder_config.kind = encode.kind.clone();
			let enc = Encoder::new(&encoder_config)?;
			tracing::info!(
				encoder = enc.name(),
				device = cam.device(),
				"viewer subscribed: capturing"
			);
			camera = Some(cam);
			encoder = Some(enc);
			force_keyframe = true;
		}

		let frame = match camera.as_mut().expect("camera open above").read()? {
			Some(frame) => frame,
			None => break, // device stopped producing frames
		};

		let ts = Timestamp::from_micros(clock.micros())?;
		last_ts = ts;

		let packets = encoder
			.as_mut()
			.expect("encoder built above")
			.encode(&frame, force_keyframe)?;
		force_keyframe = false;
		// Once the encoder has emitted a frame, the importer has parsed the SPS
		// and the catalog rendition exists, so the gate can take over.
		catalog_ready |= !packets.is_empty();
		producer.publish(packets, ts)?;
	}

	// Flush whatever the encoder still holds, then close the track. Log
	// (don't discard) flush/publish errors at shutdown; they're not worth
	// aborting the close over, but silently dropping them hides real failures.
	if let Some(enc) = encoder.as_mut() {
		match enc.finish() {
			Ok(packets) => {
				if let Err(err) = producer.publish(packets, last_ts) {
					tracing::warn!(error = %err, "failed to publish final video packets");
				}
			}
			Err(err) => tracing::warn!(error = %err, "failed to flush video encoder"),
		}
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
		// Clear active too: otherwise a shutdown that races an
		// still-subscribed track leaves the worker in the capture path,
		// where it never checks `closed` until the next publish fails.
		state.active = false;
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

//! Encode decoded video frames and publish them as a moq video track.
//!
//! Encoding is strictly on demand: the track and catalog entry are advertised
//! immediately, but the camera stays closed (LED off, no CPU) until a subscriber
//! appears. When the last viewer leaves, the camera is released again. This
//! mirrors `moq-boy`, which pauses its emulator on `TrackProducer::used()` /
//! `unused()`.

use std::sync::{Arc, Condvar, Mutex};

use moq_net::Timestamp;

use crate::Error;
use crate::capture::{self, FrameSource};

use super::encoder::{self, Codec, Encoder};

/// Last-resort framerate when neither the caller nor the camera reports one.
const DEFAULT_FRAMERATE: u32 = 30;

/// Per-codec splitter + importer pair. Each codec frames its packets and resolves
/// its catalog rendition differently, so the producer holds one of these.
enum Codecs {
	H264 {
		split: moq_mux::codec::h264::Split,
		import: moq_mux::codec::h264::Import,
	},
	H265 {
		split: moq_mux::codec::h265::Split,
		import: moq_mux::codec::h265::Import,
	},
}

/// Publishes encoded video frames as a moq track (avc3 / hev1 depending on the
/// codec).
///
/// Built on the async side so the track is advertised (and the catalog
/// registered) before the camera opens; this is what lets a subscriber
/// trigger capture on demand. The `moq_mux::codec` importer for the codec
/// handles catalog registration and framing.
pub struct Producer {
	codecs: Codecs,
}

impl Producer {
	/// Publish a track for `codec` into `broadcast`, registering its rendition
	/// in `catalog`. The packets fed to [`publish`](Self::publish) must be in
	/// that codec's framing (the matching [`Encoder`](super::Encoder) emits it).
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		codec: Codec,
	) -> Result<Self, Error> {
		let codecs = match codec {
			Codec::H264 => {
				let track = moq_mux::import::unique_track(&mut broadcast, ".avc3")?;
				Codecs::H264 {
					split: moq_mux::codec::h264::Split::new(),
					import: moq_mux::codec::h264::Import::new(track, catalog),
				}
			}
			Codec::H265 => {
				let track = moq_mux::import::unique_track(&mut broadcast, ".hev1")?;
				Codecs::H265 {
					split: moq_mux::codec::h265::Split::new(),
					import: moq_mux::codec::h265::Import::new(track, catalog),
				}
			}
		};
		Ok(Self { codecs })
	}

	/// A watch-only handle to the track's subscriber demand, created eagerly so
	/// subscription state is observable before any frames arrive. Watch it via
	/// [`used`](moq_net::TrackDemand::used) / [`unused`](moq_net::TrackDemand::unused).
	pub fn demand(&self) -> moq_net::TrackDemand {
		match &self.codecs {
			Codecs::H264 { import, .. } => import.demand(),
			Codecs::H265 { import, .. } => import.demand(),
		}
	}

	/// Publish already-encoded packets at the given timestamp. Each packet is one
	/// whole access unit in the producer's codec framing.
	pub fn publish(&mut self, packets: Vec<bytes::Bytes>, timestamp: Timestamp) -> Result<(), Error> {
		for packet in packets {
			// The encoder emits one whole access unit per packet, so flush to emit it.
			match &mut self.codecs {
				Codecs::H264 { split, import } => {
					let mut frames = split.decode(&packet, Some(timestamp))?;
					frames.extend(split.flush(Some(timestamp))?);
					import.decode(frames)?;
				}
				Codecs::H265 { split, import } => {
					let mut frames = split.decode(&packet, Some(timestamp))?;
					frames.extend(split.flush(Some(timestamp))?);
					import.decode(frames)?;
				}
			}
		}
		Ok(())
	}

	/// Finalize the track.
	pub fn finish(&mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.finish()?,
			Codecs::H265 { import, .. } => import.finish()?,
		}
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
	/// Output codec. Defaults to [`Codec::H264`].
	pub codec: Codec,
	/// Encoder implementation preference.
	pub kind: encoder::Kind,
}

/// Capture a webcam and publish it as an on-demand video track.
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

	let producer = Producer::new(broadcast, catalog, encode.codec)?;
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
			encoder_config.codec = encode.codec;
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::encode::{Config, Encoder};

	/// Encode a handful of synthetic frames for `codec` and publish them through a
	/// real [`Producer`], returning the catalog rendition's track name. The
	/// rendition only appears once the matching importer parses the codec config
	/// out of the encoded keyframe, so a returned name proves the whole
	/// encode -> split -> import -> catalog path works for that codec.
	///
	/// `kind` is explicit so the test picks a deterministic encoder rather than
	/// `Auto`, which on Linux CI would try the NVENC backend and panic in cudarc
	/// on a GPU-less runner.
	async fn roundtrip_rendition(codec: Codec, kind: encoder::Kind) -> String {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut producer = Producer::new(broadcast, catalog.clone(), codec).unwrap();

		let mut config = Config::new(320, 240, 30);
		config.codec = codec;
		config.kind = kind;
		let mut encoder = Encoder::new(&config).unwrap();
		assert_eq!(encoder.codec(), codec);

		let rgba = vec![0x80u8; 320 * 240 * 4];
		for i in 0..10u64 {
			let packets = encoder.encode_rgba(&rgba, 320, 240, i == 0).unwrap();
			let ts = Timestamp::from_micros(i * 33_333).unwrap();
			producer.publish(packets, ts).unwrap();
		}
		let tail = encoder.finish().unwrap();
		producer
			.publish(tail, Timestamp::from_micros(10 * 33_333).unwrap())
			.unwrap();

		let snapshot = catalog.snapshot();
		snapshot
			.video
			.renditions
			.keys()
			.next()
			.cloned()
			.expect("the importer should have registered a video rendition")
	}

	#[tokio::test]
	async fn h264_roundtrip_publishes_avc3() {
		// Software (openh264) so the test is deterministic and never touches a
		// hardware backend.
		assert!(
			roundtrip_rendition(Codec::H264, encoder::Kind::Software)
				.await
				.ends_with(".avc3")
		);
	}

	/// H.265 has no software encoder, so this only runs where a hardware one
	/// exists (VideoToolbox on macOS, the only hardware backend on this target).
	#[cfg(target_os = "macos")]
	#[tokio::test]
	async fn h265_roundtrip_publishes_hev1() {
		assert!(
			roundtrip_rendition(Codec::H265, encoder::Kind::Hardware)
				.await
				.ends_with(".hev1")
		);
	}
}

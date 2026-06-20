//! Encode decoded video frames and publish them as an H.264 moq track.
//!
//! Encoding is strictly on demand: the avc3 track and catalog entry are
//! advertised immediately, but the camera stays closed (LED off, no CPU)
//! until a subscriber appears. When the last viewer leaves, the camera is
//! released again. This mirrors `moq-boy`, which pauses its emulator on
//! `TrackProducer::used()` / `unused()`.

use moq_net::Timestamp;

use crate::Error;
use crate::capture;

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

	let mut producer = Producer::new(broadcast, catalog)?;
	let demand = producer.demand();

	let result = capture_loop(&mut producer, &demand, &capture, &encode, &clock).await;

	// Best-effort clean close. This runs only when the loop ends on its own (the
	// track is usually already going away by then); a Ctrl+C cancels the future
	// before this point, since async `Drop` can't finalize the track.
	if let Err(err) = producer.finish() {
		tracing::debug!(error = %err, "video track finish after capture ended");
	}
	result
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

/// Async capture/encode loop. Captures one frame up front to populate the
/// catalog (the codec/resolution only exist once the encoder has produced an
/// SPS), then releases the camera whenever the last viewer leaves and reopens it
/// when one returns.
///
/// Cancel safety: every wait here is a real `.await` (a frame read or a demand
/// transition), so dropping this future (e.g. on Ctrl+C) drops `camera`, which
/// releases the device and turns the LED off. No blocking thread is left behind.
async fn capture_loop(
	producer: &mut Producer,
	demand: &moq_net::TrackDemand,
	capture: &capture::Config,
	encode: &Options,
	clock: &moq_mux::Clock,
) -> Result<(), Error> {
	// The catalog video rendition only appears once a frame has been encoded (the
	// importer reads the SPS). Until then we capture regardless of demand so a
	// catalog-driven subscriber can discover the track and trigger `used()`.
	// After that we release the camera while unwatched.
	let mut catalog_ready = false;

	loop {
		if catalog_ready {
			// Idle until a viewer subscribes; the track ending is a clean exit.
			if let Err(err) = demand.used().await {
				log_track_ended(err);
				return Ok(());
			}
		}

		// Open the camera and an encoder sized to its negotiated mode.
		let mut camera = capture::open(capture).await?;
		// Prefer an explicit --fps, otherwise the camera's reported rate, falling
		// back only if the backend doesn't expose one.
		let framerate = capture
			.framerate
			.or_else(|| camera.framerate())
			.unwrap_or(DEFAULT_FRAMERATE);
		let mut encoder_config = encoder::Config::new(camera.width(), camera.height(), framerate);
		encoder_config.bitrate = encode.bitrate;
		encoder_config.kind = encode.kind.clone();
		let mut encoder = Encoder::new(&encoder_config)?;
		// Force an IDR on the first frame of each (re)open so a viewer subscribing
		// after an idle gap can start decoding immediately.
		let mut force_keyframe = true;
		tracing::info!(encoder = encoder.name(), device = camera.device(), "capturing");

		loop {
			// While watched, race the next frame against the last viewer leaving so
			// we release the camera promptly when demand drops. `biased` checks
			// demand first so an unwatched track stops before reading another frame.
			let frame = if catalog_ready {
				tokio::select! {
					biased;
					res = demand.unused() => {
						if let Err(err) = res {
							log_track_ended(err);
							return Ok(());
						}
						break; // no viewers: release the camera, then wait for one
					}
					frame = camera.read() => frame,
				}
			} else {
				camera.read().await
			};

			let Some(frame) = frame else { break }; // device stopped producing frames

			let ts = Timestamp::from_micros(clock.micros())?;
			let packets = encoder.encode(&frame, force_keyframe)?;
			force_keyframe = false;
			// Once the encoder emits a frame the importer has parsed the SPS and
			// the catalog rendition exists, so demand gating can take over.
			catalog_ready |= !packets.is_empty();
			producer.publish(packets, ts)?;
		}

		// Drop the camera (LED off) and encoder before waiting for the next viewer.
		drop(camera);
		if catalog_ready {
			tracing::info!("no viewers: released camera");
		}
	}
}

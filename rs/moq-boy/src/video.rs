//! Video encoding pipeline: RGBA framebuffer -> H.264 -> MoQ, via `moq-video`.
//!
//! Runs on a dedicated thread so the emulator's frame loop never blocks on the
//! encoder. Frames arrive on a bounded channel; if the encoder falls behind,
//! frames are dropped to keep latency low. moq-video does the RGBA -> H.264
//! encode and the avc3 publish; this module keeps moq-boy's threading,
//! frame-dropping, force-keyframe and timing-stats behavior.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::emulator::{HEIGHT, WIDTH};

/// Handle to the video encoding thread.
///
/// Frames are submitted via `try_frame()` (non-blocking, drops if full).
pub struct VideoEncoder {
	tx: tokio::sync::mpsc::Sender<EncoderMsg>,
	/// Watch-only handle to the video track, for monitoring used/unused.
	pub demand: moq_net::TrackDemand,
	force_keyframe: Arc<AtomicBool>,
	/// Latest encode duration in microseconds.
	encode_duration: Arc<AtomicU64>,
	_thread: std::thread::JoinHandle<()>,
}

struct EncoderMsg {
	rgba: Bytes,
	ts: hang::container::Timestamp,
}

impl VideoEncoder {
	pub fn spawn(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Self {
		let (tx, rx) = tokio::sync::mpsc::channel(4);
		let producer = moq_video::encode::Producer::new(broadcast, catalog, moq_video::encode::Codec::H264)
			.expect("failed to create avc3 producer");
		let demand = producer.demand();

		let force_keyframe = Arc::new(AtomicBool::new(false));
		let encode_duration = Arc::new(AtomicU64::new(0));
		let fk = force_keyframe.clone();
		let ed = encode_duration.clone();
		let thread = std::thread::Builder::new()
			.name("video-encoder".into())
			.spawn(move || encoder_thread(rx, producer, fk, ed))
			.expect("failed to spawn video encoder thread");

		Self {
			tx,
			demand,
			force_keyframe,
			encode_duration,
			_thread: thread,
		}
	}

	/// Send a frame to the encoder. Non-blocking: drops the frame if the
	/// channel is full (capacity=4) to keep latency low.
	pub fn try_frame(&self, rgba: Bytes, ts: hang::container::Timestamp) {
		if self.tx.try_send(EncoderMsg { rgba, ts }).is_err() {
			tracing::warn!("video frame dropped: encoder backpressure");
		}
	}

	/// Force the next encoded frame to be a keyframe (I-frame).
	/// Used on resume after pause so new viewers can start decoding.
	pub fn force_keyframe(&self) {
		self.force_keyframe.store(true, Ordering::Release);
	}

	/// Latest per-frame encode duration.
	pub fn encode_duration(&self) -> Duration {
		Duration::from_micros(self.encode_duration.load(Ordering::Relaxed))
	}
}

fn encoder_thread(
	mut rx: tokio::sync::mpsc::Receiver<EncoderMsg>,
	mut producer: moq_video::encode::Producer,
	force_keyframe: Arc<AtomicBool>,
	encode_duration: Arc<AtomicU64>,
) {
	let mut encoder: Option<moq_video::encode::Encoder> = None;

	while let Some(msg) = rx.blocking_recv() {
		let enc = match encoder.as_mut() {
			Some(enc) => enc,
			None => {
				// Game Boy is 160x144; force software (libx264) since hardware
				// encoders can reject such tiny resolutions.
				let mut config = moq_video::encode::Config::new(WIDTH, HEIGHT, 60);
				config.kind = moq_video::encode::Kind::Software;
				match moq_video::encode::Encoder::new(&config) {
					Ok(enc) => encoder.insert(enc),
					Err(e) => {
						tracing::error!(error = %e, "H.264 encoder init failed");
						return;
					}
				}
			}
		};

		let keyframe = force_keyframe.swap(false, Ordering::AcqRel);
		let start = Instant::now();
		match enc.encode_rgba(&msg.rgba, WIDTH, HEIGHT, keyframe) {
			Ok(packets) => {
				if let Err(e) = producer.publish(packets, msg.ts) {
					// Publish only fails once the track/broadcast is gone, which
					// is terminal -- stop rather than flooding logs every frame.
					tracing::error!(error = %e, "video publish failed; stopping encoder");
					return;
				}
			}
			// A single bad frame is tolerable; keep going.
			Err(e) => tracing::error!(error = %e, "H.264 encode error"),
		}
		encode_duration.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
	}
}

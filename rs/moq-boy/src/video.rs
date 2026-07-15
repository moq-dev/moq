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

enum EncoderMsg {
	/// Encode and publish one RGBA framebuffer at `ts`.
	Frame {
		rgba: Bytes,
		ts: hang::container::Timestamp,
	},
	/// Close the current group, marking its content as ending at `end` (the video
	/// track went idle). Ordered after the last `Frame` so the group closes cleanly.
	EndGroup { end: hang::container::Timestamp },
}

impl VideoEncoder {
	pub fn spawn(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Self {
		let (tx, rx) = tokio::sync::mpsc::channel(4);
		let producer = moq_video::encode::Producer::new(broadcast, catalog).expect("failed to create avc3 producer");
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
		if self.tx.try_send(EncoderMsg::Frame { rgba, ts }).is_err() {
			tracing::warn!("video frame dropped: encoder backpressure");
		}
	}

	/// Close the current video group, marking its content as ending at `end`, and
	/// force the next frame to be a keyframe.
	///
	/// Call when the video track goes idle (pause). A consumer then bounds the last
	/// frame at `end` instead of stretching it across the idle gap to the resumed
	/// group, and the resumed group still opens on a decodable keyframe. This replaces
	/// a separate force-keyframe-on-resume: closing a group already requires the next
	/// frame to be a keyframe, so the two are one operation.
	pub fn cut(&self, end: hang::container::Timestamp) {
		// Arm the keyframe before the close so the resumed group opens on an IDR.
		self.force_keyframe.store(true, Ordering::Release);
		// Blocking, not try_send: end-of-group is rare and must not be dropped, or the
		// last pre-pause frame would stretch across the gap -- the bug this prevents.
		if self.tx.blocking_send(EncoderMsg::EndGroup { end }).is_err() {
			tracing::warn!("video cut dropped: encoder gone");
		}
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
		let (rgba, ts) = match msg {
			EncoderMsg::Frame { rgba, ts } => (rgba, ts),
			EncoderMsg::EndGroup { end } => {
				// Close the current group at the pre-pause end so the last frame isn't
				// stretched across the idle gap. No encoder work: it's just a boundary.
				if let Err(e) = producer.cut(end) {
					tracing::error!(error = %e, "video cut failed; stopping encoder");
					return;
				}
				continue;
			}
		};

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
		match enc.encode_rgba(&rgba, WIDTH, HEIGHT, keyframe) {
			Ok(packets) => {
				if let Err(e) = producer.publish(packets, ts) {
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

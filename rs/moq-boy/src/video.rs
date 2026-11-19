//! Video encoding pipeline: RGBA framebuffer -> H.264 -> MoQ, via `moq-video`.
//!
//! Runs on a dedicated thread so the emulator's frame loop never blocks on the
//! encoder. Frames arrive on a bounded channel; if the encoder falls behind,
//! frames are dropped to keep latency low. moq-video's encode producer does the
//! RGBA -> H.264 encode and the avc3 publish; this module keeps moq-boy's threading,
//! frame-dropping, force-keyframe and timing-stats behavior.

use kio::clock::Instant;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;

use crate::emulator::{HEIGHT, WIDTH};

/// The Game Boy's 59.727 Hz, rounded: the encoder's time base and the framerate the catalog
/// advertises.
const FRAMERATE: u32 = 60;

/// Handle to the video encoding thread.
///
/// Frames are submitted via `try_frame()` (non-blocking, drops if full).
pub struct VideoEncoder {
	tx: tokio::sync::mpsc::Sender<EncoderMsg>,
	/// Watch-only handle to the video track, for monitoring used/unused.
	pub demand: moq_net::track::Demand,
	force_keyframe: Arc<AtomicBool>,
	/// Latest encode duration in microseconds.
	encode_duration: Arc<AtomicU64>,
	_thread: std::thread::JoinHandle<()>,
}

enum EncoderMsg {
	Frame {
		rgba: Bytes,
		ts: hang::container::Timestamp,
	},
	/// Publish the pause marker. Goes down the same channel as the frames so it lands
	/// after the last one encoded, not racing ahead of it.
	Discontinuity,
}

impl VideoEncoder {
	/// Publish the catalog rendition, then hand the encoder to a worker thread. The returned
	/// handle feeds it frames; the emulator never blocks on it.
	pub async fn spawn(broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Self {
		let (tx, rx) = tokio::sync::mpsc::channel(4);

		// Game Boy is 160x144; force the openh264 software encoder since hardware
		// encoders can reject such tiny resolutions.
		let mut config =
			moq_video::encode::Config::new(WIDTH, HEIGHT, moq_video::FrameRate::new(FRAMERATE, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;

		// The rendition is probed before the emulator has run a frame, so a viewer can discover
		// (and unpause) a moq-boy that has published nothing.
		let options = moq_video::encode::Options::new(config);
		let producer = moq_video::encode::Producer::new(&broadcast, catalog, options)
			.await
			.expect("failed to open the H.264 encoder");
		let demand = producer.demand();

		let force_keyframe = Arc::new(AtomicBool::new(false));
		let encode_duration = Arc::new(AtomicU64::new(0));
		let fk = force_keyframe.clone();
		let ed = encode_duration.clone();
		// The thread drives the producer's futures through this runtime, whose timers its rate
		// control uses.
		let runtime = tokio::runtime::Handle::current();
		let thread = std::thread::Builder::new()
			.name("video-encoder".into())
			.spawn(move || runtime.block_on(encoder_thread(rx, producer, fk, ed)))
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

	/// End the open group whole where its pictures stop, so the PTS jump on resume reads as a
	/// break rather than one very long frame, and a viewer arriving mid-pause isn't served the
	/// pre-pause group as if it were live. The encoder closes until the next frame, so a
	/// paused emulator holds none.
	///
	/// Blocks rather than dropping on a full channel: a dropped frame costs one frame, a
	/// dropped marker silently reinstates the bug it exists to prevent.
	pub fn discontinuity(&self) {
		if self.tx.blocking_send(EncoderMsg::Discontinuity).is_err() {
			tracing::warn!("video discontinuity dropped: encoder gone");
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

/// Encode each frame on its own thread, so a slow encode never stalls the emulator.
///
/// Driven by `Handle::block_on` on a thread of its own, so the producer's awaits block only it.
async fn encoder_thread(
	mut rx: tokio::sync::mpsc::Receiver<EncoderMsg>,
	mut producer: moq_video::encode::Producer,
	force_keyframe: Arc<AtomicBool>,
	encode_duration: Arc<AtomicU64>,
) {
	while let Some(msg) = rx.recv().await {
		let (rgba, ts) = match msg {
			EncoderMsg::Frame { rgba, ts } => (rgba, ts),
			EncoderMsg::Discontinuity => {
				// Marked even when the drain fails, which only loses the last frames.
				if let Err(e) = producer.discontinuity().await {
					tracing::warn!(error = %e, "failed to drain the video encoder at the discontinuity");
				}
				continue;
			}
		};

		// Asked for before the conversion below, which can fail: the encoder holds the
		// request until a frame actually arrives, so dropping this frame delays the
		// keyframe rather than losing it. A viewer needs a decodable starting point;
		// otherwise the encoder's own GOP keys the stream. A refusal is terminal:
		// only a backend that cannot cut at all gives one, and this never selects one.
		if force_keyframe.swap(false, Ordering::AcqRel)
			&& let Err(e) = producer.cut().await
		{
			tracing::error!(error = %e, "H.264 encoder cannot cut a group; stopping encoder");
			producer.abort(moq_net::Error::Transport(e.to_string()));
			return;
		}
		let start = Instant::now();
		let surface = match moq_video::Surface::rgba(&rgba, moq_video::Size::new(WIDTH, HEIGHT)) {
			Ok(surface) => surface,
			// A single bad frame is tolerable; keep going.
			Err(e) => {
				tracing::error!(error = %e, "RGBA conversion error");
				continue;
			}
		};
		// A frame of the configured size only fails to encode or publish once the codec or the
		// track is gone, which is terminal: stop rather than flooding logs every frame.
		if let Err(e) = producer.write(moq_video::Frame::new(surface, ts)).await {
			tracing::error!(error = %e, "video encode failed; stopping encoder");
			// Subscribers see the cause rather than an unfinished track's `Dropped`.
			producer.abort(moq_net::Error::Transport(e.to_string()));
			return;
		}
		encode_duration.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
	}
}

//! Video encoding pipeline: RGBA framebuffer -> H.264 -> MoQ.
//!
//! Runs on a dedicated thread to avoid blocking the emulator's frame loop.
//! The emulator sends RGBA frames via a bounded channel; if the encoder
//! falls behind, frames are dropped with a warning (to keep latency low).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::emulator::{HEIGHT, WIDTH};

/// Handle to the video encoding thread.
///
/// Frames are submitted via `try_frame()` (non-blocking, drops if full).
/// The encoder thread converts RGBA -> YUV420P -> H.264 and publishes
/// encoded packets via `moq_mux::import::Avc3`.
pub struct VideoEncoder {
	tx: tokio::sync::mpsc::Sender<EncoderMsg>,
	/// Clone of the video track producer, for monitoring used/unused.
	pub track: moq_lite::TrackProducer,
	force_keyframe: Arc<AtomicBool>,
	_thread: std::thread::JoinHandle<()>,
}

struct EncoderMsg {
	rgba: Bytes,
	ts: hang::container::Timestamp,
}

impl VideoEncoder {
	pub fn spawn(broadcast: moq_lite::BroadcastProducer, catalog: moq_mux::CatalogProducer) -> Self {
		let (tx, rx) = tokio::sync::mpsc::channel(4);
		let avc3 = moq_mux::import::Avc3::new(broadcast, catalog);
		let force_keyframe = Arc::new(AtomicBool::new(false));
		let track = avc3.track().clone();

		let fk = force_keyframe.clone();
		let thread = std::thread::Builder::new()
			.name("video-encoder".into())
			.spawn(move || encoder_thread(rx, avc3, fk))
			.expect("failed to spawn video encoder thread");

		Self {
			tx,
			track,
			force_keyframe,
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
}

fn encoder_thread(
	mut rx: tokio::sync::mpsc::Receiver<EncoderMsg>,
	mut avc3: moq_mux::import::Avc3,
	force_keyframe: Arc<AtomicBool>,
) {
	let mut encoder: Option<Encoder> = None;
	let mut scaler: Option<ffmpeg_next::software::scaling::Context> = None;

	while let Some(msg) = rx.blocking_recv() {
		let enc = lazy_init(&mut encoder, || Encoder::new(WIDTH, HEIGHT), "H.264 encoder");
		let color_scaler = lazy_init(
			&mut scaler,
			|| {
				// POINT filtering (nearest neighbor) preserves pixel-art crispness.
				ffmpeg_next::software::scaling::Context::get(
					ffmpeg_next::format::Pixel::RGBA,
					WIDTH,
					HEIGHT,
					ffmpeg_next::format::Pixel::YUV420P,
					WIDTH,
					HEIGHT,
					ffmpeg_next::software::scaling::Flags::POINT,
				)
				.map_err(Into::into)
			},
			"RGBA scaler",
		);

		let (Some(enc), Some(color_scaler)) = (enc, color_scaler) else {
			return;
		};

		let mut yuv = match rgba_to_yuv(&msg.rgba, color_scaler, enc.frame_count) {
			Ok(f) => f,
			Err(e) => {
				tracing::error!(error = %e, "RGBA->YUV failed");
				continue;
			}
		};

		if force_keyframe.swap(false, Ordering::AcqRel) {
			yuv.set_kind(ffmpeg_next::picture::Type::I);
		}

		if let Err(e) = enc.encode_yuv(&yuv, msg.ts, &mut avc3) {
			tracing::error!(error = %e, "H.264 encode error");
		}
	}
}

fn lazy_init<'a, T>(slot: &'a mut Option<T>, init: impl FnOnce() -> Result<T>, name: &str) -> Option<&'a mut T> {
	if slot.is_none() {
		match init() {
			Ok(v) => *slot = Some(v),
			Err(e) => {
				tracing::error!(error = %e, "{name} init failed");
				return None;
			}
		}
	}
	slot.as_mut()
}

/// Convert RGBA framebuffer to YUV420P for the H.264 encoder.
/// Copies row-by-row to handle ffmpeg's stride (which may differ from width*4).
fn rgba_to_yuv(
	rgba: &[u8],
	scaler: &mut ffmpeg_next::software::scaling::Context,
	frame_count: u64,
) -> Result<ffmpeg_next::frame::Video> {
	let mut rgba_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, WIDTH, HEIGHT);
	let stride = rgba_frame.stride(0);
	let row_bytes = WIDTH as usize * 4;

	for y in 0..HEIGHT as usize {
		let src_offset = y * row_bytes;
		let dst_offset = y * stride;
		rgba_frame.data_mut(0)[dst_offset..dst_offset + row_bytes]
			.copy_from_slice(&rgba[src_offset..src_offset + row_bytes]);
	}

	let mut yuv = ffmpeg_next::frame::Video::empty();
	scaler.run(&rgba_frame, &mut yuv)?;
	yuv.set_pts(Some(frame_count as i64));

	// First frame is always an I-frame to bootstrap the decoder.
	if frame_count == 0 {
		yuv.set_kind(ffmpeg_next::picture::Type::I);
	}

	Ok(yuv)
}

struct Encoder {
	encoder: ffmpeg_next::encoder::video::Encoder,
	frame_count: u64,
}

impl Encoder {
	fn new(width: u32, height: u32) -> Result<Self> {
		let codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264).context("H.264 encoder not found")?;
		let ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
		let mut enc = ctx.encoder().video()?;
		enc.set_width(width);
		enc.set_height(height);
		enc.set_format(ffmpeg_next::format::Pixel::YUV420P);
		enc.set_time_base(ffmpeg_next::Rational::new(1, 60));
		enc.set_frame_rate(Some(ffmpeg_next::Rational::new(60, 1)));
		// Keyframe every 240 frames (~4 seconds at 60fps).
		// Viewers joining mid-stream wait at most this long for a keyframe.
		enc.set_gop(240);

		let mut opts = ffmpeg_next::Dictionary::new();
		opts.set("preset", "ultrafast"); // Minimize encoding latency.
		opts.set("tune", "zerolatency"); // Disable B-frames and lookahead.
		opts.set("crf", "18"); // High quality — GB content is very low bitrate regardless.
		let encoder = enc.open_with(opts)?;

		Ok(Self {
			encoder,
			frame_count: 0,
		})
	}

	fn encode_yuv(
		&mut self,
		yuv: &ffmpeg_next::frame::Video,
		ts: hang::container::Timestamp,
		output: &mut moq_mux::import::Avc3,
	) -> Result<()> {
		self.encoder.send_frame(yuv)?;

		let mut pkt = ffmpeg_next::Packet::empty();
		while self.encoder.receive_packet(&mut pkt).is_ok() {
			let data = pkt.data().context("empty encoded packet")?;
			output.decode_frame(&mut &*data, Some(ts))?;
		}
		self.frame_count += 1;

		Ok(())
	}
}

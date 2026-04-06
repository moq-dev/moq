use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::emulator::{HEIGHT, WIDTH};

/// Run the video encoding pipeline on a dedicated thread.
/// Receives RGBA frames, encodes to H.264, publishes via MoQ.
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

	/// Send a frame to the encoder. Non-blocking: drops the frame if the channel is full.
	pub fn try_frame(&self, rgba: Bytes, ts: hang::container::Timestamp) {
		let _ = self.tx.try_send(EncoderMsg { rgba, ts });
	}

	/// Force the next encoded frame to be a keyframe. Cannot be lost.
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
		enc.set_gop(120);

		let mut opts = ffmpeg_next::Dictionary::new();
		opts.set("preset", "ultrafast");
		opts.set("tune", "zerolatency");
		opts.set("crf", "18");
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

//! Hardware H.264 / H.265 backend via Android MediaCodec (`AMediaCodec`).
//!
//! Runs the codec in synchronous ByteBuffer mode: each raw frame is written into
//! a dequeued input buffer as NV12, and the finished access units are drained
//! back out. MediaCodec emits Annex-B, which is what the matching catalog
//! importer wants (`moq_mux` avc3 / hev1 mode), so unlike VideoToolbox there is
//! no AVCC rewrite. The parameter sets arrive once as a codec-config buffer and
//! are prepended to every keyframe that doesn't already carry them, which is
//! what makes each IDR independently decodable.
//!
//! The codec is a queued device: it encodes frame N while frame N+k goes in, so
//! an access unit is stamped with the frame it belongs to, found through the
//! sample time the codec echoes back in its `BufferInfo`, rather than with
//! whatever frame happens to be going in at the time.
//!
//! Two things the NDK only exposes from API 28, above the API 26 this crate
//! builds against:
//!
//! - The input buffer geometry (`AMediaCodec_getInputFormat`), so the NV12 is
//!   written tightly packed at the configured resolution. A device whose encoder
//!   pads its input rows is the gap.
//! - The name of the codec that was opened (`AMediaCodec_getName`), so a device
//!   with no hardware encoder for the requested codec gets the AOSP software one
//!   under this name instead of falling through to openh264. MediaCodec hands
//!   back the device's preferred encoder, which is the hardware one wherever
//!   there is one. The visible consequence is that
//!   [`Kind::Hardware`](crate::encode::Kind::Hardware) cannot be enforced here:
//!   this backend sits in the hardware table and answers even where the device
//!   has only a software encoder.
//!
//! The codec is created, driven, and dropped on the dedicated encode thread (see
//! `encode::sink`), which is what lets the blocking waits here not park a tokio
//! worker.

use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use moq_net::Timestamp;
use ndk::media::media_codec::{
	self, BufferInfo, DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodecDirection, OutputBuffer,
};
use ndk::media::media_format::MediaFormat;
use ndk::media_error::MediaError;

use super::super::encoder::{Codec, Config};
use super::{Backend, Encoded};
use crate::{Color, Error, Frame, I420};

pub(crate) const NAME: &str = "mediacodec";

/// The MIME types MediaCodec names the two codecs by.
const MIME_H264: &str = "video/avc";
const MIME_H265: &str = "video/hevc";

// `AMediaFormat` keys. The NDK ships them as `AMEDIAFORMAT_KEY_*` string
// constants that the `ndk` crate does not re-export; they are the same strings
// `android.media.MediaFormat` documents.
const KEY_MIME: &str = "mime";
const KEY_WIDTH: &str = "width";
const KEY_HEIGHT: &str = "height";
const KEY_BIT_RATE: &str = "bitrate";
const KEY_BITRATE_MODE: &str = "bitrate-mode";
const KEY_FRAME_RATE: &str = "frame-rate";
const KEY_COLOR_FORMAT: &str = "color-format";
const KEY_COLOR_STANDARD: &str = "color-standard";
const KEY_COLOR_TRANSFER: &str = "color-transfer";
const KEY_COLOR_RANGE: &str = "color-range";
const KEY_I_FRAME_INTERVAL: &str = "i-frame-interval";
const KEY_LATENCY: &str = "latency";
const KEY_LOW_LATENCY: &str = "low-latency";
const KEY_MAX_B_FRAMES: &str = "max-bframes";
const KEY_PRIORITY: &str = "priority";
const KEY_REQUEST_SYNC_FRAME: &str = "request-sync";
const KEY_VIDEO_BITRATE: &str = "video-bitrate";

/// `COLOR_FormatYUV420SemiPlanar`: NV12, a full-size luma plane followed by one
/// interleaved chroma plane. The one raw format every MediaCodec encoder
/// accepts.
const COLOR_FORMAT_NV12: i32 = 21;

/// `BITRATE_MODE_CBR`. A live track is sized by its uplink, not by its content,
/// so a constant rate is what the congestion controller's estimate means.
const BITRATE_MODE_CBR: i32 = 2;

/// `PRIORITY_REALTIME`: this is a live stream, not a file being transcoded.
const PRIORITY_REALTIME: i32 = 0;

// `MediaFormat.COLOR_STANDARD_*` / `COLOR_TRANSFER_*` / `COLOR_RANGE_*`, the
// codes MediaCodec translates into the bitstream's VUI.
const COLOR_STANDARD_BT709: i32 = 1;
const COLOR_STANDARD_BT601_NTSC: i32 = 4;
const COLOR_TRANSFER_SDR_VIDEO: i32 = 3;
const COLOR_RANGE_FULL: i32 = 1;
const COLOR_RANGE_LIMITED: i32 = 2;

// `AMEDIACODEC_BUFFER_FLAG_*`, likewise not re-exported by the `ndk` crate.
const FLAG_KEY_FRAME: u32 = 1;
const FLAG_CODEC_CONFIG: u32 = 2;
const FLAG_END_OF_STREAM: u32 = 4;

/// How long to wait for a free input buffer before dropping the frame. A codec
/// that has not freed one within about a third of a frame interval at 30 fps is
/// backed up behind its own output, and a live encoder that blocks instead
/// builds latency it can never give back.
const INPUT_TIMEOUT: Duration = Duration::from_millis(10);

/// Poll for finished output without waiting. [`MediaCodec::encode`] drains on
/// both sides of every submission, so output that isn't ready yet is collected
/// on the next frame; blocking here would spend most of a frame interval waiting
/// for a picture the codec hasn't started.
const OUTPUT_TIMEOUT: Duration = Duration::ZERO;

/// How long each round of a tail drain waits for the next access unit.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(50);

/// How many rounds a tail drain runs before giving up on an end-of-stream that
/// never arrives, so a wedged codec fails the group rather than the process.
const DRAIN_ROUNDS: u32 = 20;

pub(crate) struct MediaCodec {
	codec: media_codec::MediaCodec,
	/// Which codec's NAL headers to read, for spotting a keyframe that already
	/// carries its own parameter sets.
	kind: Codec,
	width: usize,
	height: usize,
	framerate: u32,
	/// The parameter sets (SPS/PPS, plus VPS for H.265) as the codec-config
	/// buffer delivered them: Annex-B, ahead of the first picture. Kept because
	/// every keyframe has to carry them for a subscriber joining there.
	parameter_sets: Option<Bytes>,
	/// Frames handed to the codec that haven't come back out, oldest first: the
	/// sample time we gave it paired with the frame's real timestamp. MediaCodec
	/// buffers, so its output is matched against this rather than stamped with
	/// whatever is being fed at the time.
	pending: VecDeque<(i64, Timestamp)>,
	/// The last timestamp paired with an output, reused if the codec ever hands
	/// back more access units than it was fed frames. Repeating a time is far
	/// kinder to a consumer than the jump to zero the alternative would produce.
	last_timestamp: Option<Timestamp>,
	frame_index: i64,
	/// Set while an IDR is owed: asked for by the caller, or by a flush that
	/// dropped the reference frames the next picture would predict from.
	keyframe_pending: bool,
	/// True once end of input has been signalled and before the flush that takes
	/// it back, since a codec in that state refuses input.
	ended: bool,
}

// SAFETY: `AMediaCodec` is an owned handle with no thread affinity; the NDK only
// requires that calls on one codec are serialized, which they are because every
// method here takes `&mut self` and the encode thread owns the backend outright.
// `Send` is what lets the boxed trait object satisfy `Backend: Send`.
unsafe impl Send for MediaCodec {}

impl MediaCodec {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		// backend::open only routes codecs this backend advertises, so the match is
		// exhaustive; a new Codec variant won't compile here until it's handled.
		let mime = match config.codec {
			Codec::H264 => MIME_H264,
			Codec::H265 => MIME_H265,
		};

		let codec = media_codec::MediaCodec::from_encoder_type(mime)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("no MediaCodec encoder for {mime}")))?;
		let format = encoder_format(config, mime);
		codec
			.configure(&format, None, MediaCodecDirection::Encoder)
			.map_err(|e| codec_err("configure", e))?;
		codec.start().map_err(|e| codec_err("start", e))?;

		tracing::info!(
			encoder = NAME,
			codec = ?config.codec,
			width = config.width,
			height = config.height,
			"opened video encoder"
		);
		Ok(Box::new(Self {
			codec,
			kind: config.codec,
			width: config.width as usize,
			height: config.height as usize,
			framerate: config.framerate.max(1),
			parameter_sets: None,
			pending: VecDeque::new(),
			last_timestamp: None,
			frame_index: 0,
			keyframe_pending: false,
			ended: false,
		}))
	}

	/// The codec-side clock for the next frame, in microseconds.
	///
	/// A monotonic index over the configured framerate rather than the frame's
	/// own timestamp: the codec only needs a clock that increases, the real
	/// timestamp rides alongside in `pending`, and this is the value the codec
	/// echoes back on the matching output.
	fn sample_time(&self) -> i64 {
		self.frame_index * 1_000_000 / self.framerate as i64
	}

	/// Ask the codec to make the next picture an IDR.
	fn request_keyframe(&self) -> Result<(), Error> {
		let mut params = MediaFormat::new();
		params.set_i32(KEY_REQUEST_SYNC_FRAME, 0);
		self.codec
			.set_parameters(params)
			.map_err(|e| codec_err("request a sync frame", e))
	}

	/// Write one frame into a free input buffer, dropping it if the codec has
	/// none.
	fn submit(&mut self, frame: &Frame, keyframe: bool) -> Result<(), Error> {
		if keyframe {
			// Keep the request pending until a frame is actually accepted. A full
			// codec queue drops this input, but the next submitted picture still has
			// to open the group with an IDR.
			self.keyframe_pending = true;
			self.request_keyframe()?;
		}

		let i420 = frame.surface.to_i420()?;
		let size = I420::len(self.width as u32, self.height as u32);
		let sample_time = self.sample_time();

		let submitted = match self
			.codec
			.dequeue_input_buffer(INPUT_TIMEOUT)
			.map_err(|e| codec_err("dequeue an input buffer", e))?
		{
			DequeuedInputBufferResult::Buffer(mut buffer) => {
				fill_nv12(buffer.buffer_mut(), &i420, self.width, self.height)?;
				self.codec
					.queue_input_buffer(buffer, 0, size, sample_time as u64, 0)
					.map_err(|e| codec_err("queue an input buffer", e))?;
				true
			}
			// Every input buffer is still with the codec, which means it is behind
			// on encoding rather than on anything we can hand it. Drop the frame:
			// the drain around this call is what frees a buffer for the next one.
			DequeuedInputBufferResult::TryAgainLater => false,
		};

		if !submitted {
			tracing::debug!(encoder = NAME, "no input buffer available, dropping a frame");
			return Ok(());
		}

		self.pending.push_back((sample_time, frame.timestamp));
		self.frame_index += 1;
		self.keyframe_pending = false;
		Ok(())
	}

	/// Collect every access unit the codec has ready, waiting `timeout` on each
	/// round. Returns whether it reported end of stream, which is the only thing
	/// that says the tail is out rather than still coming.
	fn drain(&mut self, timeout: Duration, out: &mut Vec<Encoded>) -> Result<bool, Error> {
		loop {
			let taken = match self
				.codec
				.dequeue_output_buffer(timeout)
				.map_err(|e| codec_err("dequeue an output buffer", e))?
			{
				DequeuedOutputBufferInfoResult::Buffer(buffer) => {
					let info = *buffer.info();
					let unit = access_unit(&buffer, &info);
					// Released before anything else can fail: the slot belongs to the
					// codec, and an early return still holding it would starve encoding.
					self.codec
						.release_output_buffer(buffer, false)
						.map_err(|e| codec_err("release an output buffer", e))?;
					Some((info, unit))
				}
				DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(false),
				// A codec revising its output format or rotating its buffer set says
				// nothing about the bitstream: the parameter sets arrive as a
				// codec-config buffer either way.
				DequeuedOutputBufferInfoResult::OutputFormatChanged
				| DequeuedOutputBufferInfoResult::OutputBuffersChanged => None,
			};
			let Some((info, unit)) = taken else { continue };

			let flags = info.flags();
			if flags & FLAG_CODEC_CONFIG != 0 {
				self.parameter_sets = Some(unit);
			} else if !unit.is_empty() {
				let timestamp =
					take_timestamp(&mut self.pending, &mut self.last_timestamp, info.presentation_time_us());
				let payload = if flags & FLAG_KEY_FRAME != 0 {
					with_parameter_sets(self.parameter_sets.as_ref(), self.kind, unit)
				} else {
					unit
				};
				out.push(Encoded::new(payload, timestamp));
			}

			if flags & FLAG_END_OF_STREAM != 0 {
				return Ok(true);
			}
		}
	}

	/// Signal end of input and wait the codec's tail out, returning everything it
	/// was still holding.
	///
	/// The wait is the point. An encoder that buffers won't part with the frames
	/// it holds until it knows no more input is coming, so sweeping whatever
	/// happens to be ready already truncates the stream: that is one access unit
	/// lost per group, and on a live track the frame does not vanish but
	/// reappears ahead of the next group's keyframe.
	fn drain_tail(&mut self) -> Result<Vec<Encoded>, Error> {
		let mut out = Vec::new();
		// Every frame fed has already come back out, so there is no tail to wait
		// for and no reason to cycle the codec through end of stream. This is the
		// common case: the format asks for a one-frame pipeline.
		if self.ended || self.pending.is_empty() {
			return Ok(out);
		}

		self.signal_end_of_input(&mut out)?;
		self.ended = true;

		for _ in 0..DRAIN_ROUNDS {
			if self.drain(DRAIN_TIMEOUT, &mut out)? {
				// Anything still outstanding was discarded by the codec rather than
				// encoded, so it will never be paired with an output.
				self.pending.clear();
				return Ok(out);
			}
		}

		Err(Error::Codec(anyhow::anyhow!(
			"MediaCodec did not reach end of stream within {:?}",
			DRAIN_TIMEOUT * DRAIN_ROUNDS
		)))
	}

	/// Queue the empty buffer that marks the end of input.
	///
	/// ByteBuffer mode has no `AMediaCodec_signalEndOfInputStream`: that one is
	/// for a codec fed through a surface.
	fn signal_end_of_input(&mut self, out: &mut Vec<Encoded>) -> Result<(), Error> {
		let sample_time = self.sample_time();
		for _ in 0..DRAIN_ROUNDS {
			let queued = match self
				.codec
				.dequeue_input_buffer(INPUT_TIMEOUT)
				.map_err(|e| codec_err("dequeue an input buffer", e))?
			{
				DequeuedInputBufferResult::Buffer(buffer) => {
					self.codec
						.queue_input_buffer(buffer, 0, 0, sample_time as u64, FLAG_END_OF_STREAM)
						.map_err(|e| codec_err("queue end of stream", e))?;
					true
				}
				DequeuedInputBufferResult::TryAgainLater => false,
			};
			if queued {
				return Ok(());
			}
			// Every input buffer is still with the codec; collecting its output is
			// what frees one.
			self.drain(OUTPUT_TIMEOUT, out)?;
		}

		Err(Error::Codec(anyhow::anyhow!(
			"MediaCodec never freed an input buffer for the end of stream"
		)))
	}
}

impl Backend for MediaCodec {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error> {
		let mut out = Vec::new();
		// Collect whatever the codec finished while the caller was elsewhere, so
		// the input buffer asked for below isn't stuck behind an output nobody
		// picked up.
		self.drain(OUTPUT_TIMEOUT, &mut out)?;
		self.submit(frame, keyframe || self.keyframe_pending)?;
		self.drain(OUTPUT_TIMEOUT, &mut out)?;
		Ok(out)
	}

	fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		let out = self.drain_tail()?;
		if self.ended {
			// End of input leaves the codec refusing input, and only a flush takes
			// that back. Nothing more is needed in synchronous mode: the NDK
			// documents the `AMediaCodec_start` after a flush as an asynchronous-mode
			// requirement.
			self.codec.flush().map_err(|e| codec_err("flush", e))?;
			self.ended = false;
			// A flush drops the reference frames, so the next picture has to be an
			// IDR. Asked for rather than assumed: not every encoder inserts one.
			self.keyframe_pending = true;
		}
		Ok(out)
	}

	fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
		self.drain_tail()
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		// Settable on a live codec and applied without an IDR, which is what the
		// rate control loop wants. The NDK does warn that a parameter change may
		// silently fail to apply, and nothing in the API distinguishes that from
		// one that took, so a device that ignores this reports success.
		let mut params = MediaFormat::new();
		params.set_i32(KEY_VIDEO_BITRATE, clamp_i32(bitrate));
		self.codec
			.set_parameters(params)
			.map_err(|e| codec_err("set the bitrate", e))
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// The `AMediaFormat` describing what we want encoded and how.
fn encoder_format(config: &Config, mime: &str) -> MediaFormat {
	let mut format = MediaFormat::new();
	format.set_str(KEY_MIME, mime);
	format.set_i32(KEY_WIDTH, config.width as i32);
	format.set_i32(KEY_HEIGHT, config.height as i32);
	format.set_i32(KEY_COLOR_FORMAT, COLOR_FORMAT_NV12);
	format.set_i32(KEY_BIT_RATE, clamp_i32(config.resolved_bitrate()));
	format.set_i32(KEY_BITRATE_MODE, BITRATE_MODE_CBR);
	format.set_i32(KEY_FRAME_RATE, config.framerate as i32);
	format.set_i32(KEY_PRIORITY, PRIORITY_REALTIME);
	// MediaCodec takes the keyframe interval in seconds rather than frames, and
	// reads it as a float, so a sub-second GOP survives instead of rounding to
	// zero (which would mean an IDR on every frame).
	format.set_f32(KEY_I_FRAME_INTERVAL, config.gop as f32 / config.framerate.max(1) as f32);
	// Ask for the shortest pipeline the device offers: output a frame after
	// input, and no B-frames, whose reorder delay a live track has no use for.
	// Both are hints an older device drops, which is why `flush` still drains the
	// tail properly rather than assuming there is never one.
	format.set_i32(KEY_LATENCY, 1);
	format.set_i32(KEY_LOW_LATENCY, 1);
	format.set_i32(KEY_MAX_B_FRAMES, 0);

	// State the color space so the encoder writes it into the bitstream's VUI and
	// a decoder doesn't fall back to guessing it from the frame height. BT.601
	// goes out as the NTSC standard (SMPTE 170M primaries and matrix, both code
	// point 6) with the BT.709 transfer curve, which is what every other backend
	// emits: the two curves are defined identically and 709 is the only one all
	// of them can name.
	let color = config.resolved_color();
	let standard = match color {
		Color::Bt601Limited | Color::Bt601Full => COLOR_STANDARD_BT601_NTSC,
		Color::Bt709Limited | Color::Bt709Full => COLOR_STANDARD_BT709,
	};
	format.set_i32(KEY_COLOR_STANDARD, standard);
	format.set_i32(KEY_COLOR_TRANSFER, COLOR_TRANSFER_SDR_VIDEO);
	format.set_i32(
		KEY_COLOR_RANGE,
		if color.limited() {
			COLOR_RANGE_LIMITED
		} else {
			COLOR_RANGE_FULL
		},
	);
	format
}

/// Write `i420` into a MediaCodec input buffer as tightly packed NV12.
///
/// # Errors
///
/// Fails when the codec's buffer is smaller than one picture, which is the
/// device disagreeing with the geometry it was configured with rather than
/// anything a caller can fix.
fn fill_nv12(buffer: &mut [MaybeUninit<u8>], i420: &I420, width: usize, height: usize) -> Result<(), Error> {
	let luma_len = width * height;
	let needed = luma_len + luma_len / 2;
	if buffer.len() < needed {
		return Err(Error::Codec(anyhow::anyhow!(
			"MediaCodec input buffer is {} bytes, needs {needed} for {width}x{height} NV12",
			buffer.len()
		)));
	}

	let (luma, chroma) = buffer[..needed].split_at_mut(luma_len);
	// Y is already the plane MediaCodec wants, byte for byte.
	// SAFETY: `luma` is exactly `luma_len` bytes by the split above, and
	// `i420.y()` is `i420.width() * i420.height()`, which equals `luma_len`
	// because `Encoder::encode` rejects any frame whose size differs from the
	// config this codec was opened with. They are distinct allocations (a codec
	// input buffer and our own frame), and `MaybeUninit<u8>` shares the layout of
	// `u8`, so copying initialized bytes over it is what initializes it.
	unsafe {
		std::ptr::copy_nonoverlapping(i420.y().as_ptr(), luma.as_mut_ptr().cast::<u8>(), luma_len);
	}

	// NV12 interleaves the two chroma planes into one.
	for ((pair, u), v) in chroma.chunks_exact_mut(2).zip(i420.u()).zip(i420.v()) {
		pair[0].write(*u);
		pair[1].write(*v);
	}
	Ok(())
}

/// The valid bytes of an output buffer.
///
/// `BufferInfo` delimits the access unit inside a buffer that is usually larger,
/// and a codec reporting a window outside its own buffer is clamped rather than
/// panicking a slice. An empty one is not read at all: it is a marker rather
/// than a picture (the end-of-stream buffer is one), and asking the NDK for the
/// bytes of a buffer it has none for panics rather than returning an error.
fn access_unit(buffer: &OutputBuffer<'_>, info: &BufferInfo) -> Bytes {
	if info.size() <= 0 {
		return Bytes::new();
	}

	let bytes = buffer.buffer();
	let start = (info.offset().max(0) as usize).min(bytes.len());
	let end = start.saturating_add(info.size().max(0) as usize).min(bytes.len());
	Bytes::copy_from_slice(&bytes[start..end])
}

/// The timestamp of the frame an output belongs to, found by the sample time the
/// codec echoed back and removed from `pending`.
///
/// Falls back to the oldest frame still outstanding when the sample time matches
/// nothing: the encoder emits access units in the order it was fed (low latency,
/// no reordering), so oldest-first is the right pairing, and dropping the packet
/// or stamping it with the wrong frame would both be worse.
fn take_timestamp(
	pending: &mut VecDeque<(i64, Timestamp)>,
	last: &mut Option<Timestamp>,
	sample_time: i64,
) -> Timestamp {
	// `remove` rather than `pop_front`: without reordering these coincide, but a
	// codec that did reorder would otherwise pair every later packet wrongly.
	let matched = match pending.iter().position(|(fed, _)| *fed == sample_time) {
		Some(index) => pending.remove(index),
		None => {
			let oldest = pending.pop_front();
			if oldest.is_some() {
				tracing::debug!(sample_time, "encoder output did not match a fed sample time");
			}
			oldest
		}
	};

	match matched {
		Some((_, timestamp)) => {
			*last = Some(timestamp);
			timestamp
		}
		// Nothing outstanding: the codec produced more access units than it was fed
		// frames, which shouldn't happen. Repeat the last frame's time so the
		// stream keeps flowing in order rather than jumping backwards.
		None => {
			tracing::warn!("encoder produced output with no frame outstanding");
			last.unwrap_or(Timestamp::ZERO)
		}
	}
}

/// A keyframe with the parameter sets in front of it, so a subscriber joining
/// there can decode without having seen the codec-config buffer.
///
/// Left alone when the codec already prepended them itself, which some devices
/// do whether or not they were asked.
fn with_parameter_sets(sets: Option<&Bytes>, codec: Codec, unit: Bytes) -> Bytes {
	let Some(sets) = sets else { return unit };
	if opens_with_parameter_set(&unit, codec) {
		return unit;
	}
	let mut out = BytesMut::with_capacity(sets.len() + unit.len());
	out.put_slice(sets);
	out.put_slice(&unit);
	out.freeze()
}

/// Whether an Annex-B access unit opens with a parameter set: an H.264 SPS
/// (type 7), or an H.265 VPS or SPS (types 32 and 33).
fn opens_with_parameter_set(unit: &[u8], codec: Codec) -> bool {
	let header = match unit {
		[0, 0, 0, 1, header, ..] | [0, 0, 1, header, ..] => *header,
		_ => return false,
	};
	match codec {
		Codec::H265 => matches!((header >> 1) & 0x3f, 32 | 33),
		_ => header & 0x1f == 7,
	}
}

/// Wrap an NDK media error, naming the call that produced it.
fn codec_err(what: &str, error: MediaError) -> Error {
	Error::Codec(anyhow::anyhow!("failed to {what} on the MediaCodec encoder: {error}"))
}

fn clamp_i32(value: u64) -> i32 {
	value.min(i32::MAX as u64) as i32
}

#[cfg(test)]
mod tests {
	use super::*;

	const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1e];
	const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84];

	fn timestamp(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	#[test]
	fn a_keyframe_without_parameter_sets_gets_them() {
		let sets = Bytes::from_static(SPS);
		let unit = Bytes::from_static(IDR);
		let out = with_parameter_sets(Some(&sets), Codec::H264, unit);
		assert_eq!(&out[..SPS.len()], SPS);
		assert_eq!(&out[SPS.len()..], IDR);
	}

	#[test]
	fn a_keyframe_that_already_carries_them_is_left_alone() {
		let sets = Bytes::from_static(SPS);
		let mut unit = Vec::from(SPS);
		unit.extend_from_slice(IDR);
		let unit = Bytes::from(unit);

		let out = with_parameter_sets(Some(&sets), Codec::H264, unit.clone());
		assert_eq!(out, unit);
	}

	#[test]
	fn h265_parameter_sets_are_recognized_by_their_own_header() {
		// A two-byte H.265 header: VPS is type 32, an IDR_W_RADL slice type 19.
		let vps = [0, 0, 0, 1, 0x40, 0x01];
		let idr = [0, 0, 0, 1, 0x26, 0x01];
		assert!(opens_with_parameter_set(&vps, Codec::H265));
		assert!(!opens_with_parameter_set(&idr, Codec::H265));
	}

	#[test]
	fn output_is_paired_with_the_frame_it_was_encoded_from() {
		let mut pending = VecDeque::from(vec![(0, timestamp(1_000)), (33_333, timestamp(34_333))]);
		let mut last = None;

		// The codec is a frame behind, so the first output carries the first
		// frame's sample time while the second frame is already in.
		assert_eq!(take_timestamp(&mut pending, &mut last, 0), timestamp(1_000));
		assert_eq!(take_timestamp(&mut pending, &mut last, 33_333), timestamp(34_333));
		assert!(pending.is_empty());
	}

	#[test]
	fn an_unmatched_sample_time_falls_back_to_the_oldest_frame() {
		let mut pending = VecDeque::from(vec![(0, timestamp(1_000))]);
		let mut last = None;
		assert_eq!(take_timestamp(&mut pending, &mut last, 12_345), timestamp(1_000));

		// Nothing outstanding: repeat rather than jump back to zero.
		assert_eq!(take_timestamp(&mut pending, &mut last, 12_345), timestamp(1_000));
	}

	#[test]
	#[ignore = "needs an Android device with a MediaCodec H.264 encoder"]
	fn encodes_a_keyframe_with_parameter_sets_inline() {
		let config = Config::new(320, 240, 30);
		let mut backend = MediaCodec::open(&config).expect("a MediaCodec encoder");

		let size = config.size();
		let i420 = I420::new(size.width, size.height, vec![0x80; I420::len(size.width, size.height)]).unwrap();
		let frame = Frame::new(crate::Surface::I420(i420), timestamp(0));

		let mut encoded = backend.encode(&frame, true).unwrap();
		if encoded.is_empty() {
			encoded = backend.flush().unwrap();
		}

		let annexb: Vec<u8> = encoded.iter().flat_map(|frame| frame.payload.iter().copied()).collect();
		moq_mux::codec::h264::config(&annexb).expect("a keyframe carrying its own parameter sets");
	}
}

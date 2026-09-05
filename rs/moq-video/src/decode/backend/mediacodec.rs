//! Hardware H.264 / H.265 / AV1 decode backend via Android MediaCodec
//! (`AMediaCodec`).
//!
//! The inverse of the encode MediaCodec backend, and the one place the two
//! differ in shape: the decoder is configured with an `ImageReader`'s surface as
//! its output rather than with CPU output buffers. A decoded picture is then an
//! `AHardwareBuffer` a GL or Vulkan consumer imports directly, which is what
//! [`Surface::HardwareBuffer`] carries, and the read-back to I420 takes the row
//! and pixel strides off the image instead of guessing at the device's padding.
//! The ByteBuffer output path offers neither.
//!
//! Access units arrive Annex-B with the parameter sets inline ahead of each
//! keyframe (the front end converts avc1 / hvc1 for us), which MediaCodec takes
//! as-is, so no `csd-0` / `csd-1` configuration is needed and nothing here parses
//! an avcC record.
//!
//! The codec is fed access units and hands back output buffers in display order.
//! Releasing one for rendering propagates its presentation time to the image in
//! nanoseconds, so each acquired image carries its own timestamp even if the
//! surface drops an excessive frame.
//!
//! Frames hold slots in the reader's queue, so a consumer that hoards them
//! stalls decoding. That is the same trade the VideoToolbox and NVDEC backends
//! make: draw and drop.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bytes::Bytes;
use moq_net::Timestamp;
use ndk::hardware_buffer::HardwareBufferUsage;
use ndk::media::image_reader::{AcquireResult, Image, ImageFormat, ImageReader};
use ndk::media::media_codec::{self, DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodecDirection};
use ndk::media::media_format::MediaFormat;
use ndk::media_error::MediaError;

use super::{Backend, Codec, Config};
use crate::frame::{
	Surface,
	android::{HardwareBuffer, Reader},
};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "mediacodec";

/// The MIME types MediaCodec names the codecs by.
const MIME_H264: &str = "video/avc";
const MIME_H265: &str = "video/hevc";
const MIME_AV1: &str = "video/av01";

// `AMediaFormat` keys. The NDK ships them as `AMEDIAFORMAT_KEY_*` string
// constants that the `ndk` crate does not re-export; they are the same strings
// `android.media.MediaFormat` documents.
const KEY_MIME: &str = "mime";
const KEY_WIDTH: &str = "width";
const KEY_HEIGHT: &str = "height";
const KEY_ALLOW_FRAME_DROP: &str = "allow-frame-drop";
const KEY_LOW_LATENCY: &str = "low-latency";
const KEY_PRIORITY: &str = "priority";

/// `PRIORITY_REALTIME`: this is a live stream, not a file being played back.
const PRIORITY_REALTIME: i32 = 0;

/// `AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG`, likewise not re-exported by the `ndk`
/// crate.
const FLAG_CODEC_CONFIG: u32 = 2;
const FLAG_END_OF_STREAM: u32 = 4;

/// The size the reader's queue is created at.
///
/// Only a default: MediaCodec sets the real geometry on the window once it has
/// parsed the stream, and each image reports what it actually got, which is what
/// a frame's size comes from. Sized for the common case so the queue usually
/// does not have to reallocate.
const DEFAULT_SIZE: (i32, i32) = (1920, 1080);

/// How many decoded pictures the reader's queue holds.
///
/// It has to cover the decoder's reference pictures (4 or so for H.264), the
/// consumer's playout buffer, and whatever is in flight between them; anything
/// less and the decoder stalls waiting for a slot that a consumer holding two
/// frames could have freed.
const QUEUE_DEPTH: i32 = 8;

/// How long to wait for a free input buffer on each round.
const INPUT_TIMEOUT: Duration = Duration::from_millis(10);

/// Poll for decoded pictures without waiting. [`MediaCodec::decode`] drains on
/// both sides of every submission, so a picture that is not ready yet is
/// collected on the next access unit rather than waited for here.
const OUTPUT_TIMEOUT: Duration = Duration::ZERO;

/// How long each round of an end-of-stream drain waits for codec output or a
/// rendered image.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(50);

/// How many rounds a tail drain runs before giving up on a wedged codec.
const DRAIN_ROUNDS: u32 = 20;

/// How many rounds a submission spends freeing an input buffer before giving up.
const SUBMIT_ROUNDS: u32 = 20;

pub(crate) struct MediaCodec {
	/// Declared first so it is dropped first: the codec writes into the reader's
	/// queue, so it has to stop before the reader can go.
	codec: media_codec::MediaCodec,
	/// Shared with every frame handed out, since deleting the reader invalidates
	/// images already acquired from it.
	reader: Arc<Reader>,
	/// Woken by the reader when a rendered output becomes available. Rendering to
	/// a surface is asynchronous with respect to releasing the codec buffer, so
	/// the end-of-stream drain needs this rather than polling or sleeping.
	images: Arc<ImageSignal>,
	/// Last image-listener generation already observed by `collect`.
	image_generation: u64,
	/// Codec output buffers released for rendering but not yet acquired as images.
	pending_images: usize,
	/// Whether this stream has accepted any input and therefore needs EOS.
	fed: bool,
	/// Whether the last acquire found every slot held, so the stall is reported
	/// once on the way in rather than on every poll.
	stalled: bool,
}

#[derive(Default)]
struct ImageSignal {
	generation: Mutex<u64>,
	ready: Condvar,
}

impl ImageSignal {
	fn notify(&self) {
		let mut generation = self.generation.lock().unwrap_or_else(|e| e.into_inner());
		*generation = generation.wrapping_add(1);
		self.ready.notify_one();
	}

	fn current(&self) -> u64 {
		*self.generation.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn wait(&self, generation: u64, timeout: Duration) -> u64 {
		let current = self.generation.lock().unwrap_or_else(|e| e.into_inner());
		let (current, _) = self
			.ready
			.wait_timeout_while(current, timeout, |current| *current == generation)
			.unwrap_or_else(|e| e.into_inner());
		*current
	}
}

// SAFETY: `AMediaCodec` and `AImageReader` are owned handles with no thread
// affinity; the NDK only requires that calls on one of them are serialized,
// which they are because every method here takes `&mut self` and one decode task
// owns the backend outright. `Send` is what lets the boxed trait object satisfy
// `Backend: Send`.
unsafe impl Send for MediaCodec {}

impl MediaCodec {
	/// Open a decoder for `codec`.
	///
	/// `config` is accepted for signature parity: MediaCodec decodes at the
	/// stream's native size and has no scaler to point
	/// [`Config::resize`](crate::decode::Config) at.
	pub(crate) fn open(codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		let mime = match codec {
			Codec::H264 => MIME_H264,
			Codec::H265 => MIME_H265,
			Codec::Av1 => MIME_AV1,
		};

		// `GPU_SAMPLED_IMAGE` is what a consumer importing the buffer as a texture
		// needs; `CPU_READ_OFTEN` is what makes the read-back in
		// `Surface::into_i420` work at all, at the cost of a layout the device may
		// otherwise have tiled.
		let (width, height) = DEFAULT_SIZE;
		let mut reader = ImageReader::new_with_usage(
			width,
			height,
			ImageFormat::YUV_420_888,
			HardwareBufferUsage::GPU_SAMPLED_IMAGE | HardwareBufferUsage::CPU_READ_OFTEN,
			QUEUE_DEPTH,
		)
		.map_err(|e| reader_err("create an ImageReader", e))?;
		let images = Arc::new(ImageSignal::default());
		let ready = images.clone();
		reader
			.set_image_listener(Box::new(move |_| ready.notify()))
			.map_err(|e| reader_err("register the image listener", e))?;
		// `HardwareBuffer::buffer` acquires an extra reference for the caller.
		// Android requires a removal listener whenever that can happen. The owned
		// reference already keeps the allocation alive, so there is no cleanup to
		// perform in the notification itself.
		reader
			.set_buffer_removed_listener(Box::new(|_, _| {}))
			.map_err(|e| reader_err("register the buffer removal listener", e))?;
		let surface = reader.window().map_err(|e| reader_err("get the reader's window", e))?;

		let decoder = media_codec::MediaCodec::from_decoder_type(mime)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("no MediaCodec decoder for {mime}")))?;
		let format = decoder_format(mime, width, height);
		decoder
			.configure(&format, Some(&surface), MediaCodecDirection::Decoder)
			.map_err(|e| codec_err("configure", e))?;
		decoder.start().map_err(|e| codec_err("start", e))?;

		tracing::info!(decoder = NAME, codec = codec.label(), "opened video decoder");
		Ok(Box::new(Self {
			codec: decoder,
			reader: Arc::new(Reader::new(reader)),
			images,
			image_generation: 0,
			pending_images: 0,
			fed: false,
			stalled: false,
		}))
	}

	/// Copy one access unit into a free input buffer.
	///
	/// Waits for a buffer rather than dropping the access unit when the codec has
	/// none free: a decoder that skips one produces a broken picture until the
	/// next keyframe, which is a far worse trade than the wait.
	fn submit(&mut self, access_unit: &[u8], timestamp: Timestamp, out: &mut Vec<Frame>) -> Result<(), Error> {
		let time = timestamp.as_micros().min(u64::MAX as u128) as u64;

		for _ in 0..SUBMIT_ROUNDS {
			let queued = match self
				.codec
				.dequeue_input_buffer(INPUT_TIMEOUT)
				.map_err(|e| codec_err("dequeue an input buffer", e))?
			{
				DequeuedInputBufferResult::Buffer(mut buffer) => {
					let target = buffer.buffer_mut();
					if target.len() < access_unit.len() {
						return Err(Error::Codec(anyhow::anyhow!(
							"MediaCodec input buffer is {} bytes, needs {} for this access unit",
							target.len(),
							access_unit.len()
						)));
					}
					// SAFETY: `target` is at least `access_unit.len()` bytes (checked
					// just above), the two are distinct allocations (a codec input buffer
					// and the caller's payload), and `MaybeUninit<u8>` shares the layout
					// of `u8`, so copying initialized bytes over it is what initializes
					// it.
					unsafe {
						std::ptr::copy_nonoverlapping(
							access_unit.as_ptr(),
							target.as_mut_ptr().cast::<u8>(),
							access_unit.len(),
						);
					}
					self.codec
						.queue_input_buffer(buffer, 0, access_unit.len(), time, 0)
						.map_err(|e| codec_err("queue an input buffer", e))?;
					self.fed = true;
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
			"MediaCodec never freed an input buffer for an access unit"
		)))
	}

	/// Render every decoded picture the codec has ready and collect the images
	/// they turn into.
	fn drain(&mut self, timeout: Duration, out: &mut Vec<Frame>) -> Result<bool, Error> {
		loop {
			let ended = match self
				.codec
				.dequeue_output_buffer(timeout)
				.map_err(|e| codec_err("dequeue an output buffer", e))?
			{
				DequeuedOutputBufferInfoResult::Buffer(buffer) => {
					let info = *buffer.info();
					// Never `OutputBuffer::buffer`: a codec configured with a surface has
					// no CPU-visible output buffer, and asking for one panics. An empty
					// buffer is the codec's own marker rather than a picture, and a
					// codec-config one is the parameter sets echoed back.
					let render = info.flags() & FLAG_CODEC_CONFIG == 0 && info.size() > 0;
					self.codec
						.release_output_buffer(buffer, render)
						.map_err(|e| codec_err("release an output buffer", e))?;
					if render {
						self.pending_images += 1;
					}
					info.flags() & FLAG_END_OF_STREAM != 0
				}
				DequeuedOutputBufferInfoResult::TryAgainLater => {
					self.collect(out)?;
					return Ok(false);
				}
				// The stream's geometry is read off each image rather than off the
				// format, and the parameter sets are inline, so neither of these
				// changes anything here.
				DequeuedOutputBufferInfoResult::OutputFormatChanged
				| DequeuedOutputBufferInfoResult::OutputBuffersChanged => false,
			};

			// Eagerly, because releasing for rendering queues the picture into the
			// reader before it returns, so the image is normally there already and
			// waiting for the next access unit would add a frame of latency.
			self.collect(out)?;
			if ended {
				return Ok(true);
			}
		}
	}

	/// Acquire every rendered picture the reader has ready.
	fn collect(&mut self, out: &mut Vec<Frame>) -> Result<(), Error> {
		loop {
			match self
				.reader
				.acquire_next_image()
				.map_err(|e| reader_err("acquire an image", e))?
			{
				AcquireResult::Image(image) => {
					self.pending_images = self.pending_images.saturating_sub(1);
					// MediaCodec propagates the output buffer's presentation time to a
					// rendered surface in nanoseconds. Read it from the image itself so a
					// surface-dropped frame cannot shift every later timestamp.
					let nanos = image
						.timestamp()
						.map_err(|e| reader_err("read an image's timestamp", e))?;
					let timestamp = Timestamp::from_nanos(nanos.max(0) as u64)?;
					let (left, top, width, height) = image_size(&image)?;
					let buffer = HardwareBuffer::new(self.reader.clone(), image, left, top, width, height);
					out.push(Frame::new(Surface::HardwareBuffer(buffer), timestamp));
				}
				// The picture is on its way but not queued yet, so it comes out on the
				// next round rather than being lost.
				AcquireResult::NoBufferAvailable => {
					self.image_generation = self.images.current();
					self.stalled = false;
					break;
				}
				// Every slot in the queue is held by a consumer, so there is nowhere
				// for this picture to go until one is dropped. Decoding is stalled
				// behind whoever is hoarding frames, which is worth saying out loud.
				AcquireResult::MaxImagesAcquired => {
					// Once, on the way into the stall. `collect` runs two or three
					// times per access unit, so warning on every poll would log at
					// frame rate for as long as the consumer holds its frames.
					if !self.stalled {
						self.stalled = true;
						tracing::warn!(
							decoder = NAME,
							depth = QUEUE_DEPTH,
							"every decoded frame is still held by a consumer; decoding is stalled"
						);
					}
					break;
				}
			}
		}
		Ok(())
	}

	/// Signal end of input and wait until every delayed output buffer is released.
	fn drain_tail(&mut self) -> Result<Vec<Frame>, Error> {
		let mut out = Vec::new();
		if !self.fed {
			return Ok(out);
		}

		self.signal_end_of_input(&mut out)?;
		let mut ended = false;
		for _ in 0..DRAIN_ROUNDS {
			if self.drain(DRAIN_TIMEOUT, &mut out)? {
				ended = true;
				break;
			}
		}
		if !ended {
			return Err(Error::Codec(anyhow::anyhow!(
				"MediaCodec did not reach end of stream within {:?}",
				DRAIN_TIMEOUT * DRAIN_ROUNDS
			)));
		}

		// Releasing an output buffer only schedules the surface render. Wait for the
		// ImageReader callback so the final pictures are part of this stream rather
		// than appearing after the next keyframe.
		for _ in 0..DRAIN_ROUNDS {
			self.collect(&mut out)?;
			if self.pending_images == 0 {
				break;
			}
			self.image_generation = self.images.wait(self.image_generation, DRAIN_TIMEOUT);
		}
		if self.pending_images > 0 {
			tracing::warn!(
				decoder = NAME,
				dropped = self.pending_images,
				"rendered decoder outputs never arrived at the ImageReader"
			);
			self.pending_images = 0;
		}

		Ok(out)
	}

	/// Queue the empty input buffer that marks the end of a ByteBuffer stream.
	fn signal_end_of_input(&mut self, out: &mut Vec<Frame>) -> Result<(), Error> {
		for _ in 0..DRAIN_ROUNDS {
			let queued = match self
				.codec
				.dequeue_input_buffer(INPUT_TIMEOUT)
				.map_err(|e| codec_err("dequeue an input buffer", e))?
			{
				DequeuedInputBufferResult::Buffer(buffer) => {
					self.codec
						.queue_input_buffer(buffer, 0, 0, 0, FLAG_END_OF_STREAM)
						.map_err(|e| codec_err("queue end of stream", e))?;
					true
				}
				DequeuedInputBufferResult::TryAgainLater => false,
			};
			if queued {
				return Ok(());
			}
			self.drain(OUTPUT_TIMEOUT, out)?;
		}

		Err(Error::Codec(anyhow::anyhow!(
			"MediaCodec never freed an input buffer for the end of stream"
		)))
	}
}

impl Backend for MediaCodec {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		let mut out = Vec::new();
		// Collect what the codec finished while the caller was elsewhere, which is
		// also what frees the input buffer the submission below needs.
		self.drain(OUTPUT_TIMEOUT, &mut out)?;
		self.submit(&access_unit, timestamp, &mut out)?;
		self.drain(OUTPUT_TIMEOUT, &mut out)?;
		Ok(out)
	}

	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		let out = self.drain_tail()?;
		if self.fed {
			// EOS leaves the codec refusing input. Synchronous MediaCodec resumes after
			// flush without another start call, and the front end requires the next
			// access unit to be a keyframe after this method returns.
			self.codec.flush().map_err(|e| codec_err("flush", e))?;
			self.fed = false;
		}
		Ok(out)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// The `AMediaFormat` describing the stream to decode.
///
/// The dimensions are a starting guess that the first keyframe's parameter sets
/// correct, since nothing upstream of a backend knows the coded size. The
/// parameter sets ride the bitstream, so there is no `csd-0` / `csd-1` to set.
fn decoder_format(mime: &str, width: i32, height: i32) -> MediaFormat {
	let mut format = MediaFormat::new();
	format.set_str(KEY_MIME, mime);
	format.set_i32(KEY_WIDTH, width);
	format.set_i32(KEY_HEIGHT, height);
	// ImageReader is part of the decoded stream rather than a presentation sink.
	// Keep backpressure explicit through its acquired-image limit instead of
	// letting Android Q+ silently discard a surface frame. Older versions ignore
	// this key, and image timestamps still keep later frames correctly paired.
	format.set_i32(KEY_ALLOW_FRAME_DROP, 0);
	// Ask the decoder to hold as little as it can, which is the difference
	// between a live stream and a file. A hint an older device drops.
	format.set_i32(KEY_LOW_LATENCY, 1);
	format.set_i32(KEY_PRIORITY, PRIORITY_REALTIME);
	format
}

/// The picture's visible size, both dimensions rounded down to even.
///
/// The crop rectangle rather than the buffer dimensions: a coded picture is
/// padded up to a macroblock multiple, so 1080 lines arrive as a 1088-line
/// buffer, and the crop is what says how much of it is the picture. Taken only
/// when the crop is anchored at the origin, since the read-back walks each plane
/// from there; a decoder that offsets its crop gets the whole buffer rather than
/// a picture shifted by the offset.
fn image_size(image: &Image) -> Result<(u32, u32, u32, u32), Error> {
	let width = image.width().map_err(|e| reader_err("read an image's width", e))?;
	let height = image.height().map_err(|e| reader_err("read an image's height", e))?;
	let crop = image.crop_rect().map_err(|e| reader_err("read an image's crop", e))?;

	let (mut left, mut top, mut w, mut h) = (0, 0, width, height);
	let (cropped_w, cropped_h) = (crop.right - crop.left, crop.bottom - crop.top);
	if crop.left >= 0 && crop.top >= 0 && crop.right <= width && crop.bottom <= height && cropped_w > 0 && cropped_h > 0
	{
		(left, top, w, h) = (crop.left, crop.top, cropped_w, cropped_h);
	}

	// 4:2:0 chroma is 2x2, so an odd dimension has no whole chroma sample to go
	// with its last row or column.
	let (w, h) = (w.max(0) as u32 & !1, h.max(0) as u32 & !1);
	if w == 0 || h == 0 {
		return Err(Error::Codec(anyhow::anyhow!(
			"MediaCodec produced a {width}x{height} image, which is not a picture"
		)));
	}
	Ok((left as u32, top as u32, w, h))
}

/// Wrap an NDK media error from the codec, naming the call that produced it.
fn codec_err(what: &str, error: MediaError) -> Error {
	Error::Codec(anyhow::anyhow!("failed to {what} on the MediaCodec decoder: {error}"))
}

/// Wrap an NDK media error from the reader, naming the call that produced it.
fn reader_err(what: &str, error: MediaError) -> Error {
	Error::Codec(anyhow::anyhow!(
		"failed to {what} on the decoder's ImageReader: {error}"
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The hardware round trip: encode a picture with the MediaCodec encoder and
	/// decode it back, which is the only way to see that the surface output path
	/// produces a frame at all.
	#[test]
	#[ignore = "needs an Android device with MediaCodec H.264 hardware"]
	fn decodes_what_the_encoder_produced() {
		use crate::encode::{Config as EncodeConfig, Kind as EncodeKind};

		let size = crate::Size::new(320, 240);
		let mut config = EncodeConfig::new(size.width, size.height, 30);
		config.kind = EncodeKind::Named(NAME.to_owned());
		let mut encoder = crate::encode::Encoder::new(&config).expect("a MediaCodec encoder");

		let i420 = crate::I420::new(
			size.width,
			size.height,
			vec![0x80; crate::I420::len(size.width, size.height)],
		)
		.unwrap();

		let mut decoder = MediaCodec::open(Codec::H264, &Config::new()).expect("a MediaCodec decoder");
		let mut frames = Vec::new();
		for index in 0..30u64 {
			let timestamp = Timestamp::from_micros(index * 33_333).unwrap();
			let frame = Frame::new(Surface::I420(i420.clone()), timestamp);
			encoder.keyframe();
			for encoded in encoder.encode(&frame).unwrap() {
				frames.extend(decoder.decode(encoded.payload, encoded.timestamp, true).unwrap());
			}
		}
		for encoded in encoder.flush().unwrap() {
			frames.extend(decoder.decode(encoded.payload, encoded.timestamp, true).unwrap());
		}
		frames.extend(decoder.flush().unwrap());

		// A flush drains the previous stream and leaves the same decoder reusable.
		let timestamp = Timestamp::from_micros(1_000_000).unwrap();
		let frame = Frame::new(Surface::I420(i420.clone()), timestamp);
		encoder.keyframe();
		for encoded in encoder.encode(&frame).unwrap() {
			frames.extend(decoder.decode(encoded.payload, encoded.timestamp, true).unwrap());
		}
		for encoded in encoder.flush().unwrap() {
			frames.extend(decoder.decode(encoded.payload, encoded.timestamp, true).unwrap());
		}
		frames.extend(decoder.flush().unwrap());

		let frame = frames.first().expect("at least one decoded frame");
		assert_eq!(frame.size(), size);
		assert!(
			matches!(frame.surface, Surface::HardwareBuffer(_)),
			"a decoded picture should stay in its hardware buffer",
		);

		// And it has to survive the read-back, which is the arm every CPU
		// consumer takes and the only exercise `download_i420`'s stride walking
		// gets. Mid-gray in, mid-gray out.
		let frames = frames.into_iter().next().expect("checked above");
		let i420 = frames.surface.into_i420().expect("read back to I420");
		assert_eq!(i420.len(), crate::I420::len(size.width, size.height));
		let luma = &i420[..(size.width * size.height) as usize];
		assert!(
			luma.iter().all(|byte| byte.abs_diff(0x80) <= 8),
			"the read-back luma plane should still be mid-gray",
		);
	}
}

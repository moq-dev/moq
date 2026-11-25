//! Native video encode/decode via [`moq_video`].
//!
//! The video counterpart to [`audio`](crate::audio): publish raw pictures as an
//! encoded video track, and subscribe to one and hand back decoded raw frames,
//! with the codec running inside the FFI boundary (VideoToolbox on macOS, Media
//! Foundation on Windows, NVENC/NVDEC on Linux, openh264 as the software
//! fallback; no ffmpeg). Siblings to `moq_publish_media_*` /
//! `moq_consume_video`, which carry already-encoded frames for a caller that
//! brings its own codec.
//!
//! Decode is H.264 only; a non-H.264 rendition fails the subscribe with a
//! terminal error on the callback. Encode covers H.264 and H.265 (see
//! [`moq_video_codec`]).

use std::ffi::{c_char, c_void};
use std::sync::{Arc, OnceLock};

use tokio::sync::oneshot;

use crate::decode::Read;
use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{
	Error, Id, NonZeroSlab, Shared, State, ffi, moq_decode_gap, moq_decode_read_kind, moq_decode_start, moq_timestamp,
};

// ---- C-visible types ----

/// Pixel layout of the raw frames handed to [`moq_encode_video_frame`].
///
/// The enum is exposed in the C header for readability, but ABI fields that
/// carry it are typed `u32`. A C caller passing an unknown discriminant gets
/// `Error::InvalidCode` instead of UB.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_pixel_format {
	/// Tightly-packed planar I420: Y, then U, then V, no row padding.
	/// `width * height * 3 / 2` bytes, the same layout [`moq_decode_video`]
	/// hands back.
	MOQ_VIDEO_PIXEL_FORMAT_I420 = 0,
	/// Tightly-packed RGBA, `width * height * 4` bytes, no row padding.
	MOQ_VIDEO_PIXEL_FORMAT_RGBA = 1,
}

/// Output video codec for [`moq_encode_video`].
///
/// Not every codec has a backend on every machine: H.265 is hardware-only, so
/// publishing it fails where no hardware encoder is available.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_codec {
	/// H.264 / AVC, published as an `avc3` track.
	MOQ_VIDEO_CODEC_H264 = 0,
	/// H.265 / HEVC, published as a `hev1` track.
	MOQ_VIDEO_CODEC_H265 = 1,
}

/// Which encoder implementation [`moq_encode_video`] should use.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_encoder_kind {
	/// Prefer a platform hardware encoder, falling back to software.
	MOQ_VIDEO_ENCODER_KIND_AUTO = 0,
	/// Hardware only; fails if none is available.
	MOQ_VIDEO_ENCODER_KIND_HARDWARE = 1,
	/// Software only (openh264, H.264 only).
	MOQ_VIDEO_ENCODER_KIND_SOFTWARE = 2,
	/// A specific backend, named by `moq_video_encoder_output::encoder`.
	MOQ_VIDEO_ENCODER_KIND_NAMED = 3,
}

/// An exact video frame rate in frames per second.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct moq_video_frame_rate {
	/// Frames per second numerator; must be nonzero.
	pub numerator: u32,
	/// Frames per second denominator; must be nonzero.
	pub denominator: u32,
}

/// Raw frame layout the caller hands to [`moq_encode_video_frame`], plus
/// the resolution and rate the encoder is opened at. Every published frame must
/// match `width` x `height`; scale before publishing if your source moves.
#[repr(C)]
pub struct moq_video_encoder_input {
	/// `moq_video_pixel_format` discriminant.
	pub format: u32,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base, the default
	/// bitrate, and rounding the join to whole frames. Must be non-zero.
	pub framerate: moq_video_frame_rate,
}

/// Codec-side configuration for [`moq_encode_video`]. Every knob spells
/// "unset" as 0.
#[repr(C)]
pub struct moq_video_encoder_output {
	/// `moq_video_codec` discriminant.
	pub codec: u32,
	/// Target bitrate in bits per second. 0 derives one from the resolution and
	/// framerate.
	pub bitrate: u64,
	/// The time between join points, in microseconds: a keyframe opens a group
	/// this often, rounded to whole frames, so a subscriber joining mid-stream
	/// waits at most this long before it can decode. 0 uses 2 seconds.
	pub join_us: u64,
	/// `moq_video_encoder_kind` discriminant.
	pub kind: u32,
	/// Backend name, UTF-8, e.g. `"videotoolbox"`, `"nvenc"`, `"mediafoundation"`,
	/// `"openh264"`. Read only when `kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
	pub encoder: *const c_char,
	/// The length in bytes of `encoder`.
	pub encoder_len: usize,
}

/// Rate control for [`moq_encode_video`].
///
/// NULL or zeroed holds the configured bitrate regardless of congestion. New fields
/// always append, so a zeroed struct keeps meaning the defaults.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct moq_video_encode_options {
	/// A handle from [`crate::moq_session_allocator`] whose grant the encoder follows,
	/// or 0 to hold the configured bitrate.
	pub bandwidth: u32,
	/// The [`moq_encode_video`] handle of the next lower rendition of the same source,
	/// or 0. It makes this encoder a simulcast rung: while its grant cannot fund it, it
	/// stops encoding (frames published to it are dropped) and advertises itself
	/// disabled, so the lower renditions keep their share, then resumes on a keyframe
	/// once the grant recovers. It takes the track priority one below `lower`'s, so the
	/// allocator fills that one first. It needs a `bandwidth`, and a bitrate above
	/// `lower`'s.
	pub lower: u32,
}

/// One raw frame handed to [`moq_encode_video_frame`].
///
/// Pixel format and resolution are fixed by [`moq_video_encoder_input`] at
/// publish time, so a frame carries neither: `data` is exactly one picture in
/// that layout, borrowed for the duration of the call (the encoder copies before
/// returning). The decode side has its own [`moq_video_frame`], which does carry
/// dimensions, since there they are what the stream turned out to be.
#[repr(C)]
pub struct moq_video_encoder_frame {
	/// The presentation timestamp.
	pub timestamp: moq_timestamp,
	/// The raw frame bytes in the configured layout.
	pub data: *const u8,
	/// The length in bytes of `data`.
	pub data_size: usize,
}

/// Decode-side configuration the caller passes to [`moq_decode_video`].
///
/// `format` selects the CPU pixel layout of each [`moq_video_frame`] (`I420`
/// is `width * height * 3 / 2` bytes, `RGBA` is `width * height * 4` bytes),
/// and `width`/`height` select its size: zero both for the stream's native
/// size, otherwise both must be even and non-zero.
///
/// This struct is versioned by recompilation, not by reserved fields: adding a
/// field changes its layout, so rebuild callers against the `moq.h` that ships
/// with the `libmoq.a` they link.
#[repr(C)]
pub struct moq_video_decoder_output {
	/// How far behind the live edge to play: where a cold join starts and when
	/// a stalled group is skipped. Same knob as `moq_consume_video`'s `delay`.
	/// Zeroed: start at the newest group and skip aggressively (the moq-mux
	/// default); state `min_us` (with `min_stated`) as your playout delay and set
	/// `max_us` at or past it for a softer skip.
	pub delay: crate::api::moq_delay,
	/// `moq_video_pixel_format` discriminant. Unknown values fail
	/// [`moq_decode_video`] rather than decoding into an assumed layout.
	pub format: u32,
	/// Target width in pixels. 0 with `height` 0 means the native size.
	pub width: u32,
	/// Target height in pixels. 0 with `width` 0 means the native size.
	pub height: u32,
	/// `moq_decode_start` discriminant: where to start on a track that already
	/// holds groups. 0 = the oldest, decoding everything cached; a live player
	/// sets `MOQ_DECODE_START_LATEST`.
	pub start: u32,
	/// Decode to the platform's own surface and hand it out as each frame's `surface`,
	/// skipping the CPU conversion: `format`, `width`, and `height` are then ignored and
	/// `data` is NULL. Only Apple platforms have one, an IOSurface-backed NV12
	/// `CVPixelBufferRef` from VideoToolbox, with no software fallback; elsewhere
	/// [`moq_decode_video`] fails with `MOQ_ERROR_UNSUPPORTED`.
	pub surface: bool,
}

/// One decoded video frame from [`moq_decode_video`]: pixels plus a
/// presentation timestamp.
///
/// The pixel layout is what [`moq_video_decoder_output`]'s `format` asked
/// for: I420 is the Y plane (`width * height`), then U, then V (`width/2 *
/// height/2` each), no row padding, BT.601 limited range; RGBA is tightly
/// packed `width * height * 4` bytes, no row padding.
///
/// The read id owns the decoded picture, and the pixels are produced from it
/// on the first [`moq_decode_video_read`] call for that id. `data` stays valid
/// until the same id is released with [`moq_decode_video_read_free`].
///
/// The publish side has its own [`moq_video_encoder_frame`], which carries no
/// dimensions because the encoder already fixed them.
#[repr(C)]
pub struct moq_video_frame {
	/// The presentation timestamp.
	pub timestamp: moq_timestamp,
	/// The decoded width in pixels.
	pub width: u32,
	/// The decoded height in pixels.
	pub height: u32,
	/// The raw frame bytes in the configured layout, or NULL for a `surface` decode.
	pub data: *const u8,
	/// The length in bytes of `data`.
	pub data_size: usize,
	/// The decoder's `CVPixelBufferRef` for a `surface` decode, otherwise NULL. Borrowed: it
	/// belongs to the decoder's pool, valid until the read is freed, so keep the read until
	/// every GPU command reading the surface has completed.
	pub surface: *const c_void,
}

/// One read from [`moq_decode_video`]: a decoded picture, or a break between them.
/// `kind` says which field is filled; the others are zeroed.
#[repr(C)]
pub struct moq_video_read {
	/// Which kind of read this is.
	pub kind: moq_decode_read_kind,
	/// The decoded picture, for [moq_decode_read_kind::MOQ_DECODE_READ_FRAME].
	pub frame: moq_video_frame,
	/// The lost span, for [moq_decode_read_kind::MOQ_DECODE_READ_GAP]. Every picture from
	/// before the loss precedes it, and what follows does not continue what came before.
	pub gap: moq_decode_gap,
	/// Where the stopped encoder's media stops, for
	/// [moq_decode_read_kind::MOQ_DECODE_READ_END]. The stopped encoder's last pictures
	/// precede it; what follows comes from a fresh encoder.
	pub end: moq_timestamp,
}

// ---- State extension (used internally by lib.rs) ----

/// Raw-video state: encoders being published, plus decoder tasks and the
/// reads they delivered.
#[derive(Default)]
pub struct Video {
	producers: NonZeroSlab<Shared<VideoEncoder>>,
	consumer_tasks: NonZeroSlab<Task>,
	reads: NonZeroSlab<Read<Arc<VideoFrame>>>,
}

/// Wait out an encode-thread round trip from a C entry point.
///
/// The C ABI hands back a status code, so there is no executor to yield to and
/// this is where [`Sink`](moq_video::encode::Sink)'s futures stop. Blocking is
/// also what paces the caller: a raw frame is megabytes, so a publish free to run
/// ahead of the codec would queue pictures without bound.
///
/// `pollster` rather than a tokio helper because those panic when the calling
/// thread is driving a runtime, which the one dispatching a callback is.
pub(crate) fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
	pollster::block_on(future)
}

/// A [`Producer`](moq_video::encode::Producer) plus the pixel format its caller feeds it (fixed at
/// publish time, so a frame carries only pixels and a timestamp).
///
/// The producer's encoder is a [`Sink`](moq_video::encode::Sink), which owns the thread it runs
/// on: a C caller drives a handle from whichever thread it likes, so a bare `Encoder` would be
/// built on one thread and dropped on another, unbalancing the per-thread COM apartment the
/// Windows backend opens.
pub(crate) struct VideoEncoder {
	producer: moq_video::encode::Producer,
	format: moq_video_pixel_format,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
	/// Whether it was published against an allocator, so [`moq_encode_video_reservation`]
	/// hands out its claim.
	reserved: bool,
	children: crate::publish::Children,
}

impl crate::publish::Abort for Shared<VideoEncoder> {
	fn abort(self, err: moq_net::Error) {
		// The encoder and the bandwidth claim close with the producer.
		if let Some(encoder) = self.take() {
			encoder.producer.abort(err);
		}
	}
}

/// A delivered frame: the decoded picture, retained until its id is freed, and
/// the packed pixels [`moq_decode_video_read`] produces from it on first use.
///
/// Converting on first use rather than at delivery keeps the work off the
/// delivery task and skips it for a frame the caller frees unread. The pixels
/// are kept once produced, since the C ABI hands out a pointer that must stay
/// put until the id is freed.
struct VideoFrame {
	frame: moq_video::Frame,
	output: DecoderOutput,
	pixels: OnceLock<Pixels>,
}

/// A frame's pixels in the layout and size [`moq_decode_video`] asked for.
struct Pixels {
	width: u32,
	height: u32,
	data: Vec<u8>,
}

impl VideoFrame {
	fn pixels(&self) -> Result<&Pixels, Error> {
		if let Some(pixels) = self.pixels.get() {
			return Ok(pixels);
		}

		// The decoder's scale hint is best effort: a backend without a scaler
		// ignores it, so enforce the requested size here rather than trusting it.
		let resized;
		let frame = match self.output.size {
			Some(size) if self.frame.size() != size => {
				resized = self.frame.resize(size, &moq_video::resize::Config::default())?;
				&resized
			}
			_ => &self.frame,
		};
		let size = frame.size();
		let data = match self.output.format {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => frame.surface.to_i420()?.into_owned().into_data(),
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => frame
				.surface
				.to_rgba(&moq_video::convert::Config::default())?
				.into_data(),
		};

		// Two threads racing on one id both convert; the first result is kept.
		Ok(self.pixels.get_or_init(|| Pixels {
			width: size.width,
			height: size.height,
			data,
		}))
	}
}

/// What [`moq_decode_video`] delivers per frame: the requested CPU pixel format
/// and target size, validated up front so the delivery loop never second-guesses.
#[derive(Clone, Copy)]
pub struct DecoderOutput {
	format: moq_video_pixel_format,
	size: Option<moq_video::Size>,
	/// Hand out the decoder's surface instead of converting to `format`.
	surface: bool,
}

/// Whether this platform has a surface [`moq_video_frame::surface`] can expose.
const HAS_SURFACE: bool = cfg!(any(target_os = "macos", target_os = "ios"));

/// The `CVPixelBufferRef` a frame decoded into, or NULL when it is in CPU memory.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn surface(frame: &moq_video::Frame) -> *const c_void {
	match &frame.surface {
		moq_video::Surface::PixelBuffer(pixels) => std::ptr::from_ref(pixels.buffer()).cast(),
		_ => std::ptr::null(),
	}
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn surface(_frame: &moq_video::Frame) -> *const c_void {
	std::ptr::null()
}

impl VideoEncoder {
	/// Encode and publish one picture, following the bandwidth grant first.
	fn publish_frame(&mut self, timestamp: moq_net::Timestamp, data: &[u8]) -> Result<(), Error> {
		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let size = self.size;
		let surface = match self.format {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => {
				moq_video::Surface::I420(moq_video::I420::new(size, data.to_vec())?)
			}
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => moq_video::Surface::rgba(data, size)?,
		};
		let frame = moq_video::Frame::new(surface, timestamp);
		Ok(block_on(self.producer.write(frame))?)
	}

	fn publish_cut(&mut self) -> Result<(), Error> {
		Ok(block_on(self.producer.cut())?)
	}

	fn publish_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		let bitrate = moq_net::bandwidth::Rate::from_bps(bitrate);
		Ok(block_on(self.producer.set_bitrate(bitrate))?)
	}

	/// Drain and finish the encoder behind handle `id`.
	///
	/// A drain that fails ends the track with an error instead, since a clean finish promises
	/// the subscriber everything the publisher produced.
	fn publish_finish(self, id: Id) -> Result<(), Error> {
		let child = crate::publish::Child::Video(id);
		match block_on(self.producer.finish()) {
			Ok(finished) => {
				// The broadcast keeps the finished track, so its finish still holds it.
				self.children.keep(child, finished);
				Ok(())
			}
			Err(err) => {
				self.children.forget(child);
				Err(err.into())
			}
		}
	}
}

impl Video {
	/// Hand out a handle for a published encoder, so the broadcast's close ends it.
	pub(crate) fn insert(
		&mut self,
		children: &crate::publish::Children,
		producer: moq_video::encode::Producer,
		format: moq_video_pixel_format,
		size: moq_video::Size,
		reserved: bool,
	) -> Result<Id, Error> {
		let encoder = Shared::new(VideoEncoder {
			producer,
			format,
			size,
			reserved,
			children: children.clone(),
		});
		children.insert(&mut self.producers, encoder, crate::publish::Child::Video)
	}

	/// The encoder's place in a simulcast ladder, for the rendition above it.
	pub(crate) fn rung(&self, id: Id) -> Result<moq_video::encode::Rung, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.producer
			.rung())
	}

	pub(crate) fn reservation(&self, id: Id) -> Result<Option<Arc<moq_net::bandwidth::Reservation>>, Error> {
		let shared = self.producer(id)?;
		let guard = shared.lock();
		let encoder = guard.as_ref().ok_or(Error::MediaNotFound)?;
		Ok(encoder.reserved.then(|| encoder.producer.reservation().clone()))
	}

	/// A watch-only handle to the encoded track's subscriber demand.
	pub(crate) fn demand(&self, id: Id) -> Result<moq_net::track::Demand, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.producer
			.demand())
	}

	/// Resolve a producer handle, so the caller can encode with the global lock
	/// released.
	///
	/// Bind the result before locking it: a temporary [`State`] guard lives to the
	/// end of the statement that created it, so resolving and locking in one
	/// expression would put the encode back under the global lock.
	pub(crate) fn producer(&self, id: Id) -> Result<Shared<VideoEncoder>, Error> {
		self.producers.get(id).cloned().ok_or(Error::MediaNotFound)
	}

	/// Resolve a producer and drop its id, so nothing can be published to it after.
	pub(crate) fn remove(&mut self, id: Id) -> Result<Shared<VideoEncoder>, Error> {
		self.producers.remove(id).ok_or(Error::MediaNotFound)
	}

	/// Decode a rendition and deliver read handles to the callback.
	pub fn consume(
		&mut self,
		rendition: crate::consume::Rendition<hang::catalog::VideoConfig>,
		options: moq_video::decode::Options,
		output: DecoderOutput,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let crate::consume::Rendition {
			broadcast,
			origin,
			config: catalog,
			name,
			driver,
		} = rendition;

		// `Consumer::new` subscribes (blocking on SUBSCRIBE_OK), so run it inside
		// the task to keep this entrypoint non-blocking.
		self.consumer_tasks.spawn(
			on_frame,
			|state| &mut state.video.consumer_tasks,
			|mut close| async move {
				driver
					.watch(async move {
						// The rendition may live in a sibling broadcast that is never announced, and
						// SUBSCRIBE_OK may never come, so the close races both steps.
						let setup = async {
							let broadcast = crate::consume::Rendition::resolve(broadcast, origin, &catalog).await?;
							Ok(moq_video::decode::Consumer::new(&broadcast, &catalog, name, options).await?)
						};
						let Some(consumer) = crate::consume::Consume::until_closed(&mut close, setup).await? else {
							return Ok(());
						};
						Self::run(on_frame, consumer, close, output).await
					})
					.await
			},
		)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_video::decode::Consumer,
		mut close: oneshot::Receiver<()>,
		output: DecoderOutput,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready read.
			let read = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				read = consumer.read() => match read? {
					Some(moq_video::decode::Read::Frame(frame)) => Read::Frame(Arc::new(VideoFrame {
						frame,
						output,
						pixels: OnceLock::new(),
					})),
					Some(moq_video::decode::Read::Gap { timestamp, duration }) => Read::gap(timestamp, duration)?,
					Some(moq_video::decode::Read::End { end }) => Read::end(end),
					None => return Ok(()),
				},
			};

			let read_id = State::lock().video.reads.insert(read)?;
			callback.call(Ok(read_id));
		}
	}

	/// Stop decoding and release the consumer handle.
	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		self.consumer_tasks.close(id, Error::TrackNotFound)
	}

	/// Resolve a read handle, so the caller can convert its frame with the global
	/// lock released.
	fn read(&self, id: Id) -> Result<Read<Arc<VideoFrame>>, Error> {
		self.reads.get(id).cloned().ok_or(Error::FrameNotFound)
	}

	/// Release a delivered read handle.
	pub fn read_free(&mut self, id: Id) -> Result<(), Error> {
		self.reads.remove(id).ok_or(Error::FrameNotFound)?;
		Ok(())
	}
}

// ---- C entry points ----

fn pixel_format_from_u32(value: u32) -> Result<moq_video_pixel_format, Error> {
	Ok(match value {
		v if v == moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 as u32 => {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420
		}
		v if v == moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32 => {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA
		}
		_ => return Err(Error::InvalidCode),
	})
}

/// Parse [`moq_video_decoder_output`]'s `width`/`height` into the target size:
/// 0x0 is the stream's native size, anything else must be even and non-zero
/// (I420 chroma is subsampled 2x2) and small enough for the packed byte math.
fn decoder_size(width: u32, height: u32) -> Result<Option<moq_video::Size>, Error> {
	if width == 0 && height == 0 {
		return Ok(None);
	}
	if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
		return Err(Error::InvalidConfig(format!(
			"decode size {width}x{height}: use 0x0 for the native size or even non-zero dimensions"
		)));
	}
	let size = moq_video::Size::new(width, height);
	if size
		.pixels()
		.checked_mul(4)
		.is_none_or(|bytes| usize::try_from(bytes).is_err())
	{
		return Err(Error::InvalidConfig(format!(
			"decode size {width}x{height}: dimensions too large to represent"
		)));
	}
	Ok(Some(size))
}

fn codec_from_u32(value: u32) -> Result<moq_video::encode::Codec, Error> {
	use moq_video::encode::Codec;
	Ok(match value {
		v if v == moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32 => Codec::H264,
		v if v == moq_video_codec::MOQ_VIDEO_CODEC_H265 as u32 => Codec::H265,
		_ => return Err(Error::InvalidCode),
	})
}

/// # Safety
/// - `output->encoder` must point to `output->encoder_len` bytes of UTF-8 when
///   `output->kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
unsafe fn encoder_kind(output: &moq_video_encoder_output) -> Result<moq_video::encode::Kind, Error> {
	use moq_video::encode::Kind;
	Ok(match output.kind {
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_AUTO as u32 => Kind::Auto,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_HARDWARE as u32 => Kind::Hardware,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32 => Kind::Software,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_NAMED as u32 => {
			Kind::Named(unsafe { ffi::parse_str(output.encoder, output.encoder_len)? }.to_string())
		}
		_ => return Err(Error::InvalidCode),
	})
}

/// The pixel layout and encoder configuration `input` and `output` describe.
///
/// # Safety
/// - As [encoder_kind], for `output`.
pub(crate) unsafe fn encoder_config(
	input: &moq_video_encoder_input,
	output: &moq_video_encoder_output,
) -> Result<(moq_video_pixel_format, moq_video::encode::Config), Error> {
	let format = pixel_format_from_u32(input.format)?;
	let framerate = moq_video::FrameRate::new(input.framerate.numerator, input.framerate.denominator)
		.map_err(|err| Error::InvalidConfig(format!("invalid framerate: {err}")))?;
	let mut config = moq_video::encode::Config::new(input.width, input.height, framerate);
	config.codec = codec_from_u32(output.codec)?;
	config.kind = unsafe { encoder_kind(output)? };
	// The C ABI spells an unset knob as 0, which neither field accepts as a real
	// value: a zero bitrate or join is the default, not a request.
	config.bitrate = (output.bitrate != 0).then(|| moq_net::bandwidth::Rate::from_bps(output.bitrate));
	if output.join_us != 0 {
		config.join = std::time::Duration::from_micros(output.join_us);
	}
	Ok((format, config))
}

/// Open a video track on a broadcast, encoding the raw frames you publish to it.
///
/// The encoder is opened here, so an unsupported codec, resolution, or backend
/// fails now rather than on the first frame. The track is named after the codec
/// (`.avc3` / `.hev1`) and its catalog rendition is published immediately, read
/// out of the encoder rather than guessed, so a subscriber can find the track
/// before a frame is written to it.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `input` / `output` must point to fully populated structs.
/// - `output->encoder` must point to `output->encoder_len` bytes of UTF-8 when
///   `output->kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
/// - `options` may be NULL, or must point at a readable [`moq_video_encode_options`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video(
	broadcast: u32,
	input: *const moq_video_encoder_input,
	output: *const moq_video_encoder_output,
	options: *const moq_video_encode_options,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let encode = unsafe { Encode::parse(input, output, options)? };

		// Everything the producer needs from the global state, taken out of it: bringing up a
		// hardware encoder is slow enough that every other call would wait behind it.
		let (encode, producer, catalog, children) = {
			let mut state = State::lock();
			let encode = encode.resolve(&mut state)?;
			let target = state.publish.target(broadcast)?;
			(
				encode,
				target.producer.clone(),
				target.catalog.clone(),
				target.children.clone(),
			)
		};
		let Encode {
			format,
			size,
			options,
			reserved,
			..
		} = encode;
		let encoder = block_on(moq_video::encode::Producer::new(&producer, catalog, options))?;
		State::lock().video.insert(&children, encoder, format, size, reserved)
	})
}

/// Accept a track request as a video track, encoding the raw frames you publish to it.
///
/// Consumes the request handle and returns the same kind of handle as [moq_encode_video], or a
/// negative code on failure. The request names the track. This is how an on-demand publisher
/// serves a track asked for again after its handle was freed while read: accept it with the
/// configuration it was first published with, since its readers hold the track info it set;
/// another ends the track. A configuration this machine can't encode rejects the request with
/// the cause.
///
/// # Safety
/// - As [moq_encode_video], for `input`, `output`, and `options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_track_request_encode_video(
	request: u32,
	input: *const moq_video_encoder_input,
	output: *const moq_video_encoder_output,
	options: *const moq_video_encode_options,
) -> i32 {
	ffi::enter(move || {
		let request = ffi::parse_id(request)?;
		let encode = unsafe { Encode::parse(input, output, options)? };

		// Out of the global state, as moq_encode_video opens its encoder.
		let (encode, requested) = {
			let mut state = State::lock();
			let encode = encode.resolve(&mut state)?;
			(encode, state.publish.track_request_take(request)?)
		};
		let Encode {
			format,
			size,
			options,
			reserved,
			..
		} = encode;
		let crate::publish::Requested {
			request,
			catalog,
			children,
		} = requested;
		let encoder = block_on(moq_video::encode::Producer::requested(request, catalog, options))?;
		State::lock().video.insert(&children, encoder, format, size, reserved)
	})
}

/// A video encode's configuration, parsed from its C structs.
struct Encode {
	format: moq_video_pixel_format,
	size: moq_video::Size,
	options: moq_video::encode::Options,
	/// Whether it reserves against a bandwidth allocator.
	reserved: bool,
	bandwidth: Option<Id>,
	lower: Option<Id>,
}

impl Encode {
	/// # Safety
	/// - As [moq_encode_video], for `input`, `output`, and `options`.
	unsafe fn parse(
		input: *const moq_video_encoder_input,
		output: *const moq_video_encoder_output,
		options: *const moq_video_encode_options,
	) -> Result<Self, Error> {
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let options = unsafe { options.as_ref() }.copied().unwrap_or_default();
		let bandwidth = ffi::parse_id_optional(options.bandwidth)?;
		let lower = ffi::parse_id_optional(options.lower)?;
		if lower.is_some() && bandwidth.is_none() {
			return Err(Error::InvalidConfig(
				"a lower rendition needs a bandwidth allocator to follow".into(),
			));
		}

		let (format, config) = unsafe { encoder_config(raw_input, raw_output)? };
		Ok(Self {
			format,
			size: config.size(),
			options: moq_video::encode::Options::new(config),
			reserved: bandwidth.is_some(),
			bandwidth,
			lower,
		})
	}

	/// Look up the allocator and lower rendition it names.
	fn resolve(mut self, state: &mut State) -> Result<Self, Error> {
		if let Some(bandwidth) = self.bandwidth {
			self.options.bandwidth = state.bandwidth.allocator(bandwidth)?;
		}
		if let Some(lower) = self.lower {
			self.options.lower = Some(state.video.rung(lower)?);
		}
		Ok(self)
	}
}

/// This encoder's bandwidth reservation, or 0 if it was published without one.
///
/// The handle views the encoder's claim: closing it does not release the claim, and
/// keeping it does not hold it past [moq_encode_video_finish], after which it reads no rate.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_reservation(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let mut state = State::lock();
		match state.video.reservation(producer)? {
			Some(reservation) => Ok(i32::from(state.bandwidth.borrow(&reservation)?)),
			None => Ok(0),
		}
	})
}

/// Watch whether the encoded video track has subscribers. The encoder publishes every
/// frame written, subscribed or not; opting in to run the camera and encoder only
/// while someone watches saves that work but leaves a late subscriber nothing to
/// fetch from before it arrived. See [`crate::moq_publish_media_demand`] for the
/// callback contract.
///
/// Returns a non-zero watcher handle on success, or a negative code on failure.
///
/// # Safety
/// - `on_demand` must be non-NULL.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_demand` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video_demand(
	producer: u32,
	on_demand: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let on_demand = unsafe { OnStatus::new(user_data, on_demand)? };
		let mut state = State::lock();
		let demand = state.video.demand(producer)?;
		state.publish.demand(demand, on_demand)
	})
}

/// Encode and publish one raw frame.
///
/// `frame->data` is borrowed for the duration of the call and must be exactly one
/// picture in the pixel format and at the resolution declared by
/// [`moq_video_encoder_input`].
/// A backend that pipelines publishes an earlier frame's output here, so a call
/// that emits nothing is normal rather than an error. While a rung published with a
/// `lower` rendition is disabled for lack of bandwidth, the frame is dropped
/// without encoding.
///
/// Published against a bandwidth allocator, the encoder follows its grant here,
/// before encoding, so it retunes between frames rather than mid-encode.
///
/// # Safety
/// - `frame` must point to a valid [`moq_video_encoder_frame`].
/// - `frame->data` must point to `frame->data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video_frame(producer: u32, frame: *const moq_video_encoder_frame) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };

		let producer = State::lock().video.producer(producer)?;
		let mut guard = producer.lock();
		guard
			.as_mut()
			.ok_or(Error::MediaNotFound)?
			.publish_frame(frame.timestamp.try_into()?, data)
	})
}

/// Cut a new group at the next published frame.
///
/// Optional. The encoder already keyframes every `moq_video_encoder_output.join_us`,
/// and each of those cuts a group, so a subscriber can always join
/// without you calling this. Reach for it only to place the boundaries yourself:
/// aligning groups with something the encoder cannot see, such as a scene change,
/// a source switch, or resuming after an idle gap.
///
/// The next frame is encoded as a keyframe, which closes the open group and
/// starts a new one at it. Calling this repeatedly before that frame arrives cuts
/// once, not several times.
///
/// Fails when the selected encoder cannot force a keyframe (a V4L2 driver
/// without the control): nothing is queued, and groups keep falling every
/// `join_us`.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_cut(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let producer = State::lock().video.producer(producer)?;
		producer.lock().as_mut().ok_or(Error::MediaNotFound)?.publish_cut()
	})
}

/// Retune a live encoder to `bitrate` bits per second, taking effect from
/// roughly the next frame. No keyframe is forced, so this is cheap enough to
/// drive from a congestion controller.
///
/// The configured bitrate is a ceiling on some backends (openh264 rejects a raise
/// above the rate it opened at), so set `bitrate` to the highest you will ask
/// for and adapt downwards from there.
///
/// When this encoder was published against a bandwidth allocator, the reservation
/// and the grant's ceiling move with it, so a later grant cannot retune above this
/// value.
///
/// Returns a negative code if this backend cannot retune while running. That is
/// not fatal: the encoder keeps running at its current rate, so stop adapting
/// rather than stop publishing.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_bitrate(producer: u32, bitrate: u64) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let producer = State::lock().video.producer(producer)?;
		producer
			.lock()
			.as_mut()
			.ok_or(Error::MediaNotFound)?
			.publish_bitrate(bitrate)
	})
}

/// Flush any frames the codec is still holding and finalize the video track.
///
/// The handle is released, so nothing can be published to it afterwards.
/// The broadcast keeps the finished track, catalog rendition included, until it closes, so a
/// late subscriber can still drain it.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_finish(producer: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(producer)?;
		// The id stays until the drain ends, so a racing moq_publish_close waits it out
		// instead of missing a track that still publishes. A call queued behind the drain
		// finds the encoder gone.
		let shared = State::lock().video.producer(id)?;
		let finished = {
			let mut guard = shared.lock();
			guard.take().ok_or(Error::MediaNotFound)?.publish_finish(id)
		};
		// A racing close may have released the id already.
		let _ = State::lock().video.remove(id);
		finished
	})
}

/// Release a video producer without draining or finishing it, along with its encoder and
/// bandwidth claim, dropping the frames the codec still holds; see
/// [`crate::moq_publish_track_free`] for how its track ends.
///
/// Returns zero, or a negative code if the handle is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_free(producer: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(producer)?;
		let shared = State::lock().video.producer(id)?;
		let released = shared.lock().take().ok_or(Error::MediaNotFound)?;
		released.children.forget(crate::publish::Child::Video(id));
		// A racing close may have released the id already.
		let _ = State::lock().video.remove(id);
		drop(released);
		Ok(())
	})
}

/// Subscribe to a video track and decode it into raw frames in the requested
/// CPU pixel format and size (see [`moq_video_decoder_output`]).
///
/// The catalog `index` selects which video rendition to subscribe to, matching
/// the existing `moq_consume_video` selection model. A codec or description this build
/// cannot decode, or a stream every decoder's device refuses, reports
/// [`MOQ_ERROR_UNSUPPORTED_CONFIG`](crate::MOQ_ERROR_UNSUPPORTED_CONFIG) on the terminal
/// callback, before any decoded reads.
///
/// An unknown `output->format` or an invalid `output->width`/`height` fails
/// here, before subscribing. Each frame is produced in the requested layout by
/// [`moq_decode_video_read`], which fails for that frame rather than deliver
/// another layout.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_read` is called with a positive read id per decoded frame, gap, or encoder
/// end (see [`moq_decode_video_read`]), then exactly once more with a terminal
/// code: `0` (closed cleanly) or a negative error. After the terminal (`<= 0`)
/// callback, `on_read` is never called again and `user_data` is never touched
/// again, so release `user_data` there. The terminal callback fires even after
/// [`moq_decode_video_cancel`].
///
/// `output->start` picks where it starts on a track that already holds groups.
///
/// # Safety
/// - `output` must point to a valid [`moq_video_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_read` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_video(
	catalog: u32,
	index: u32,
	output: *const moq_video_decoder_output,
	on_read: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		// Validate the request before resolving anything: a C caller can probe
		// support without a live track, and a bad request never opens a
		// subscription it would immediately drop.
		if raw.surface && !HAS_SURFACE {
			return Err(Error::Unsupported("no decoder surface on this platform".into()));
		}
		// A surface decode converts nothing, so its CPU layout and size go unread.
		let (format, size) = match raw.surface {
			true => (moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420, None),
			false => (pixel_format_from_u32(raw.format)?, decoder_size(raw.width, raw.height)?),
		};
		let start = match moq_decode_start::from_u32(raw.start)? {
			moq_decode_start::MOQ_DECODE_START_OLDEST => moq_video::decode::Start::Oldest,
			moq_decode_start::MOQ_DECODE_START_LATEST => moq_video::decode::Start::Latest,
		};
		let catalog = ffi::parse_id(catalog)?;

		let mut options = moq_video::decode::Options::new();
		options.start = start;
		options.delay = raw.delay.into();
		// A caller taking packed pixels lets a backend that can decode straight to the CPU do
		// that rather than downloading afterwards.
		options.decoder.output = match raw.surface {
			false => moq_video::Output::Cpu,
			true => moq_video::Output::Native,
		};
		// Only VideoToolbox decodes into a `CVPixelBuffer`: a software fallback would hand out
		// CPU pixels with neither `data` nor `surface` to read them by, so fail instead.
		if raw.surface {
			options.decoder.kind = moq_video::decode::Kind::Named("videotoolbox".into());
		}
		// A backend with a hardware scaler (NVDEC) honors this for free; the
		// delivery loop still enforces it, since other backends ignore it.
		options.decoder.scale_hint = size;
		let output = DecoderOutput {
			format,
			size,
			surface: raw.surface,
		};
		let on_read = unsafe { OnStatus::new(user_data, on_read)? };

		let mut state = State::lock();
		let rendition = state.consume.rendition(catalog, index as usize)?;

		let State { video, .. } = &mut *state;
		video.consume(rendition, options, output, on_read)
	})
}

/// Stop a video (raw) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-read callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Read ids already delivered are likewise not freed: each stays
/// readable after the terminal callback until released with
/// [`moq_decode_video_read_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_video_cancel(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().video.consume_close(consumer)
	})
}

/// Write a delivered read into `dst`: its kind, and the frame's pixels, the gap,
/// or the end it carries.
///
/// The first call for a frame's `id` converts the decoded picture to the layout
/// and size [`moq_decode_video`] asked for; later calls return the same pixels. A
/// conversion failure returns a negative code for this frame only, leaving the
/// id to be freed as usual. Safe to call from any thread, including inside the
/// read callback.
///
/// The written `dst->frame.data` pointer remains valid until the same `id` is
/// released with [`moq_decode_video_read_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_video_read`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_video_read(id: u32, dst: *mut moq_video_read) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		let read = State::lock().video.read(id)?;
		let (kind, gap, end) = read.info();
		let frame = match &read {
			Read::Frame(frame) if frame.output.surface => {
				let size = frame.frame.size();
				moq_video_frame {
					timestamp: frame.frame.timestamp.into(),
					width: size.width,
					height: size.height,
					data: std::ptr::null(),
					data_size: 0,
					// The slab keeps the frame, and with it the surface, until the id is freed.
					surface: surface(&frame.frame),
				}
			}
			Read::Frame(frame) => {
				let pixels = frame.pixels()?;
				moq_video_frame {
					timestamp: frame.frame.timestamp.into(),
					width: pixels.width,
					height: pixels.height,
					// The slab keeps its `Arc` until the id is freed, and the pixels are
					// never replaced once set, so the pointer outlives this call.
					data: pixels.data.as_ptr(),
					data_size: pixels.data.len(),
					surface: std::ptr::null(),
				}
			}
			_ => moq_video_frame {
				timestamp: moq_timestamp::default(),
				width: 0,
				height: 0,
				data: std::ptr::null(),
				data_size: 0,
				surface: std::ptr::null(),
			},
		};
		*dst = moq_video_read { kind, frame, gap, end };
		Ok(())
	})
}

/// Free a read previously delivered through the decode callback, releasing a
/// frame's decoded picture back to the decoder. Required for every delivered read
/// id; closing the parent consumer is not enough, and a decoder whose frames are
/// all held stalls until some are freed.
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_video_read_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().video.read_free(id)
	})
}

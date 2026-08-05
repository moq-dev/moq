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
use std::time::Duration;

use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, ffi};

// ---- C-visible types ----

/// Pixel layout of the raw frames handed to [`moq_publish_video_raw_frame`].
///
/// The enum is exposed in the C header for readability, but ABI fields that
/// carry it are typed `u32`. A C caller passing an unknown discriminant gets
/// `Error::InvalidCode` instead of UB.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_pixel_format {
	/// Tightly-packed planar I420: Y, then U, then V, no row padding.
	/// `width * height * 3 / 2` bytes, the same layout [`moq_consume_video_raw`]
	/// hands back.
	MOQ_VIDEO_PIXEL_FORMAT_I420 = 0,
	/// Tightly-packed RGBA, `width * height * 4` bytes, no row padding.
	MOQ_VIDEO_PIXEL_FORMAT_RGBA = 1,
}

/// Output video codec for [`moq_publish_video_raw`].
///
/// Not every codec has a backend on every machine: H.265 is hardware-only, so
/// publishing it fails where no hardware encoder is available.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_codec {
	/// H.264 / AVC, published as an `avc3` track.
	MOQ_VIDEO_CODEC_H264 = 0,
	/// H.265 / HEVC, published as a `hev1` track.
	MOQ_VIDEO_CODEC_H265 = 1,
}

/// Which encoder implementation [`moq_publish_video_raw`] should use.
#[repr(C)]
#[allow(non_camel_case_types)]
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

/// Raw frame layout the caller hands to [`moq_publish_video_raw_frame`], plus
/// the resolution and rate the encoder is opened at. Every published frame must
/// match `width` x `height`; scale before publishing if your source moves.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_encoder_input {
	/// `moq_video_pixel_format` discriminant.
	pub format: u32,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base and the default
	/// bitrate and keyframe interval. Must be non-zero.
	pub framerate: u32,
}

/// Codec-side configuration for [`moq_publish_video_raw`]. Every knob spells
/// "unset" as 0.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_encoder_output {
	/// `moq_video_codec` discriminant.
	pub codec: u32,
	/// Target bitrate in bits per second. 0 derives one from the resolution and
	/// framerate.
	pub bitrate: u64,
	/// Keyframe interval in frames: a subscriber joining mid-stream waits at
	/// most this many frames before it can decode. 0 uses ~2 seconds.
	pub gop: u32,
	/// `moq_video_encoder_kind` discriminant.
	pub kind: u32,
	/// Backend name, UTF-8, e.g. `"videotoolbox"`, `"nvenc"`, `"mediafoundation"`,
	/// `"openh264"`. Read only when `kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
	pub encoder: *const c_char,
	pub encoder_len: usize,
}

/// One raw frame handed to [`moq_publish_video_raw_frame`].
///
/// Pixel format and resolution are fixed by [`moq_video_encoder_input`] at
/// publish time, so a frame carries neither: `data` is exactly one picture in
/// that layout, borrowed for the duration of the call (the encoder copies before
/// returning). The decode side has its own [`moq_video_frame`], which does carry
/// dimensions, since there they are what the stream turned out to be.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_encoder_frame {
	/// Presentation timestamp, in microseconds.
	pub timestamp_us: u64,
	pub data: *const u8,
	pub data_size: usize,
}

/// Decode-side configuration the caller passes to [`moq_consume_video_raw`].
///
/// Output is always tightly-packed I420 (see [`moq_video_frame`]); there is no
/// format/resolution knob yet. The struct exists so future options (a pixel
/// format, a target size) stay additive.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_decoder_output {
	/// Upper bound on buffering before skipping a stalled group, in
	/// milliseconds. Same congestion-control knob as
	/// `moq_consume_video`'s `max_latency_ms`. 0 = skip aggressively
	/// (the moq-mux default); set to your playout buffer for a softer skip.
	pub latency_max_ms: u64,
}

/// One decoded video frame from [`moq_consume_video_raw`]: packed I420 plus a
/// presentation timestamp.
///
/// `data` is the Y plane (`width * height`), then U, then V (`width/2 *
/// height/2` each), no row padding, BT.601 limited range, with `width` and
/// `height` even. It's owned by the consume slab and stays valid until the same
/// id is released with [`moq_consume_video_raw_frame_free`].
///
/// The publish side has its own [`moq_video_encoder_frame`], which carries no
/// dimensions because the encoder already fixed them.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_frame {
	pub timestamp_us: u64,
	pub width: u32,
	pub height: u32,
	pub data: *const u8,
	pub data_size: usize,
}

// ---- State extension (used internally by lib.rs) ----

/// Raw-video state: encoders being published, plus decoder tasks and their
/// buffered decoded frames.
#[derive(Default)]
pub struct Video {
	producers: NonZeroSlab<VideoEncoder>,
	consumer_tasks: NonZeroSlab<Option<VideoTaskEntry>>,
	frames: NonZeroSlab<VideoFrame>,
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
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
	pollster::block_on(future)
}

/// An encoder paired with the track publishing its output, plus the pixel format
/// its caller feeds it (fixed at publish time, so a frame carries only pixels and
/// a timestamp).
///
/// The encoder is a [`Sink`](moq_video::encode::Sink) rather than a bare
/// `Encoder` because [`ffi::enter`] serializes calls without confining them:
/// each one runs on whichever thread the C caller used, so a bare `Encoder`
/// would be built on one thread and dropped on another, unbalancing the
/// per-thread COM apartment the Windows backend opens. The sink owns the thread
/// instead, so every caller is welcome.
struct VideoEncoder {
	encoder: moq_video::encode::Sink,
	producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	format: moq_video_pixel_format,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
}

/// A delivered frame, flattened to CPU I420 at delivery time: the C ABI hands
/// out a stable byte pointer, so a GPU-decoded frame (e.g. NVDEC) is downloaded
/// exactly once here.
struct VideoFrame {
	timestamp_us: u64,
	width: u32,
	height: u32,
	data: bytes::Bytes,
}

/// End a video track, given the result of draining its encoder into it.
///
/// A clean finish is a promise that the track holds everything the publisher
/// produced, so a lost tail has to end the track as an abort instead. Finishing
/// anyway would leave a truncated stream indistinguishable from a complete one,
/// and only the local caller would ever learn otherwise.
fn finalize(
	producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	drained: Result<(), moq_video::Error>,
) -> Result<(), Error> {
	match drained {
		Ok(()) => Ok(producer.finish()?),
		Err(err) => {
			producer.abort(moq_net::Error::Transport(err.to_string()));
			Err(err.into())
		}
	}
}

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// Same lifetime contract as the audio decoder: the task delivers one final
/// terminal callback and then removes itself, so `user_data` stays valid until
/// that callback fires. `close` is an `Option` so `consume_close` can drop just
/// the sender without removing the entry.
struct VideoTaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

impl Video {
	pub fn publish(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<moq_mux::catalog::hang::Extra>,
		format: moq_video_pixel_format,
		config: moq_video::encode::Config,
	) -> Result<Id, Error> {
		// Open the encoder first: a config this machine can't encode should fail
		// without leaving a track advertised that will never carry frames.
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;
		let producer = moq_video::encode::Producer::new(broadcast.clone(), catalog, config.codec)?;
		self.producers.insert(VideoEncoder {
			encoder,
			producer,
			format,
			size: config.size(),
		})
	}

	pub fn publish_frame(&mut self, id: Id, timestamp_us: u64, data: &[u8]) -> Result<(), Error> {
		let entry = self.producers.get_mut(id).ok_or(Error::MediaNotFound)?;

		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let size = entry.size;
		let surface = match entry.format {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => {
				moq_video::Surface::I420(moq_video::I420::new(size.width, size.height, data.to_vec())?)
			}
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => moq_video::Surface::rgba(data, size)?,
		};

		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(timestamp_us)?);
		// A backend that pipelines hands back an earlier frame's output, so this is
		// zero or more access units rather than one per call.
		let encoded = block_on(entry.encoder.encode(frame))?;
		entry.producer.publish(&encoded)?;
		Ok(())
	}

	pub fn publish_cut(&mut self, id: Id) -> Result<(), Error> {
		let entry = self.producers.get_mut(id).ok_or(Error::MediaNotFound)?;
		// A keyframe is what a cut is on the wire: the importer closes the open
		// group and starts a new one at it.
		entry.encoder.keyframe();
		Ok(())
	}

	pub fn publish_bitrate(&mut self, id: Id, bitrate: u64) -> Result<(), Error> {
		let entry = self.producers.get_mut(id).ok_or(Error::MediaNotFound)?;
		block_on(entry.encoder.set_bitrate(bitrate))?;
		Ok(())
	}

	pub fn publish_finish(&mut self, id: Id) -> Result<(), Error> {
		let entry = self.producers.remove(id).ok_or(Error::MediaNotFound)?;
		let VideoEncoder {
			encoder, mut producer, ..
		} = entry;
		// Drain the codec into the track before ending it, so the last frames land
		// in it rather than being dropped with the encoder.
		let drained = block_on(encoder.finish()).and_then(|encoded| producer.publish(&encoded));
		finalize(producer, drained)
	}

	pub fn consume(
		&mut self,
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &hang::catalog::VideoConfig,
		name: &str,
		config: moq_video::decode::Config,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let broadcast = broadcast.clone();
		let catalog = catalog.clone();
		let name = name.to_string();

		let channel = oneshot::channel();
		let entry = VideoTaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.consumer_tasks.insert(Some(entry))?;

		// `Consumer::new` subscribes (blocking on SUBSCRIBE_OK), so run it inside
		// the task to keep this entrypoint non-blocking.
		tokio::spawn(async move {
			let res = async move {
				let consumer = moq_video::decode::Consumer::new(&broadcast, &catalog, name, config).await?;
				Self::run(on_frame, consumer, channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().video.consumer_tasks.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_video::decode::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready frame.
			let frame = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				frame = consumer.read() => match frame? {
					Some(frame) => frame,
					None => return Ok(()),
				},
			};

			// Flatten to CPU bytes outside the lock (a GPU frame downloads here),
			// then hold the lock only to buffer it; release before the callback.
			let size = frame.size();
			let frame = VideoFrame {
				// The C ABI carries microseconds; the decoded frame's Timestamp is
				// constrained to a QUIC VarInt, so the microsecond value fits a u64.
				timestamp_us: frame.timestamp.as_micros() as u64,
				width: size.width,
				height: size.height,
				data: frame.surface.into_i420()?,
			};
			let frame_id = State::lock().video.frames.insert(frame)?;
			callback.call(Ok(frame_id));
		}
	}

	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.consumer_tasks
			.get_mut(id)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?
			.close
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	pub fn frame_info(&self, id: Id, dst: &mut moq_video_frame) -> Result<(), Error> {
		let frame = self.frames.get(id).ok_or(Error::FrameNotFound)?;
		*dst = moq_video_frame {
			timestamp_us: frame.timestamp_us,
			width: frame.width,
			height: frame.height,
			data: frame.data.as_ptr(),
			data_size: frame.data.len(),
		};
		Ok(())
	}

	pub fn frame_free(&mut self, id: Id) -> Result<(), Error> {
		self.frames.remove(id).ok_or(Error::FrameNotFound)?;
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

/// Open a video track on a broadcast, encoding the raw frames you publish to it.
///
/// The encoder is opened here, so an unsupported codec, resolution, or backend
/// fails now rather than on the first frame. The track is named after the codec
/// (`.avc3` / `.hev1`) and its catalog rendition appears once the first keyframe
/// has been encoded, which is where the resolution and codec string come from.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `input` / `output` must point to fully populated structs.
/// - `output->encoder` must point to `output->encoder_len` bytes of UTF-8 when
///   `output->kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_raw(
	broadcast: u32,
	input: *const moq_video_encoder_input,
	output: *const moq_video_encoder_output,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		let format = pixel_format_from_u32(raw_input.format)?;

		let mut config = moq_video::encode::Config::new(raw_input.width, raw_input.height, raw_input.framerate);
		config.codec = codec_from_u32(raw_output.codec)?;
		config.kind = unsafe { encoder_kind(raw_output)? };
		// The C ABI spells an unset knob as 0, which neither field accepts as a real
		// value: a zero bitrate or GOP is the default, not a request.
		config.bitrate = (raw_output.bitrate != 0).then_some(raw_output.bitrate);
		if raw_output.gop != 0 {
			config.gop = raw_output.gop;
		}

		let mut state = State::lock();
		let State { publish, video, .. } = &mut *state;
		let (broadcast_producer, catalog) = publish.pair_mut(broadcast)?;

		video.publish(broadcast_producer, catalog.clone(), format, config)
	})
}

/// Encode and publish one raw frame.
///
/// `frame->data` is borrowed for the duration of the call and must be exactly one
/// picture in the pixel format and at the resolution declared by
/// [`moq_video_encoder_input`].
/// A backend that pipelines publishes an earlier frame's output here, so a call
/// that emits nothing is normal rather than an error.
///
/// # Safety
/// - `frame` must point to a valid [`moq_video_encoder_frame`].
/// - `frame->data` must point to `frame->data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_raw_frame(producer: u32, frame: *const moq_video_encoder_frame) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };

		State::lock().video.publish_frame(producer, frame.timestamp_us, data)
	})
}

/// Cut a new group at the next published frame.
///
/// Optional. The encoder already keyframes every `moq_video_encoder_output.gop`
/// frames, and each of those cuts a group, so a subscriber can always join
/// without you calling this. Reach for it only to place the boundaries yourself:
/// aligning groups with something the encoder cannot see, such as a scene change,
/// a source switch, or resuming after an idle gap.
///
/// The next frame is encoded as a keyframe, which closes the open group and
/// starts a new one at it. Calling this repeatedly before that frame arrives cuts
/// once, not several times.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_video_raw_cut(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		State::lock().video.publish_cut(producer)
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
/// Returns a negative code if this backend cannot retune while running. That is
/// not fatal: the encoder keeps running at its current rate, so stop adapting
/// rather than stop publishing.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_video_raw_bitrate(producer: u32, bitrate: u64) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		State::lock().video.publish_bitrate(producer, bitrate)
	})
}

/// Flush any frames the codec is still holding and finalize the video track.
///
/// The handle is released, so nothing can be published to it afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_video_raw_finish(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		State::lock().video.publish_finish(producer)
	})
}

/// Subscribe to a video track and decode it into raw I420 frames.
///
/// The catalog `index` selects which video rendition to subscribe to, matching
/// the existing `moq_consume_video` selection model. Only H.264 is
/// supported; a non-H.264 rendition fails on the terminal callback.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_frame` is called with a positive frame id per decoded frame, then exactly
/// once more with a terminal code: `0` (closed cleanly) or a negative error.
/// After the terminal (`<= 0`) callback, `on_frame` is never called again and
/// `user_data` is never touched again, so release `user_data` there. The terminal
/// callback fires even after [`moq_consume_video_raw_close`].
///
/// # Safety
/// - `output` must point to a valid [`moq_video_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_raw(
	catalog: u32,
	index: u32,
	output: *const moq_video_decoder_output,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		let mut config = moq_video::decode::Config::new();
		config.latency_max = if raw.latency_max_ms == 0 {
			None
		} else {
			Some(Duration::from_millis(raw.latency_max_ms))
		};
		let on_frame = unsafe { OnStatus::new(user_data, on_frame) };

		let mut state = State::lock();
		let (broadcast, video_cfg, name) = state.consume.video_rendition(catalog, index as usize)?;

		let State { video, .. } = &mut *state;
		video.consume(&broadcast, &video_cfg, &name, config, on_frame)
	})
}

/// Stop a video (raw) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-frame callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Frame ids already delivered are likewise not freed; release each
/// with [`moq_consume_video_raw_frame_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_raw_close(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().video.consume_close(consumer)
	})
}

/// Copy a delivered frame's metadata into `dst`.
///
/// The written `dst->data` pointer remains valid until the same `id` is released
/// with [`moq_consume_video_raw_frame_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_video_frame`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_raw_frame(id: u32, dst: *mut moq_video_frame) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().video.frame_info(id, dst)
	})
}

/// Free a frame previously delivered through the consume callback. Required for
/// every delivered frame id; closing the parent consumer is not enough.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_raw_frame_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().video.frame_free(id)
	})
}
#[cfg(test)]
mod tests {
	use super::*;

	/// A video track wired up without an encoder, plus a subscriber on it: enough
	/// to pin what [`finalize`] shows the far end.
	async fn track_under_test() -> (
		moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
		moq_net::track::Subscriber,
	) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, moq_mux::catalog::hang::Catalog::default())
				.unwrap();
		let consumer = broadcast.consume();
		let producer = moq_video::encode::Producer::new(broadcast, catalog, moq_video::encode::Codec::H264).unwrap();

		let name = producer.demand().name().to_string();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		(producer, track)
	}

	/// A clean finish reaches the subscriber as the end of the track, which is what
	/// makes the abort case below meaningful rather than vacuous.
	#[tokio::test]
	async fn a_successful_drain_ends_the_track_cleanly() {
		let (producer, mut track) = track_under_test().await;
		finalize(producer, Ok(())).unwrap();
		assert!(matches!(track.recv_group().await, Ok(None)), "expected a clean end");
	}

	/// Regression: a lost tail must reach the subscriber as an abort. Finishing the
	/// track anyway would report a truncated stream as a complete one, and only the
	/// publisher would ever know otherwise.
	#[tokio::test]
	async fn a_failed_drain_aborts_the_track() {
		let (producer, mut track) = track_under_test().await;
		let err = moq_video::Error::Codec(anyhow::anyhow!("the codec lost the tail"));
		finalize(producer, Err(err)).unwrap_err();

		let Err(err) = track.recv_group().await else {
			panic!("expected an abort, not a clean end");
		};
		assert!(
			err.to_string().contains("the codec lost the tail"),
			"the abort should carry the drain failure: {err}"
		);
	}
}

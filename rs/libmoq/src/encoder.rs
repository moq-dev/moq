//! Codec-only encoders, independent of any broadcast.
//!
//! Encoding submits a frame; [moq_audio_encoder_receive] and [moq_video_encoder_receive] pop
//! one packet at a time, which the encoder handle keeps until its next call or until it is
//! freed. [moq_audio_encoder_finish] keeps its packets the same way, readable by index with
//! [moq_audio_encoder_packet]. Publish them with [crate::moq_publish_audio] or
//! [crate::moq_publish_video] and [crate::moq_publish_media_frame].

use std::time::Duration;

use bytes::Bytes;

use crate::state::Shared;
use crate::{
	Error, NonZeroSlab, State, ffi, moq_audio_encoder_output, moq_audio_init, moq_frame, moq_timestamp,
	moq_video_encoder_frame, moq_video_encoder_input, moq_video_encoder_output, moq_video_pixel_format,
};

/// Open codec-only encoders. Each sits behind its own lock, since encoding a frame can block
/// on the codec and must not hold the global one.
#[derive(Default)]
pub struct Encoders {
	audio: NonZeroSlab<Shared<AudioEncoder>>,
	video: NonZeroSlab<Shared<VideoEncoder>>,
}

/// One packet emitted by a codec-only audio encoder.
///
/// The codec stamps nothing, so unlike a [moq_frame] it carries no timestamp or keyframe.
#[repr(C)]
pub struct moq_audio_packet {
	/// The encoded codec payload, borrowed from the encoder until its next call or
	/// [moq_audio_encoder_free].
	pub payload: *const u8,
	/// The length in bytes of `payload`.
	pub payload_size: usize,
}

/// Read packet `index` of the last call into `dst`.
fn packet(packets: &[Bytes], index: u32, dst: &mut moq_audio_packet) -> Result<(), Error> {
	let payload = packets.get(index as usize).ok_or(Error::NoIndex)?;
	*dst = moq_audio_packet {
		payload: payload.as_ptr(),
		payload_size: payload.len(),
	};
	Ok(())
}

/// One access unit the last call produced, owning what [moq_frame] borrows.
struct VideoPacket {
	payload: Bytes,
	/// Boxed so the pointer handed to C stays put while the encoder moves.
	timestamp: Box<moq_timestamp>,
	keyframe: bool,
}

impl VideoPacket {
	fn info(&self) -> moq_frame {
		moq_frame {
			payload: self.payload.as_ptr(),
			payload_size: self.payload.len(),
			timestamp: std::ptr::from_ref(&*self.timestamp),
			keyframe: self.keyframe,
		}
	}
}

/// The packet count a call returns.
fn count(packets: &[Bytes]) -> Result<i32, Error> {
	i32::try_from(packets.len()).map_err(|_| Error::OutOfRange("too many packets".into()))
}

struct AudioEncoder {
	/// `None` once finished; the packets it drained stay readable.
	encoder: Option<moq_audio::encode::Encoder>,
	/// The init that publishes the packets, read once at creation.
	init: Option<(crate::moq_audio_format, Bytes)>,
	packets: Vec<Bytes>,
}

impl AudioEncoder {
	fn encoder(&mut self) -> Result<&mut moq_audio::encode::Encoder, Error> {
		self.encoder
			.as_mut()
			.ok_or_else(|| Error::InvalidConfig("the encoder finished".into()))
	}

	fn keep(&mut self, packets: impl IntoIterator<Item = moq_audio::encode::Encoded>) -> Result<i32, Error> {
		// Reuses the buffer, since every received packet passes through it.
		self.packets.clear();
		self.packets.extend(packets.into_iter().map(|packet| packet.payload));
		count(&self.packets)
	}
}

struct VideoEncoder {
	encoder: moq_video::encode::Sink,
	format: moq_video_pixel_format,
	size: moq_video::Size,
	/// The last received access unit, owning what [moq_video_encoder_receive] lent out.
	packet: Option<VideoPacket>,
}

/// Run `f` on an encoder resolved under the global lock, then locked on its own.
fn with<T, R>(
	id: u32,
	slab: fn(&Encoders) -> &NonZeroSlab<Shared<T>>,
	f: impl FnOnce(&mut T) -> Result<R, Error>,
) -> Result<R, Error> {
	let id = ffi::parse_id(id)?;
	// A statement of its own, so the global guard drops before the encoder's lock is taken.
	let shared = slab(&State::lock().encoders).get(id).cloned().ok_or(Error::NotFound)?;
	let mut guard = shared.lock();
	f(guard.as_mut().ok_or(Error::NotFound)?)
}

fn audio(encoders: &Encoders) -> &NonZeroSlab<Shared<AudioEncoder>> {
	&encoders.audio
}

fn video(encoders: &Encoders) -> &NonZeroSlab<Shared<VideoEncoder>> {
	&encoders.video
}

/// Open a codec-only audio encoder: interleaved f32 PCM in, codec packets out.
///
/// `output` is read as for [crate::moq_encode_audio], except that with no input to match, a
/// zero `sample_rate` is 48000 and a zero `channels` is 2. Returns a positive encoder handle,
/// or a negative code on failure; release it with [moq_audio_encoder_free].
///
/// # Safety
/// - `output` must point at a readable [moq_audio_encoder_output] whose `codec` is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_create(output: *const moq_audio_encoder_output) -> i32 {
	ffi::enter(move || {
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let codec = unsafe { ffi::parse_str(raw.codec, raw.codec_len)? };
		let codec = codec.parse().map_err(|_| Error::UnknownFormat(codec.to_string()))?;
		let sample_rate = if raw.sample_rate == 0 { 48_000 } else { raw.sample_rate };
		let layout = moq_audio::Layout::from_channels(if raw.channels == 0 { 2 } else { raw.channels })?;
		let input = moq_audio::encode::Input::new(sample_rate, layout);
		let mut config = moq_audio::encode::Config::from_input(codec, &input);
		config.bitrate = (raw.bitrate != 0).then(|| moq_net::bandwidth::Rate::from_bps(raw.bitrate.into()));
		if raw.frame_duration_us != 0 {
			config.frame_duration = Duration::from_micros(raw.frame_duration_us.into());
		}

		let encoder = moq_audio::encode::Encoder::new(&config)?;
		let format = match encoder.codec() {
			moq_audio::encode::Codec::Opus => Some(crate::moq_audio_format::MOQ_AUDIO_FORMAT_OPUS),
			moq_audio::encode::Codec::Aac => Some(crate::moq_audio_format::MOQ_AUDIO_FORMAT_AAC),
			_ => None,
		};
		let init = format.zip(encoder.catalog().description);
		State::lock().encoders.audio.insert(Shared::new(AudioEncoder {
			encoder: Some(encoder),
			init,
			packets: Vec::new(),
		}))
	})
}

/// The interleaved samples per channel each [moq_audio_encoder_encode] call takes.
///
/// Returns the positive count, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_audio_encoder_frame_size(encoder: u32) -> i32 {
	ffi::enter(move || {
		with(encoder, audio, |audio| {
			i32::try_from(audio.encoder()?.frame_size()).map_err(|_| Error::OutOfRange("frame size".into()))
		})
	})
}

/// The init that publishes this encoder's packets through [crate::moq_publish_audio]: an
/// OpusHead or an AudioSpecificConfig, with `label` NULL and `hint` zeroed.
///
/// `init` borrows the encoder's storage until [moq_audio_encoder_free]. Returns zero, or a
/// negative code: `MOQ_ERROR_UNSUPPORTED` for a codec with no init, such as PCM.
///
/// # Safety
/// - `dst` must point at a writable [moq_audio_init].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_init(encoder: u32, dst: *mut moq_audio_init) -> i32 {
	ffi::enter(move || {
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		with(encoder, audio, |audio| {
			let (format, init) = audio
				.init
				.as_ref()
				.ok_or_else(|| Error::Unsupported("this codec publishes no init".into()))?;
			*dst = moq_audio_init {
				format: *format as u32,
				init: init.as_ptr(),
				init_len: init.len(),
				label: std::ptr::null(),
				label_len: 0,
				hint: crate::moq_audio_hint::default(),
			};
			Ok(())
		})
	})
}

/// Retune the live encoder to `bitrate` bits per second.
#[unsafe(no_mangle)]
pub extern "C" fn moq_audio_encoder_bitrate(encoder: u32, bitrate: u64) -> i32 {
	ffi::enter(move || {
		with(encoder, audio, |audio| {
			audio
				.encoder()?
				.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate))?;
			Ok(())
		})
	})
}

/// Encode one frame of interleaved f32 PCM, exactly [moq_audio_encoder_frame_size] samples
/// per channel.
///
/// Returns zero, or a negative code on failure. Read the frame's packet with
/// [moq_audio_encoder_receive], which a pipelining codec readies only after later frames.
///
/// # Safety
/// - `pcm` must point at `pcm_len` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_encode(encoder: u32, pcm: *const f32, pcm_len: usize) -> i32 {
	ffi::enter(move || {
		let pcm = unsafe { ffi::parse_slice(pcm, pcm_len)? };
		with(encoder, audio, |audio| {
			audio.encoder()?.encode(pcm)?;
			Ok(0)
		})
	})
}

/// Pop the oldest ready packet into `dst`; call until it returns zero after each
/// [moq_audio_encoder_encode] or [moq_audio_encoder_flush].
///
/// Returns one when it wrote a packet, zero while the codec holds the rest, or a negative code.
/// `payload` borrows the encoder until its next call or [moq_audio_encoder_free].
///
/// # Safety
/// - `dst` must point at a writable [moq_audio_packet].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_receive(encoder: u32, dst: *mut moq_audio_packet) -> i32 {
	ffi::enter(move || {
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		with(encoder, audio, |audio| {
			let Some(ready) = audio.encoder()?.receive()? else {
				return Ok(0);
			};
			audio.keep([ready])?;
			packet(&audio.packets, 0, dst)?;
			Ok(1)
		})
	})
}

/// Mark the end of input, so [moq_audio_encoder_receive] returns every packet the codec still
/// holds, without padding or closing the encoder.
///
/// Returns zero, or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn moq_audio_encoder_flush(encoder: u32) -> i32 {
	ffi::enter(move || {
		with(encoder, audio, |audio| {
			audio.encoder()?.flush()?;
			Ok(0)
		})
	})
}

/// Drain the encoder, padding one final partial frame of interleaved f32 PCM (`pcm_len` may
/// be zero).
///
/// Writes the padded samples to discard from the decoded tail into `discard_padding`, and
/// returns the number of packets drained, read with [moq_audio_encoder_packet], or a negative
/// code. Nothing more can be encoded; free the handle once the packets are read.
///
/// # Safety
/// - `pcm` must point at `pcm_len` floats, or be NULL with a zero length.
/// - `discard_padding` must point at a writable `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_finish(
	encoder: u32,
	pcm: *const f32,
	pcm_len: usize,
	discard_padding: *mut u64,
) -> i32 {
	ffi::enter(move || {
		let pcm = unsafe { ffi::parse_slice(pcm, pcm_len)? };
		let discard_padding = unsafe { discard_padding.as_mut() }.ok_or(Error::InvalidPointer)?;
		with(encoder, audio, |audio| {
			let encoder = audio
				.encoder
				.take()
				.ok_or_else(|| Error::InvalidConfig("the encoder finished".into()))?;
			let finish = encoder.finish(pcm)?;
			*discard_padding = finish.discard_padding() as u64;
			audio.keep(finish.into_packets())
		})
	})
}

/// Read packet `index` from the last [moq_audio_encoder_finish].
///
/// `payload` borrows the encoder until its next call or [moq_audio_encoder_free]. Returns zero,
/// or a negative code, `MOQ_ERROR_NO_INDEX` past the last packet.
///
/// # Safety
/// - `dst` must point at a writable [moq_audio_packet].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_audio_encoder_packet(encoder: u32, index: u32, dst: *mut moq_audio_packet) -> i32 {
	ffi::enter(move || {
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		with(encoder, audio, |audio| packet(&audio.packets, index, dst))
	})
}

/// Close a codec-only audio encoder and release its packets.
#[unsafe(no_mangle)]
pub extern "C" fn moq_audio_encoder_free(encoder: u32) -> i32 {
	ffi::enter(move || {
		let encoder = ffi::parse_id(encoder)?;
		let shared = State::lock().encoders.audio.remove(encoder).ok_or(Error::NotFound)?;
		// Taken outside the global lock: a call still encoding holds the encoder's own.
		shared.take();
		Ok(())
	})
}

/// Open a codec-only video encoder: raw pictures in, access units out.
///
/// `input` and `output` are read as for [crate::moq_encode_video], and the encoder is opened
/// here, so an unsupported codec, resolution, or backend fails now. Returns a positive encoder
/// handle, or a negative code on failure; release it with [moq_video_encoder_free].
///
/// # Safety
/// - `input` and `output` must point at fully populated structs, as for [crate::moq_encode_video].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_video_encoder_create(
	input: *const moq_video_encoder_input,
	output: *const moq_video_encoder_output,
) -> i32 {
	ffi::enter(move || {
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let (format, config) = unsafe { crate::video::encoder_config(raw_input, raw_output)? };
		// Opened before the global lock is taken: bringing up a hardware encoder is slow.
		let encoder = crate::video::block_on(moq_video::encode::Sink::open(&config))?;
		State::lock().encoders.video.insert(Shared::new(VideoEncoder {
			encoder,
			format,
			size: config.size(),
			packet: None,
		}))
	})
}

/// Encode one raw picture in the input's layout and resolution.
///
/// Returns zero, or a negative code on failure. Read its access units with
/// [moq_video_encoder_receive], which a pipelining backend readies only after later pictures.
///
/// # Safety
/// - `frame` must point at a valid [moq_video_encoder_frame] whose `data` holds `data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_video_encoder_encode(encoder: u32, frame: *const moq_video_encoder_frame) -> i32 {
	ffi::enter(move || {
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };
		let timestamp = frame.timestamp.try_into()?;
		with(encoder, video, |video| {
			let surface = match video.format {
				moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => {
					moq_video::Surface::I420(moq_video::I420::new(video.size, data.to_vec())?)
				}
				moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => moq_video::Surface::rgba(data, video.size)?,
			};
			let frame = moq_video::Frame::new(surface, timestamp);
			crate::video::block_on(video.encoder.encode(frame))?;
			Ok(0)
		})
	})
}

/// Force the next encoded picture to start a new group with a keyframe.
#[unsafe(no_mangle)]
pub extern "C" fn moq_video_encoder_cut(encoder: u32) -> i32 {
	ffi::enter(move || {
		with(encoder, video, |video| {
			crate::video::block_on(video.encoder.cut())?;
			Ok(())
		})
	})
}

/// Retune the live encoder to `bitrate` bits per second, from roughly the next picture.
#[unsafe(no_mangle)]
pub extern "C" fn moq_video_encoder_bitrate(encoder: u32, bitrate: u64) -> i32 {
	ffi::enter(move || {
		with(encoder, video, |video| {
			crate::video::block_on(video.encoder.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate)))?;
			Ok(())
		})
	})
}

/// Pop the oldest ready access unit into `dst`; call until it returns zero after each
/// [moq_video_encoder_encode] or [moq_video_encoder_flush].
///
/// Returns one when it wrote an access unit, zero while the codec holds the rest, or a negative
/// code. `payload` and `timestamp` borrow the encoder until its next call or
/// [moq_video_encoder_free]; `keyframe` says whether the unit starts from a keyframe.
///
/// # Safety
/// - `dst` must point at a writable [moq_frame].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_video_encoder_receive(encoder: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		with(encoder, video, |video| {
			let Some(ready) = video.encoder.receive()? else {
				return Ok(0);
			};
			let packet = video.packet.insert(VideoPacket {
				payload: ready.payload,
				timestamp: Box::new(ready.timestamp.into()),
				keyframe: ready.keyframe,
			});
			*dst = packet.info();
			Ok(1)
		})
	})
}

/// Mark the end of input, so [moq_video_encoder_receive] returns every access unit the codec
/// still holds. The encoder stays open: the next picture starts new input.
///
/// Returns zero, or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn moq_video_encoder_flush(encoder: u32) -> i32 {
	ffi::enter(move || {
		with(encoder, video, |video| {
			crate::video::block_on(video.encoder.flush())?;
			Ok(0)
		})
	})
}

/// Close a codec-only video encoder and release its access units.
#[unsafe(no_mangle)]
pub extern "C" fn moq_video_encoder_free(encoder: u32) -> i32 {
	ffi::enter(move || {
		let encoder = ffi::parse_id(encoder)?;
		let shared = State::lock().encoders.video.remove(encoder).ok_or(Error::NotFound)?;
		// Taken outside the global lock: a call still encoding holds the encoder's own.
		shared.take();
		Ok(())
	})
}

//! Raw-audio import/export via [`moq_audio`].
//!
//! Sibling to `moq_publish_media_*` / `moq_consume_audio_ordered`
//! (those handle already-encoded frames). These functions accept and
//! return raw PCM, with Opus encode/decode happening inside the FFI
//! boundary.
//!
//! Format / sample rate / channel count are fixed at producer or
//! consumer construction via [`moq_audio_encoder_input`] /
//! [`moq_audio_encoder_output`] / [`moq_audio_decoder_output`], so
//! each [`moq_audio_frame`] carries only payload bytes and a
//! timestamp.

use std::ffi::{c_char, c_void};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, ffi};

// ---- C-visible types ----

/// Raw PCM sample layout, mirroring WebCodecs `AudioData.format`.
///
/// The enum is exposed in the C header for readability, but ABI
/// fields/parameters that carry it are typed `u32`. A C caller
/// passing an unknown discriminant gets `Error::InvalidCode` instead
/// of UB.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_audio_format {
	MOQ_AUDIO_FORMAT_U8 = 0,
	MOQ_AUDIO_FORMAT_S16 = 1,
	MOQ_AUDIO_FORMAT_S32 = 2,
	MOQ_AUDIO_FORMAT_F32 = 3,
	MOQ_AUDIO_FORMAT_U8_PLANAR = 4,
	MOQ_AUDIO_FORMAT_S16_PLANAR = 5,
	MOQ_AUDIO_FORMAT_S32_PLANAR = 6,
	MOQ_AUDIO_FORMAT_F32_PLANAR = 7,
}

fn audio_format_from_u32(value: u32) -> Result<moq_audio::AudioFormat, Error> {
	use moq_audio::AudioFormat;
	Ok(match value {
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_U8 as u32 => AudioFormat::U8,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_S16 as u32 => AudioFormat::S16,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_S32 as u32 => AudioFormat::S32,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_F32 as u32 => AudioFormat::F32,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_U8_PLANAR as u32 => AudioFormat::U8Planar,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_S16_PLANAR as u32 => AudioFormat::S16Planar,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_S32_PLANAR as u32 => AudioFormat::S32Planar,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_F32_PLANAR as u32 => AudioFormat::F32Planar,
		_ => return Err(Error::InvalidCode),
	})
}

/// PCM layout the caller hands to [`moq_publish_audio_raw_frame`].
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_encoder_input {
	/// `moq_audio_format` discriminant.
	pub format: u32,
	pub sample_rate: u32,
	pub channels: u32,
}

/// Codec-side configuration. `sample_rate` / `channels` = 0 means
/// "match the input (snapping the rate up to a libopus-supported
/// value if necessary)".
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_encoder_output {
	/// Codec id, UTF-8 (currently only "opus").
	pub codec: *const c_char,
	pub codec_len: usize,
	/// 0 = derive from input.
	pub sample_rate: u32,
	/// 0 = derive from input.
	pub channels: u32,
	/// 0 = libopus default.
	pub bitrate: u32,
	/// Encoded frame duration in milliseconds. Opus accepts
	/// 2.5/5/10/20/40/60 ms; pass 20 to match the JS publish path.
	/// (For 2.5 ms, the caller must pre-round; integer ms only.)
	pub frame_duration_ms: u32,
}

/// PCM layout the caller wants out of [`moq_consume_audio_raw`].
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_decoder_output {
	pub format: u32,
	/// 0 = deliver at the codec's native sample rate.
	pub sample_rate: u32,
	/// 0 = deliver at the codec's native channel count.
	pub channels: u32,
	/// Upper bound on buffering before skipping a stalled group, in
	/// milliseconds. Same congestion-control knob as
	/// `moq_consume_audio_ordered`'s `max_latency_ms`. 0 = skip
	/// aggressively (the moq-mux default); set to your playout
	/// buffer (tens to a few hundred ms) for a softer skip. Named
	/// `_max` to leave room for a future `latency_min_ms`
	/// (jitter-buffer floor).
	pub latency_max_ms: u64,
}

/// One audio frame: payload bytes plus a presentation timestamp.
///
/// `data` is owned by the consume slab (see
/// [`moq_consume_audio_raw_frame_free`]) or borrowed by the publish call
/// (the publisher copies before returning).
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_frame {
	pub timestamp_us: u64,
	pub data: *const u8,
	pub data_size: usize,
}

// ---- State extensions (used internally by lib.rs) ----

#[derive(Default)]
pub struct Audio {
	producers: NonZeroSlab<moq_audio::AudioProducer>,
	consumer_tasks: NonZeroSlab<Option<AudioTaskEntry>>,
	frames: NonZeroSlab<moq_audio::Frame>,
}

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so `consume_close` can drop just the sender without
/// removing the entry. The task delivers one final terminal callback and then
/// removes itself, so `user_data` stays valid until that callback fires.
struct AudioTaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

impl Audio {
	pub fn publish(
		&mut self,
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::hang::Producer,
		name: &str,
		input: moq_audio::EncoderInput,
		output: moq_audio::EncoderOutput,
	) -> Result<Id, Error> {
		let producer = moq_audio::AudioProducer::new(broadcast, catalog, name, input, output)?;
		self.producers.insert(producer)
	}

	pub fn publish_frame(&mut self, id: Id, frame: moq_audio::Frame) -> Result<(), Error> {
		let producer = self.producers.get_mut(id).ok_or(Error::MediaNotFound)?;
		producer.write(&frame)?;
		Ok(())
	}

	pub fn publish_close(&mut self, id: Id) -> Result<(), Error> {
		let producer = self.producers.remove(id).ok_or(Error::MediaNotFound)?;
		producer.finish()?;
		Ok(())
	}

	pub fn consume(
		&mut self,
		broadcast: &moq_net::BroadcastConsumer,
		catalog: &hang::catalog::AudioConfig,
		name: &str,
		output: moq_audio::DecoderOutput,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consumer = moq_audio::AudioConsumer::new(broadcast, catalog, name, output)?;

		let channel = oneshot::channel();
		let entry = AudioTaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.consumer_tasks.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = Self::run(on_frame, consumer, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().audio.consumer_tasks.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_audio::AudioConsumer,
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

			// Hold the lock only to buffer the frame; release it before the callback.
			let frame_id = State::lock().audio.frames.insert(frame)?;
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

	pub fn frame_info(&self, id: Id, dst: &mut moq_audio_frame) -> Result<(), Error> {
		let frame = self.frames.get(id).ok_or(Error::FrameNotFound)?;
		*dst = moq_audio_frame {
			timestamp_us: frame.timestamp_us,
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

/// Open an audio track on a broadcast.
///
/// The encoder configuration is fixed at construction; subsequent
/// frame writes pass only payload + timestamp via
/// [`moq_publish_audio_raw_frame`].
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `name` must point to `name_len` bytes of UTF-8.
/// - `input` / `output` must point to fully populated structs.
/// - `output->codec` must point to `output->codec_len` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_raw(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	input: *const moq_audio_encoder_input,
	output: *const moq_audio_encoder_output,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? }.to_string();
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let codec_str = unsafe { ffi::parse_str(raw_output.codec, raw_output.codec_len)? };

		let encoder_input = moq_audio::EncoderInput {
			format: audio_format_from_u32(raw_input.format)?,
			sample_rate: raw_input.sample_rate,
			channels: raw_input.channels,
		};
		let encoder_output = moq_audio::EncoderOutput {
			codec: codec_str
				.parse()
				.map_err(|_| Error::UnknownFormat(codec_str.to_string()))?,
			sample_rate: if raw_output.sample_rate == 0 {
				None
			} else {
				Some(raw_output.sample_rate)
			},
			channels: if raw_output.channels == 0 {
				None
			} else {
				Some(raw_output.channels)
			},
			bitrate: if raw_output.bitrate == 0 {
				None
			} else {
				Some(raw_output.bitrate)
			},
			frame_duration: Duration::from_millis(raw_output.frame_duration_ms.into()),
		};

		let mut state = State::lock();
		let State { publish, audio, .. } = &mut *state;
		let (broadcast_producer, catalog) = publish.pair_mut(broadcast)?;

		audio.publish(
			broadcast_producer,
			catalog.clone(),
			&name,
			encoder_input,
			encoder_output,
		)
	})
}

/// Push one audio frame.
///
/// `frame->data` is borrowed for the duration of the call; the
/// producer copies before returning.
///
/// # Safety
/// - `frame` must point to a valid [`moq_audio_frame`].
/// - `frame->data` must point to `frame->data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_raw_frame(producer: u32, frame: *const moq_audio_frame) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };

		let owned = moq_audio::Frame {
			timestamp_us: frame.timestamp_us,
			data: Bytes::copy_from_slice(data),
		};

		State::lock().audio.publish_frame(producer, owned)
	})
}

/// Flush any pending samples and finalize an audio producer.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_audio_raw_close(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		State::lock().audio.publish_close(producer)
	})
}

/// Subscribe to an audio track and decode it into PCM.
///
/// The catalog `index` identifies which audio rendition to subscribe
/// to, matching the existing `moq_consume_audio_ordered` selection
/// model. TODO: a future API will pick the right rendition
/// automatically (ABR).
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_frame` is called with a positive frame ID per frame, then exactly once
/// more with a terminal code: `0` (closed cleanly) or a negative error. After
/// the terminal (`<= 0`) callback, `on_frame` is never called again and
/// `user_data` is never touched again, so release `user_data` there. The
/// terminal callback fires even after [`moq_consume_audio_raw_close`].
///
/// # Safety
/// - `output` must point to a valid [`moq_audio_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_raw(
	catalog: u32,
	index: u32,
	output: *const moq_audio_decoder_output,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		let decoder_output = moq_audio::DecoderOutput {
			format: audio_format_from_u32(raw.format)?,
			sample_rate: if raw.sample_rate == 0 {
				None
			} else {
				Some(raw.sample_rate)
			},
			channels: if raw.channels == 0 { None } else { Some(raw.channels) },
			latency_max: if raw.latency_max_ms == 0 {
				None
			} else {
				Some(Duration::from_millis(raw.latency_max_ms))
			},
		};
		let on_frame = unsafe { OnStatus::new(user_data, on_frame) };

		let mut state = State::lock();
		let (broadcast, audio_cfg, name) = state.consume.audio_rendition(catalog, index as usize)?;

		let State { audio, .. } = &mut *state;
		audio.consume(&broadcast, &audio_cfg, &name, decoder_output, on_frame)
	})
}

/// Stop an audio (raw PCM) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-frame callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Frame IDs already delivered to the callback are likewise not freed;
/// release each with [`moq_consume_audio_raw_frame_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_audio_raw_close(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().audio.consume_close(consumer)
	})
}

/// Copy a delivered frame's metadata into `dst`.
///
/// The written `dst->data` pointer remains valid until the same `id`
/// is released with [`moq_consume_audio_raw_frame_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_audio_frame`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_raw_frame(id: u32, dst: *mut moq_audio_frame) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().audio.frame_info(id, dst)
	})
}

/// Free a frame previously delivered through the consume callback.
/// Required for every delivered frame ID; closing the parent consumer
/// is not enough.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_audio_raw_frame_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().audio.frame_free(id)
	})
}

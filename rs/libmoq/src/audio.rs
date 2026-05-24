//! Raw-audio import/export via [`moq_audio`].
//!
//! Mirrors the encoded media API (`moq_publish_media_*` /
//! `moq_consume_audio_ordered`) but talks in PCM samples on the C side
//! and goes through [`moq_audio::AudioProducer`] /
//! [`moq_audio::AudioConsumer`] for the codec work.

use std::ffi::{c_char, c_void};

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, ffi};

// ---- C-visible types ----

/// Raw PCM sample layout, mirroring WebCodecs `AudioData.format`.
///
/// Exposed as a C enum for header readability, but the ABI fields and
/// parameters that carry it (`moq_raw_audio.format`,
/// `moq_consume_raw_audio_opus`'s `output_format`) are typed as `u32`.
/// A C caller passing an unknown discriminant would otherwise be UB at
/// the Rust boundary; the integer ABI lets us validate via
/// `audio_format_from_u32` before any downstream code runs.
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

/// Convert a C-side discriminant into a typed Rust `AudioFormat`,
/// rejecting unknown values up front.
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

/// Convert a typed Rust `AudioFormat` into its C-side discriminant.
fn audio_format_to_u32(f: moq_audio::AudioFormat) -> Result<u32, Error> {
	use moq_audio::AudioFormat as A;
	Ok(match f {
		A::U8 => moq_audio_format::MOQ_AUDIO_FORMAT_U8 as u32,
		A::S16 => moq_audio_format::MOQ_AUDIO_FORMAT_S16 as u32,
		A::S32 => moq_audio_format::MOQ_AUDIO_FORMAT_S32 as u32,
		A::F32 => moq_audio_format::MOQ_AUDIO_FORMAT_F32 as u32,
		A::U8Planar => moq_audio_format::MOQ_AUDIO_FORMAT_U8_PLANAR as u32,
		A::S16Planar => moq_audio_format::MOQ_AUDIO_FORMAT_S16_PLANAR as u32,
		A::S32Planar => moq_audio_format::MOQ_AUDIO_FORMAT_S32_PLANAR as u32,
		A::F32Planar => moq_audio_format::MOQ_AUDIO_FORMAT_F32_PLANAR as u32,
		_ => return Err(Error::InvalidCode),
	})
}

/// A buffer of raw PCM samples passed across the FFI boundary.
///
/// `format` is a `u32` carrying a `moq_audio_format` discriminant; it's
/// not declared as the enum type so that an unknown value from C lands
/// in `audio_format_from_u32` instead of becoming an invalid Rust enum
/// (which would be UB).
///
/// `data` is borrowed: the pointer is valid for the duration of the C
/// call (publish) or callback (consume). Callers that need to keep the
/// samples must copy.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_raw_audio {
	pub format: u32,
	pub sample_rate: u32,
	pub channel_count: u32,
	pub timestamp_us: u64,
	pub data: *const u8,
	pub data_size: usize,
}

// ---- State extensions (used internally by lib.rs) ----

#[derive(Default)]
pub struct Audio {
	/// Active raw-audio producers.
	producers: NonZeroSlab<moq_audio::AudioProducer>,

	/// Active raw-audio consumer tasks.
	consumer_tasks: NonZeroSlab<Option<AudioTaskEntry>>,

	/// Buffered raw-audio samples ready for the C callback.
	samples: NonZeroSlab<moq_audio::AudioSamples>,
}

struct AudioTaskEntry {
	#[allow(dead_code)] // Dropping signals shutdown via channel.
	close: oneshot::Sender<()>,
	callback: OnStatus,
}

impl Audio {
	pub fn publish_opus(
		&mut self,
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::hang::Producer,
		name: &str,
		sample_rate: u32,
		channel_count: u32,
		bitrate: Option<u32>,
	) -> Result<Id, Error> {
		let producer =
			moq_audio::AudioProducer::new_opus(broadcast, catalog, name, sample_rate, channel_count, bitrate)?;
		self.producers.insert(producer)
	}

	pub fn publish_write(&mut self, id: Id, samples: &moq_audio::AudioSamples) -> Result<(), Error> {
		let producer = self.producers.get_mut(id).ok_or(Error::MediaNotFound)?;
		producer.write(samples)?;
		Ok(())
	}

	pub fn publish_close(&mut self, id: Id) -> Result<(), Error> {
		let producer = self.producers.remove(id).ok_or(Error::MediaNotFound)?;
		producer.finish()?;
		Ok(())
	}

	#[allow(clippy::too_many_arguments)]
	pub fn consume_opus(
		&mut self,
		broadcast: &moq_net::BroadcastConsumer,
		config: &hang::catalog::AudioConfig,
		name: &str,
		output_format: moq_audio::AudioFormat,
		output_sample_rate: Option<u32>,
		output_channels: Option<u32>,
		on_samples: OnStatus,
	) -> Result<Id, Error> {
		let consumer = moq_audio::AudioConsumer::subscribe_opus(
			broadcast,
			config,
			name,
			output_format,
			output_sample_rate,
			output_channels,
		)?;

		let channel = oneshot::channel();
		let entry = AudioTaskEntry {
			close: channel.0,
			callback: on_samples,
		};
		let id = self.consumer_tasks.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run(id, consumer) => res,
				_ = channel.1 => Ok(()),
			};

			if let Some(entry) = State::lock().audio.consumer_tasks.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run(task_id: Id, mut consumer: moq_audio::AudioConsumer) -> Result<(), Error> {
		while let Some(samples) = consumer.read().await? {
			let mut state = State::lock();
			let Some(Some(entry)) = state.audio.consumer_tasks.get(task_id) else {
				return Ok(());
			};
			let callback = entry.callback;
			let sample_id = state.audio.samples.insert(samples)?;
			drop(state);

			callback.call(Ok(sample_id));
		}
		Ok(())
	}

	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		self.consumer_tasks
			.get_mut(id)
			.ok_or(Error::TrackNotFound)?
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	pub fn sample_info(&self, id: Id, dst: &mut moq_raw_audio) -> Result<(), Error> {
		let samples = self.samples.get(id).ok_or(Error::FrameNotFound)?;
		*dst = moq_raw_audio {
			format: audio_format_to_u32(samples.format)?,
			sample_rate: samples.sample_rate,
			channel_count: samples.channel_count,
			timestamp_us: samples.timestamp_us,
			data: samples.data.as_ptr(),
			data_size: samples.data.len(),
		};
		Ok(())
	}

	pub fn sample_free(&mut self, id: Id) -> Result<(), Error> {
		self.samples.remove(id).ok_or(Error::FrameNotFound)?;
		Ok(())
	}
}

// ---- C entry points ----

/// Open a raw-audio Opus track on a broadcast.
///
/// `sample_rate` and `channel_count` describe the PCM the caller will
/// feed to [`moq_publish_raw_audio_write`]. A resampler runs
/// internally if `sample_rate` isn't one Opus supports natively. The
/// per-write `moq_raw_audio.format` carries the sample layout, so no
/// format is needed at publish time.
///
/// `bitrate` is bits-per-second; pass 0 for the libopus default.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `name` must point to `name_len` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_raw_audio_opus(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	sample_rate: u32,
	channel_count: u32,
	bitrate: u32,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? }.to_string();
		let bitrate = if bitrate == 0 { None } else { Some(bitrate) };

		let mut state = State::lock();
		// Split borrow so publish and audio can be borrowed mutably together.
		let State { publish, audio, .. } = &mut *state;
		// Get a mutable reference to the (broadcast, catalog) pair.
		let (broadcast_producer, catalog) = publish.pair_mut(broadcast)?;

		audio.publish_opus(
			broadcast_producer,
			catalog.clone(),
			&name,
			sample_rate,
			channel_count,
			bitrate,
		)
	})
}

/// Push a buffer of raw PCM samples to a producer.
///
/// Returns zero on success or a negative error code.
///
/// # Safety
/// - `audio` must be a valid pointer to a [`moq_raw_audio`] populated by the caller.
/// - `audio.data` must point to `audio.data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_raw_audio_write(producer: u32, audio: *const moq_raw_audio) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let audio = unsafe { audio.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(audio.data, audio.data_size)? };

		let samples = moq_audio::AudioSamples {
			format: audio_format_from_u32(audio.format)?,
			sample_rate: audio.sample_rate,
			channel_count: audio.channel_count,
			timestamp_us: audio.timestamp_us,
			data: Bytes::copy_from_slice(data),
		};

		State::lock().audio.publish_write(producer, &samples)
	})
}

/// Flush any pending samples and finalize a raw-audio producer.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_raw_audio_close(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		State::lock().audio.publish_close(producer)
	})
}

/// Subscribe to a raw-audio Opus track and decode it into PCM samples.
///
/// `output_sample_rate` of 0 means "deliver at the codec's native rate".
/// `output_channels` of 0 means "deliver at the codec's native channel count".
///
/// The catalog `index` identifies which audio rendition to subscribe to,
/// matching the existing `moq_consume_audio_ordered` selection model.
/// TODO: a future API will pick the right rendition automatically (ABR).
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `on_samples` must be valid until [`moq_consume_raw_audio_close`] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_raw_audio_opus(
	catalog: u32,
	index: u32,
	output_format: u32,
	output_sample_rate: u32,
	output_channels: u32,
	on_samples: Option<extern "C" fn(user_data: *mut c_void, samples: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let output_format = audio_format_from_u32(output_format)?;
		let on_samples = unsafe { OnStatus::new(user_data, on_samples) };

		let mut state = State::lock();
		let (broadcast, config, name) = state.consume.audio_rendition(catalog, index as usize)?;

		let State { audio, .. } = &mut *state;
		audio.consume_opus(
			&broadcast,
			&config,
			&name,
			output_format,
			if output_sample_rate == 0 {
				None
			} else {
				Some(output_sample_rate)
			},
			if output_channels == 0 {
				None
			} else {
				Some(output_channels)
			},
			on_samples,
		)
	})
}

/// Stop consuming a raw-audio track and clean up its resources.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_raw_audio_close(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().audio.consume_close(consumer)
	})
}

/// Copy a sample buffer's metadata into `dst`. The `data` pointer
/// remains valid until [`moq_consume_raw_audio_sample_free`] is called.
///
/// # Safety
/// - `dst` must point to a writable [`moq_raw_audio`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_raw_audio_sample(id: u32, dst: *mut moq_raw_audio) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().audio.sample_info(id, dst)
	})
}

/// Free a sample buffer previously delivered through the consume callback.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_raw_audio_sample_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().audio.sample_free(id)
	})
}

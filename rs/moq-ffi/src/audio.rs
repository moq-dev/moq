//! Raw-audio import/export via [`moq_audio`].
//!
//! The existing [`producer::MoqMediaProducer`](crate::producer::MoqMediaProducer)
//! and [`consumer::MoqMediaConsumer`](crate::consumer::MoqMediaConsumer)
//! deal in already-encoded frames; callers needed to bring their own
//! codec. These types let callers pass and receive raw PCM in any
//! WebCodecs `AudioData.format`, with Opus encode/decode happening
//! inside the FFI boundary.

use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::MoqBroadcastProducer;

/// Raw PCM sample format, mirroring WebCodecs `AudioData.format`.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqAudioFormat {
	U8,
	S16,
	S32,
	F32,
	U8Planar,
	S16Planar,
	S32Planar,
	F32Planar,
}

impl From<MoqAudioFormat> for moq_audio::AudioFormat {
	fn from(f: MoqAudioFormat) -> Self {
		match f {
			MoqAudioFormat::U8 => Self::U8,
			MoqAudioFormat::S16 => Self::S16,
			MoqAudioFormat::S32 => Self::S32,
			MoqAudioFormat::F32 => Self::F32,
			MoqAudioFormat::U8Planar => Self::U8Planar,
			MoqAudioFormat::S16Planar => Self::S16Planar,
			MoqAudioFormat::S32Planar => Self::S32Planar,
			MoqAudioFormat::F32Planar => Self::F32Planar,
		}
	}
}

impl TryFrom<moq_audio::AudioFormat> for MoqAudioFormat {
	type Error = MoqError;

	fn try_from(f: moq_audio::AudioFormat) -> Result<Self, MoqError> {
		use moq_audio::AudioFormat as A;
		Ok(match f {
			A::U8 => Self::U8,
			A::S16 => Self::S16,
			A::S32 => Self::S32,
			A::F32 => Self::F32,
			A::U8Planar => Self::U8Planar,
			A::S16Planar => Self::S16Planar,
			A::S32Planar => Self::S32Planar,
			A::F32Planar => Self::F32Planar,
			_ => return Err(MoqError::Codec(format!("unsupported audio format: {f:?}"))),
		})
	}
}

/// A buffer of raw PCM samples.
///
/// `data` layout is fully described by `format` and `channel_count`.
/// `timestamp_us` is the presentation timestamp of the first frame.
#[derive(uniffi::Record)]
pub struct MoqRawAudio {
	pub format: MoqAudioFormat,
	pub sample_rate: u32,
	pub channel_count: u32,
	pub timestamp_us: u64,
	pub data: Vec<u8>,
}

impl TryFrom<moq_audio::AudioSamples> for MoqRawAudio {
	type Error = MoqError;

	fn try_from(s: moq_audio::AudioSamples) -> Result<Self, MoqError> {
		Ok(Self {
			format: s.format.try_into()?,
			sample_rate: s.sample_rate,
			channel_count: s.channel_count,
			timestamp_us: s.timestamp_us,
			data: s.data.to_vec(),
		})
	}
}

impl From<MoqRawAudio> for moq_audio::AudioSamples {
	fn from(a: MoqRawAudio) -> Self {
		Self {
			format: a.format.into(),
			sample_rate: a.sample_rate,
			channel_count: a.channel_count,
			timestamp_us: a.timestamp_us,
			data: a.data.into(),
		}
	}
}

// ---- Producer ----

/// Producer for a raw-audio track.
///
/// Built via [`MoqBroadcastProducer::publish_raw_audio_opus`]. Each
/// [`write`](Self::write) accepts PCM whose layout is described by the
/// per-call [`MoqRawAudio::format`] field; the producer converts to
/// interleaved `f32` per write, resamples to the codec's rate if
/// needed, then encodes through libopus and publishes to the
/// underlying moq broadcast.
#[derive(uniffi::Object)]
pub struct MoqRawAudioProducer {
	inner: std::sync::Mutex<Option<moq_audio::AudioProducer>>,
}

#[uniffi::export]
impl MoqRawAudioProducer {
	/// Push a buffer of raw PCM. Any partial trailing frame is buffered
	/// internally and emitted with the next call (or by [`finish`](Self::finish)).
	pub fn write(&self, audio: MoqRawAudio) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		let samples: moq_audio::AudioSamples = audio.into();
		producer.write(&samples)?;
		Ok(())
	}

	/// Flush any pending samples (padded with silence) and finalize the track.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Open a raw-audio Opus track on this broadcast.
	///
	/// `sample_rate` and `channel_count` describe the PCM the caller will
	/// feed to [`MoqRawAudioProducer::write`]; a resampler runs
	/// internally if `sample_rate` isn't one Opus supports natively. The
	/// per-write `MoqRawAudio.format` carries the sample layout, so no
	/// format is needed at publish time. `bitrate` is in bits per
	/// second; pass `None` for the libopus default.
	pub fn publish_raw_audio_opus(
		&self,
		name: String,
		sample_rate: u32,
		channel_count: u32,
		bitrate: Option<u32>,
	) -> Result<Arc<MoqRawAudioProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let producer = self.with_state(|state| {
			moq_audio::AudioProducer::new_opus(
				&mut state.broadcast,
				state.catalog.clone(),
				name,
				sample_rate,
				channel_count,
				bitrate,
			)
			.map_err(Into::into)
		})?;

		Ok(Arc::new(MoqRawAudioProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}
}

// ---- Consumer ----

struct ConsumerInner {
	consumer: moq_audio::AudioConsumer,
}

impl ConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqRawAudio>, MoqError> {
		match self.consumer.read().await? {
			Some(samples) => Ok(Some(samples.try_into()?)),
			None => Ok(None),
		}
	}
}

/// Consumer for a raw-audio track.
///
/// Built via [`MoqBroadcastConsumer::subscribe_raw_audio_opus`]. Each
/// [`next`](Self::next) decodes one Opus packet (and optionally
/// resamples) into PCM in the format chosen at subscribe time.
#[derive(uniffi::Object)]
pub struct MoqRawAudioConsumer {
	task: Task<ConsumerInner>,
}

#[uniffi::export]
impl MoqRawAudioConsumer {
	pub async fn next(&self) -> Result<Option<MoqRawAudio>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to a raw-audio Opus track.
	///
	/// The `audio_config` comes from the catalog (see
	/// [`MoqCatalogConsumer::next`](crate::consumer::MoqCatalogConsumer::next)).
	/// `output_format` is the WebCodecs-style PCM layout to deliver;
	/// `output_sample_rate` / `output_channels` trigger a resample if
	/// they differ from what the catalog declares.
	///
	/// TODO: a future API will pick the right rendition automatically
	/// (ABR-style) so callers don't have to thread the catalog through.
	pub fn subscribe_raw_audio_opus(
		&self,
		name: String,
		audio_config: crate::media::MoqAudio,
		output_format: MoqAudioFormat,
		output_sample_rate: Option<u32>,
		output_channels: Option<u32>,
	) -> Result<Arc<MoqRawAudioConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let mut cfg = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			audio_config.sample_rate,
			audio_config.channel_count,
		);
		cfg.bitrate = audio_config.bitrate;
		cfg.description = audio_config.description.map(Into::into);
		cfg.container = audio_config.container.into();

		let consumer = moq_audio::AudioConsumer::subscribe_opus(
			self.inner(),
			&cfg,
			name,
			output_format.into(),
			output_sample_rate,
			output_channels,
		)?;

		Ok(Arc::new(MoqRawAudioConsumer {
			task: Task::new(ConsumerInner { consumer }),
		}))
	}
}

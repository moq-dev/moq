//! Raw-audio import/export via [`moq_audio`].
//!
//! Sibling to [`producer::MoqMediaProducer`](crate::producer::MoqMediaProducer)
//! and [`consumer::MoqMediaConsumer`](crate::consumer::MoqMediaConsumer):
//! those deal in already-encoded frames, these deal in PCM and run
//! Opus encode/decode inside the FFI boundary.

use std::sync::Arc;
use std::time::Duration;

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

/// Audio codec identifier.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqAudioCodec {
	Opus,
}

impl From<MoqAudioCodec> for moq_audio::Codec {
	fn from(c: MoqAudioCodec) -> Self {
		match c {
			MoqAudioCodec::Opus => Self::Opus,
		}
	}
}

/// Encoder configuration. Format and rates are fixed for the lifetime
/// of the producer, so each [`MoqAudioFrame`] is just bytes + timestamp.
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderConfig {
	pub codec: MoqAudioCodec,
	pub input_format: MoqAudioFormat,
	pub input_sample_rate: u32,
	pub input_channels: u32,
	pub bitrate: Option<u32>,
	/// Encoded frame duration in milliseconds. Opus accepts
	/// 2.5/5/10/20/40/60 ms; pass 20 to match the JS publish path.
	pub frame_duration_ms: u32,
}

/// Decoder configuration.
#[derive(uniffi::Record)]
pub struct MoqAudioDecoderConfig {
	pub output_format: MoqAudioFormat,
	/// `None` delivers samples at the codec's native rate.
	pub output_sample_rate: Option<u32>,
	/// `None` delivers samples at the codec's native channel count.
	pub output_channels: Option<u32>,
}

/// One audio frame: payload bytes plus a presentation timestamp.
///
/// PCM layout is fixed by the producer / consumer config, so it is
/// **not** carried per-frame. On the producer side `data` is raw PCM
/// in the configured `input_format`; on the consumer side it is raw
/// PCM in the configured `output_format`.
#[derive(uniffi::Record)]
pub struct MoqAudioFrame {
	pub timestamp_us: u64,
	pub data: Vec<u8>,
}

impl From<moq_audio::Frame> for MoqAudioFrame {
	fn from(f: moq_audio::Frame) -> Self {
		Self {
			timestamp_us: f.timestamp_us,
			data: f.data.to_vec(),
		}
	}
}

impl From<MoqAudioFrame> for moq_audio::Frame {
	fn from(f: MoqAudioFrame) -> Self {
		Self {
			timestamp_us: f.timestamp_us,
			data: f.data.into(),
		}
	}
}

// ---- Producer ----

/// Producer for a raw-audio track.
///
/// Built via [`MoqBroadcastProducer::publish_audio`]. Each
/// [`write`](Self::write) accepts an [`MoqAudioFrame`] whose `data`
/// is PCM in the format declared by the [`MoqAudioEncoderConfig`]
/// passed at publish time.
#[derive(uniffi::Object)]
pub struct MoqAudioProducer {
	inner: std::sync::Mutex<Option<moq_audio::AudioProducer>>,
}

#[uniffi::export]
impl MoqAudioProducer {
	pub fn write(&self, frame: MoqAudioFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.write(&frame.into())?;
		Ok(())
	}

	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Open an audio track on this broadcast. The catalog rendition is
	/// registered immediately so subscribers can find the track even
	/// before the first frame is written.
	pub fn publish_audio(
		&self,
		name: String,
		config: MoqAudioEncoderConfig,
	) -> Result<Arc<MoqAudioProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let producer = self.with_state(|state| {
			moq_audio::AudioProducer::new(
				&mut state.broadcast,
				state.catalog.clone(),
				name,
				moq_audio::EncoderConfig {
					codec: config.codec.into(),
					input_format: config.input_format.into(),
					input_sample_rate: config.input_sample_rate,
					input_channels: config.input_channels,
					bitrate: config.bitrate,
					frame_duration: Duration::from_millis(config.frame_duration_ms.into()),
				},
			)
			.map_err(Into::into)
		})?;

		Ok(Arc::new(MoqAudioProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}
}

// ---- Consumer ----

struct ConsumerInner {
	consumer: moq_audio::AudioConsumer,
}

impl ConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqAudioFrame>, MoqError> {
		Ok(self.consumer.read().await?.map(Into::into))
	}
}

/// Consumer for a raw-audio track.
#[derive(uniffi::Object)]
pub struct MoqAudioConsumer {
	task: Task<ConsumerInner>,
}

#[uniffi::export]
impl MoqAudioConsumer {
	pub async fn next(&self) -> Result<Option<MoqAudioFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to an audio track. `catalog_audio_config` comes from
	/// the catalog (see
	/// [`MoqCatalogConsumer::next`](crate::consumer::MoqCatalogConsumer::next));
	/// the codec is inferred from it.
	pub fn subscribe_audio(
		&self,
		name: String,
		catalog_audio_config: crate::media::MoqAudio,
		config: MoqAudioDecoderConfig,
	) -> Result<Arc<MoqAudioConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let mut cfg = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			catalog_audio_config.sample_rate,
			catalog_audio_config.channel_count,
		);
		cfg.bitrate = catalog_audio_config.bitrate;
		cfg.description = catalog_audio_config.description.map(Into::into);
		cfg.container = catalog_audio_config.container.into();

		let consumer = moq_audio::AudioConsumer::new(
			self.inner(),
			&cfg,
			name,
			moq_audio::DecoderConfig {
				output_format: config.output_format.into(),
				output_sample_rate: config.output_sample_rate,
				output_channels: config.output_channels,
			},
		)?;

		Ok(Arc::new(MoqAudioConsumer {
			task: Task::new(ConsumerInner { consumer }),
		}))
	}
}

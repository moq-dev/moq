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

/// PCM layout the caller will pass to [`MoqAudioProducer::write`].
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderInput {
	pub format: MoqAudioFormat,
	pub sample_rate: u32,
	pub channels: u32,
}

/// Codec-side configuration. `sample_rate` / `channels` `None` means
/// "match the input (snapping the rate up to a libopus-supported
/// value if necessary)".
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderOutput {
	pub codec: MoqAudioCodec,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
	pub bitrate: Option<u32>,
	/// Encoded frame duration in milliseconds. Opus accepts
	/// 2.5/5/10/20/40/60 ms; pass 20 to match the JS publish path.
	pub frame_duration_ms: u32,
}

/// PCM layout the caller wants out of [`MoqAudioConsumer::next`].
#[derive(uniffi::Record)]
pub struct MoqAudioDecoderOutput {
	pub format: MoqAudioFormat,
	/// `None` delivers samples at the codec's native rate.
	pub sample_rate: Option<u32>,
	/// `None` delivers samples at the codec's native channel count.
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group, in
	/// milliseconds. Same congestion-control knob as
	/// [`MoqBroadcastConsumer::subscribe_media`](crate::consumer::MoqBroadcastConsumer::subscribe_media)'s
	/// `max_latency_ms`: when a group stalls and a newer group is more
	/// than this far ahead, the consumer skips. `None` keeps the
	/// moq-mux default of zero (skip aggressively). Named `_max` to
	/// leave room for a future `latency_min_ms` (jitter buffer).
	pub latency_max_ms: Option<u64>,
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
/// is PCM in the format declared by the [`MoqAudioEncoderInput`]
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
		input: MoqAudioEncoderInput,
		output: MoqAudioEncoderOutput,
	) -> Result<Arc<MoqAudioProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let producer = self.with_state(|state| {
			moq_audio::AudioProducer::new(
				&mut state.broadcast,
				state.catalog.clone(),
				name,
				moq_audio::EncoderInput {
					format: input.format.into(),
					sample_rate: input.sample_rate,
					channels: input.channels,
				},
				moq_audio::EncoderOutput {
					codec: output.codec.into(),
					sample_rate: output.sample_rate,
					channels: output.channels,
					bitrate: output.bitrate,
					frame_duration: Duration::from_millis(output.frame_duration_ms.into()),
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
		catalog_audio: crate::media::MoqAudio,
		output: MoqAudioDecoderOutput,
	) -> Result<Arc<MoqAudioConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let mut cfg = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			catalog_audio.sample_rate,
			catalog_audio.channel_count,
		);
		cfg.bitrate = catalog_audio.bitrate;
		cfg.description = catalog_audio.description.map(Into::into);
		cfg.container = catalog_audio.container.into();

		let consumer = moq_audio::AudioConsumer::new(
			self.inner(),
			&cfg,
			name,
			moq_audio::DecoderOutput {
				format: output.format.into(),
				sample_rate: output.sample_rate,
				channels: output.channels,
				latency_max: output.latency_max_ms.map(Duration::from_millis),
			},
		)?;

		Ok(Arc::new(MoqAudioConsumer {
			task: Task::new(ConsumerInner { consumer }),
		}))
	}
}

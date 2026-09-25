//! Raw-audio import/export via [`moq_audio`].
//!
//! Sibling to [`producer::MoqMediaProducer`](crate::producer::MoqMediaProducer)
//! and [`consumer::MoqMediaConsumer`](crate::consumer::MoqMediaConsumer):
//! those deal in already-encoded frames, these deal in PCM and run
//! Opus encode/decode inside the FFI boundary.

use std::sync::Arc;
use std::time::Duration;

use crate::bandwidth::{MoqBandwidth, MoqReservation};
use crate::consumer::MoqBroadcastConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::MoqBroadcastProducer;

/// Raw PCM sample format, mirroring WebCodecs `AudioData.format`.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqAudioSampleFormat {
	U8,
	S16,
	S32,
	F32,
	U8Planar,
	S16Planar,
	S32Planar,
	F32Planar,
}

impl From<MoqAudioSampleFormat> for moq_audio::Format {
	fn from(f: MoqAudioSampleFormat) -> Self {
		match f {
			MoqAudioSampleFormat::U8 => Self::U8,
			MoqAudioSampleFormat::S16 => Self::S16,
			MoqAudioSampleFormat::S32 => Self::S32,
			MoqAudioSampleFormat::F32 => Self::F32,
			MoqAudioSampleFormat::U8Planar => Self::U8Planar,
			MoqAudioSampleFormat::S16Planar => Self::S16Planar,
			MoqAudioSampleFormat::S32Planar => Self::S32Planar,
			MoqAudioSampleFormat::F32Planar => Self::F32Planar,
		}
	}
}

/// Audio codec selection for the encoder.
///
/// An immutable object so adding a codec later does not break callers
/// switching over a closed enum.
#[derive(uniffi::Object)]
pub struct MoqAudioCodec {
	inner: moq_audio::encode::Codec,
}

#[uniffi::export]
impl MoqAudioCodec {
	/// Opus (RFC 6716).
	#[uniffi::constructor]
	pub fn opus() -> Arc<Self> {
		Arc::new(Self {
			inner: moq_audio::encode::Codec::Opus,
		})
	}

	/// AAC-LC (`mp4a.40.2`) through the platform's encoder, at the input's rate
	/// and layout. A host without one refuses it when the producer is built.
	/// Its frames are 1024 samples, so leave `frame_duration_us` at 0.
	#[uniffi::constructor]
	pub fn aac() -> Arc<Self> {
		Arc::new(Self {
			inner: moq_audio::encode::Codec::Aac,
		})
	}
}

impl MoqAudioCodec {
	pub(crate) fn codec(&self) -> moq_audio::encode::Codec {
		self.inner
	}
}

/// PCM layout the caller will pass to [`MoqAudioProducer::write`].
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderInput {
	pub format: MoqAudioSampleFormat,
	pub sample_rate: u32,
	/// Interleaved channel count, which also names the speaker layout by the
	/// WAVE convention: 1 mono, 2 stereo, 3 2.1, 4 quad, 5 5.0, 6 5.1, 7 6.1,
	/// 8 7.1, in front left, front right, center, LFE, back, side order.
	pub channels: u32,
}

/// Codec-side configuration. `sample_rate` / `channels` `None` means
/// "match the input (snapping the rate up to a libopus-supported
/// value if necessary)".
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderOutput {
	pub codec: Arc<MoqAudioCodec>,
	#[uniffi(default = None)]
	pub sample_rate: Option<u32>,
	#[uniffi(default = None)]
	pub channels: Option<u32>,
	#[uniffi(default = None)]
	pub bitrate: Option<u32>,
	/// Encoded frame duration in microseconds. Opus accepts exactly
	/// 2500/5000/10000/20000/40000/60000 us, and the default 20 ms matches the
	/// JS publish path. 0 takes the codec's own frame, which AAC needs.
	#[uniffi(default = 20000)]
	pub frame_duration_us: u32,
}

/// PCM layout the caller wants out of [`MoqAudioConsumer::next`].
#[derive(uniffi::Record)]
pub struct MoqAudioDecoderOutput {
	pub format: MoqAudioSampleFormat,
	/// `None` delivers samples at the codec's native rate.
	#[uniffi(default = None)]
	pub sample_rate: Option<u32>,
	/// `None` delivers samples at the codec's native channel count. A count
	/// names its layout as [`MoqAudioEncoderInput::channels`] describes, and
	/// the decoder remixes to it.
	#[uniffi(default = None)]
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group, in
	/// microseconds. Same congestion-control knob as
	/// [`MoqSubscription::max_age_us`](crate::consumer::MoqSubscription::max_age_us):
	/// when a group stalls and a newer group is more than this far ahead,
	/// the consumer skips. `None` keeps the moq-mux default of zero (skip
	/// aggressively). Named `_max` to leave room for a future
	/// `min_buffer_us` (jitter-buffer floor), which is a distinct knob: this
	/// one bounds how stale a group may be, that one how much to hold before
	/// presenting.
	#[uniffi(default = None)]
	pub max_age_us: Option<u64>,
}

/// One audio frame: payload bytes plus a presentation timestamp.
///
/// PCM layout is fixed by the producer / consumer config, so it is
/// **not** carried per-frame. On the producer side `data` is raw PCM
/// in the configured `input_format`; on the consumer side it is raw
/// PCM in the configured `output_format`.
#[derive(uniffi::Record)]
pub struct MoqAudioFrame {
	/// Presentation timestamp of the first sample, in microseconds.
	pub timestamp_us: u64,
	/// The samples, in the configured PCM layout.
	pub data: Vec<u8>,
}

impl From<moq_audio::Frame> for MoqAudioFrame {
	fn from(f: moq_audio::Frame) -> Self {
		Self {
			// The binding surface carries plain microseconds, so flatten the
			// scaled `Timestamp` here. Saturating rather than erroring: this is a
			// frame we already decoded, and a u64 overflow needs a timestamp
			// ~580,000 years out.
			timestamp_us: u64::try_from(f.timestamp.as_micros()).unwrap_or(u64::MAX),
			data: f.data.to_vec(),
		}
	}
}

impl TryFrom<MoqAudioFrame> for moq_audio::Frame {
	type Error = moq_audio::Error;

	fn try_from(f: MoqAudioFrame) -> Result<Self, Self::Error> {
		Ok(Self::new(
			f.data.into(),
			moq_net::Timestamp::from_micros(f.timestamp_us)?,
		))
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
	inner: std::sync::Mutex<Option<moq_audio::encode::Producer<moq_mux::catalog::hang::Extra>>>,
	reservation: std::sync::Mutex<Option<Arc<MoqReservation>>>,
	/// Held so the reservation's registry outlives extra bandwidth handles.
	_bandwidth: Option<Arc<MoqBandwidth>>,
}

impl MoqAudioProducer {
	fn track_demand(&self) -> Result<moq_net::track::Demand, MoqError> {
		let guard = self.inner.lock().unwrap();
		let producer = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(producer.demand())
	}
}

#[uniffi::export]
impl MoqAudioProducer {
	/// Return the name of this audio track.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::enter();
		Ok(self.track_demand()?.name().to_string())
	}

	/// A watch-only handle to whether this audio track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		Ok(MoqTrackDemand::new(self.track_demand()?))
	}

	/// Wait until this audio track has at least one active consumer.
	///
	/// Prefer [`demand`](Self::demand), a handle that can wait without borrowing this producer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self.track_demand()?;
		crate::ffi::detached(async move { demand.used().await }).await
	}

	/// Wait until this audio track has no active consumers.
	///
	/// Prefer [`demand`](Self::demand), a handle that can wait without borrowing this producer.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self.track_demand()?;
		crate::ffi::detached(async move { demand.unused().await }).await
	}

	/// Re-anchor the timeline to the next frame's timestamp.
	///
	/// Call this before writing after an idle gap so the gap remains visible in
	/// the audio PTS instead of being compressed out by the running sample count.
	pub fn reset_epoch(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.reset_epoch();
		Ok(())
	}

	pub fn write(&self, frame: MoqAudioFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let frame = moq_audio::Frame::try_from(frame)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.write(&frame)?;
		Ok(())
	}

	/// This encoder's bandwidth reservation, if it was published against a
	/// [`MoqBandwidth`]. Dropping the handle does not release the claim; the
	/// producer holds it until [`finish`](Self::finish).
	pub fn reservation(&self) -> Option<Arc<MoqReservation>> {
		self.reservation.lock().unwrap().clone()
	}

	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let mut producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		self.reservation.lock().unwrap().take();
		producer.finish()?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Open an audio track on this broadcast. The catalog rendition is
	/// registered immediately so subscribers can find the track even
	/// before the first frame is written.
	///
	/// Pass `bandwidth` to reserve this track's bitrate against the session's
	/// allocator. Following the grant waits on the Rust audio producer; this
	/// call only claims the share so a co-resident video encoder sizes itself
	/// against what is left.
	#[uniffi::method(default(bandwidth = None))]
	pub fn encode_audio(
		&self,
		name: String,
		input: MoqAudioEncoderInput,
		output: MoqAudioEncoderOutput,
		bandwidth: Option<Arc<MoqBandwidth>>,
	) -> Result<Arc<MoqAudioProducer>, MoqError> {
		let _guard = crate::ffi::runtime().enter();

		let format = input.format.into();
		let layout = moq_audio::Layout::from_channels(input.channels)?;
		let mut input = moq_audio::encode::Input::new(input.sample_rate, layout);
		input.format = format;
		// The binding surface takes an explicit track name, so pin it here rather
		// than letting the codec derive one.
		let mut options = moq_audio::encode::Options::default();
		options.track = Some(name);
		options.settings = moq_audio::encode::Settings::from_input(output.codec.codec(), &input);
		if let Some(sample_rate) = output.sample_rate {
			options.settings.sample_rate = sample_rate;
		}
		if let Some(channels) = output.channels {
			options.settings.layout = moq_audio::Layout::from_channels(channels)?;
		}
		options.settings.bitrate = output.bitrate.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		if output.frame_duration_us != 0 {
			options.settings.frame_duration = Duration::from_micros(output.frame_duration_us.into());
		}
		if let Some(bandwidth) = &bandwidth {
			options.bandwidth = bandwidth.allocator().clone();
		}

		let producer = self.with_state(|state| {
			moq_audio::encode::Producer::new(&mut state.broadcast, state.catalog.clone(), input, &options)
				.map_err(Into::into)
		})?;

		// Producer::new does not reserve today (only capture does), so the
		// binding holds the claim itself. Passing the allocator into Options
		// means the Rust producer will reserve and follow whenever it starts to.
		let reservation = bandwidth
			.as_ref()
			.map(|bandwidth| bandwidth.reserve_demand(&producer.demand(), producer.bitrate().as_bps()));

		Ok(Arc::new(MoqAudioProducer {
			inner: std::sync::Mutex::new(Some(producer)),
			reservation: std::sync::Mutex::new(reservation),
			_bandwidth: bandwidth,
		}))
	}
}

// ---- Consumer ----

struct ConsumerInner {
	consumer: moq_audio::decode::Consumer,
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
	/// The next decoded frame, or `None` once the track ends.
	pub async fn next(&self) -> Result<Option<MoqAudioFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Make current and future reads return `Cancelled`.
	///
	/// Terminal: the decoder session is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

fn audio_config(catalog_audio: crate::media::MoqAudio) -> Result<hang::catalog::AudioConfig, MoqError> {
	let codec = catalog_audio.codec.parse().map_err(|_| MoqError::Unsupported)?;
	// What moq-audio's decoder opens. It rejects the rest itself, so this is only
	// here to report an unusable rendition as a plain Unsupported rather than a
	// wrapped codec error.
	if !matches!(
		&codec,
		hang::catalog::AudioCodec::Opus | hang::catalog::AudioCodec::AAC(_)
	) {
		return Err(MoqError::Unsupported);
	}

	let mut config = hang::catalog::AudioConfig::new(codec, catalog_audio.sample_rate, catalog_audio.channel_count);
	config.label = catalog_audio.label;
	config.bitrate = catalog_audio.bitrate;
	config.description = catalog_audio.description.map(Into::into);
	config.container = catalog_audio.container.into();
	Ok(config)
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to an audio track. `catalog_audio_config` comes from
	/// the catalog (see
	/// [`MoqCatalogConsumer::next`](crate::consumer::MoqCatalogConsumer::next));
	/// the codec is inferred from it. Only Opus and AAC-LC are supported.
	///
	/// A rendition whose [`broadcast`](crate::media::MoqAudio::broadcast) names another broadcast
	/// is subscribed there, so `name` is always read from the broadcast the catalog points at.
	pub async fn decode_audio(
		&self,
		name: String,
		catalog_audio: crate::media::MoqAudio,
		output: MoqAudioDecoderOutput,
	) -> Result<Arc<MoqAudioConsumer>, MoqError> {
		// Reject the codec before resolving: resolving reaches the origin, which can invoke a
		// dynamic handler and open an upstream subscription we would immediately drop.
		let reference = catalog_audio.broadcast.clone();
		let cfg = audio_config(catalog_audio)?;
		let broadcast = self.resolve_inner(reference.as_deref()).await?;

		let mut config = moq_audio::decode::Options::default();
		config.output.format = output.format.into();
		config.output.sample_rate = output.sample_rate;
		config.output.layout = output.channels.map(moq_audio::Layout::from_channels).transpose()?;
		config.max_age = output.max_age_us.map(Duration::from_micros).unwrap_or_default();

		let consumer = moq_audio::decode::Consumer::new(&broadcast, &cfg, name, config).await?;

		Ok(Arc::new(MoqAudioConsumer {
			task: Task::new(ConsumerInner { consumer }),
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::media::{MoqAudio, MoqContainer};

	fn catalog_audio(codec: &str) -> MoqAudio {
		MoqAudio {
			label: None,
			broadcast: None,
			codec: codec.to_string(),
			description: None,
			sample_rate: 48_000,
			channel_count: 2,
			bitrate: None,
			container: MoqContainer::Legacy,
		}
	}

	#[test]
	fn audio_config_accepts_opus() {
		let config = audio_config(catalog_audio("opus")).unwrap();
		assert!(matches!(config.codec, hang::catalog::AudioCodec::Opus));
	}

	#[test]
	fn audio_config_accepts_aac() {
		let config = audio_config(catalog_audio("mp4a.40.2")).unwrap();
		assert!(matches!(config.codec, hang::catalog::AudioCodec::AAC(_)));
	}

	#[test]
	fn audio_config_rejects_a_codec_the_decoder_lacks() {
		// In the catalog, and decodable in a browser, but not by moq-audio.
		let error = audio_config(catalog_audio("flac")).unwrap_err();
		assert!(matches!(error, MoqError::Unsupported));
	}

	#[test]
	fn audio_config_rejects_unknown_codec() {
		let error = audio_config(catalog_audio("unknown")).unwrap_err();
		assert!(matches!(error, MoqError::Unsupported));
	}

	/// `#[uniffi(default = ...)]` only takes a literal, so `frame_duration_us` restates
	/// moq-audio's default rather than reading it.
	#[test]
	fn default_frame_duration_matches_moq_audio() {
		assert_eq!(
			moq_audio::encode::Options::default().settings.frame_duration,
			Duration::from_micros(20_000),
			"update #[uniffi(default)] on MoqAudioEncoderOutput::frame_duration_us"
		);
	}
}

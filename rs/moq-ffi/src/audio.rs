//! Raw-audio import/export via [`moq_audio`].
//!
//! Sibling to [`media::MoqMediaTrackProducer`](crate::media::MoqMediaTrackProducer)
//! and [`media::MoqMediaTrackConsumer`](crate::media::MoqMediaTrackConsumer):
//! those deal in already-encoded frames, these deal in PCM and run
//! Opus and AAC encode/decode inside the FFI boundary.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::bandwidth::{MoqAllocator, MoqReservation};
use crate::consumer::MoqBroadcastConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;

/// Raw PCM sample format, mirroring WebCodecs `AudioData.format`.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqAudioSampleFormat {
	/// Unsigned 8-bit PCM samples, interleaved by channel.
	U8,
	/// Signed 16-bit PCM samples, interleaved by channel.
	S16,
	/// Signed 32-bit PCM samples, interleaved by channel.
	S32,
	/// 32-bit floating-point PCM samples, interleaved by channel.
	F32,
	/// Unsigned 8-bit PCM samples, one plane per channel.
	U8Planar,
	/// Signed 16-bit PCM samples, one plane per channel.
	S16Planar,
	/// Signed 32-bit PCM samples, one plane per channel.
	S32Planar,
	/// 32-bit floating-point PCM samples, one plane per channel.
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
	/// Its frames are 1024 samples, so leave `frame_duration` unset.
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
	/// The raw PCM sample layout.
	pub format: MoqAudioSampleFormat,
	/// The number of samples per second.
	pub sample_rate: u32,
	/// The channel count, which also names the speaker layout by the
	/// WAVE convention: 1 mono, 2 stereo, 3 2.1, 4 quad, 5 5.0, 6 5.1, 7 6.1,
	/// 8 7.1, in front left, front right, center, LFE, back, side order.
	pub channels: u32,
}

/// Codec-side configuration for a [`MoqAudioProducer`] or a standalone [`MoqAudioEncoder`].
/// Opus snaps the encoded rate up to one libopus supports.
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderOutput {
	/// The codec used to encode this rendition.
	pub codec: Arc<MoqAudioCodec>,
	/// The encoded samples per second. Absent matches a producer's input rate,
	/// or 48000 for a standalone encoder, which has no input to match.
	#[uniffi(default = None)]
	pub sample_rate: Option<u32>,
	/// The encoded channel count. Absent matches a producer's input, or 2 for a
	/// standalone encoder, which has no input to match.
	#[uniffi(default = None)]
	pub channels: Option<u32>,
	/// The target bitrate in bits per second, or absent for the codec default.
	#[uniffi(default = None)]
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts exactly 2.5, 5, 10, 20, 40, or 60 ms;
	/// 10 ms trades twice the packet rate for lower latency. Absent takes the
	/// codec's own: 20 ms for Opus and 1024 samples for AAC.
	#[uniffi(default = None)]
	pub frame_duration: Option<Duration>,
}

/// Input, codec settings, and publication ownership for an audio producer.
#[derive(uniffi::Record)]
pub struct MoqAudioProducerOptions {
	/// The raw PCM the caller writes.
	pub input: MoqAudioEncoderInput,
	/// The codec settings used to encode that PCM.
	pub output: MoqAudioEncoderOutput,
	/// Published track name; `None` derives one from the codec.
	#[uniffi(default = None)]
	pub track: Option<String>,
	/// Session bandwidth allocator; absent reserves no share.
	#[uniffi(default = None)]
	pub bandwidth: Option<Arc<MoqAllocator>>,
}

impl MoqAudioEncoderOutput {
	/// Apply an explicit frame duration to `config`, keeping the codec's own when absent.
	fn packetize(&self, mut config: moq_audio::encode::Config) -> moq_audio::encode::Config {
		if let Some(frame_duration) = self.frame_duration {
			config.frame_duration = frame_duration;
		}
		config
	}
}

/// PCM layout the caller wants out of [`MoqAudioConsumer::next`].
#[derive(uniffi::Record)]
pub struct MoqAudioDecoderOutput {
	/// The raw PCM sample layout.
	pub format: MoqAudioSampleFormat,
	/// `None` delivers samples at the codec's native rate.
	#[uniffi(default = None)]
	pub sample_rate: Option<u32>,
	/// `None` delivers samples at the codec's native channel count. A count
	/// names its layout as [`MoqAudioEncoderInput::channels`] describes, and
	/// the decoder remixes to it.
	#[uniffi(default = None)]
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group. Same
	/// congestion-control knob as
	/// [`MoqSubscription::max_delay`](crate::consumer::MoqSubscription::max_delay):
	/// when a group stalls and a newer group is more than this far ahead,
	/// the consumer skips. `None` keeps the moq-mux default of zero (skip
	/// aggressively). Distinct from [`delay`](Self::delay): this one
	/// bounds how stale a group may be and adds no buffering.
	#[uniffi(default = None)]
	pub max_delay: Option<Duration>,
	/// A fixed playout delay, or `None` to estimate it from arrival timing. This
	/// is the jitter buffer to hold ahead of the playhead, reported by
	/// [`MoqAudioConsumer::delay`]. A fixed delay beyond the track's retention
	/// is refused.
	#[uniffi(default = None)]
	pub delay: Option<Duration>,
	/// Where to start on a track that already holds groups, or `None` for the oldest. A live
	/// player picks [`Latest`](crate::media::MoqDecodeStart::Latest) so a reopened decoder
	/// skips the backlog.
	#[uniffi(default = None)]
	pub start: Option<crate::media::MoqDecodeStart>,
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

/// One item of a [`MoqAudioConsumer`] read, in delivery order: decoded PCM, or a break in it.
///
/// Mirrors `moq_audio::decode::Read`, the same shape as
/// [`MoqVideoRead`](crate::video::MoqVideoRead), so a player handles loss and ends alike
/// for every media type.
#[derive(uniffi::Enum)]
pub enum MoqAudioRead {
	/// Decoded PCM.
	Frame {
		/// The decoded samples.
		frame: MoqAudioFrame,
	},
	/// Media after what was already read was lost: a group was skipped or never arrived,
	/// or a packet's timestamp did not continue the previous one. What follows does not
	/// continue what came before.
	Gap {
		/// The furthest presentation point already read, in microseconds.
		timestamp_us: u64,
		/// An upper bound on how long until the next frame resumes.
		duration: Duration,
	},
	/// The publisher stopped its encoder: it paused, switched source, or finished.
	///
	/// Frames that follow and start before `end_us` are the stopped encoder's remaining
	/// media; the first frame at or past it comes from a fresh encoder.
	End {
		/// Where the stopped encoder's media stops, in microseconds.
		end_us: u64,
	},
}

impl TryFrom<moq_audio::decode::Read> for MoqAudioRead {
	type Error = MoqError;

	fn try_from(read: moq_audio::decode::Read) -> Result<Self, MoqError> {
		use crate::consumer::timestamp_us;
		Ok(match read {
			moq_audio::decode::Read::Frame(frame) => Self::Frame { frame: frame.into() },
			moq_audio::decode::Read::Gap { timestamp, duration } => Self::Gap {
				timestamp_us: timestamp_us(timestamp)?,
				duration: crate::ffi::duration(duration.into()),
			},
			moq_audio::decode::Read::End { end } => Self::End {
				end_us: timestamp_us(end)?,
			},
		})
	}
}

/// One packet emitted by a codec-only audio encoder.
#[derive(uniffi::Record)]
pub struct MoqAudioPacket {
	/// Encoded codec payload.
	pub data: Vec<u8>,
}

/// The terminal packets and decoded padding emitted when an audio encoder finishes.
#[derive(uniffi::Record)]
pub struct MoqAudioEncoderFinished {
	/// Packets emitted while draining the encoder.
	pub packets: Vec<MoqAudioPacket>,
	/// Padded samples to discard from the decoded tail.
	pub discard_padding: u64,
}

/// A codec-only audio encoder, independent of any broadcast.
#[derive(uniffi::Object)]
pub struct MoqAudioEncoder {
	inner: std::sync::Mutex<Option<moq_audio::encode::Encoder>>,
}

fn encode_config(output: &MoqAudioEncoderOutput) -> Result<moq_audio::encode::Config, MoqError> {
	let sample_rate = output.sample_rate.unwrap_or(48_000);
	let layout = moq_audio::Layout::from_channels(output.channels.unwrap_or(2))?;
	let input = moq_audio::encode::Input::new(sample_rate, layout);
	let mut config = moq_audio::encode::Config::from_input(output.codec.codec(), &input);
	config.bitrate = output.bitrate.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
	Ok(output.packetize(config))
}

fn audio_packets(packets: impl IntoIterator<Item = moq_audio::encode::Encoded>) -> Vec<MoqAudioPacket> {
	packets
		.into_iter()
		.map(|packet| MoqAudioPacket {
			data: packet.payload.to_vec(),
		})
		.collect()
}

#[uniffi::export]
impl MoqAudioEncoder {
	/// Create an encoder from codec-side output settings.
	#[uniffi::constructor]
	pub fn new(output: MoqAudioEncoderOutput) -> Result<Arc<Self>, MoqError> {
		let encoder = moq_audio::encode::Encoder::new(&encode_config(&output)?)?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(encoder)),
		}))
	}

	/// Number of interleaved samples per channel required by `encode`.
	pub fn frame_size(&self) -> Result<u64, MoqError> {
		Ok(self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.frame_size() as u64)
	}

	/// The init that publishes this encoder's packets through
	/// [`MoqMediaTrackProducer::audio`](crate::media::MoqMediaTrackProducer::audio):
	/// an OpusHead or an AudioSpecificConfig.
	pub fn track_init(&self) -> Result<crate::media::MoqAudioInit, MoqError> {
		let guard = self.inner.lock().unwrap();
		let encoder = guard.as_ref().ok_or(MoqError::Closed)?;
		let format = match encoder.codec() {
			moq_audio::encode::Codec::Opus => crate::media::MoqAudioFormat::Opus,
			moq_audio::encode::Codec::Aac => crate::media::MoqAudioFormat::Aac,
			codec => return Err(moq_audio::Error::Unsupported(format!("no audio init for {codec}")).into()),
		};
		let data = encoder.catalog().description.ok_or(MoqError::Unsupported)?;
		Ok(crate::media::MoqAudioInit {
			format,
			data: data.to_vec(),
			label: None,
		})
	}

	/// Retune the live encoder in bits per second.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		guard
			.as_mut()
			.ok_or(MoqError::Closed)?
			.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate))?;
		Ok(())
	}

	/// Encode one codec-sized interleaved f32 PCM frame.
	pub fn encode(&self, pcm: Vec<f32>) -> Result<MoqAudioPacket, MoqError> {
		let mut guard = self.inner.lock().unwrap();
		let packet = guard.as_mut().ok_or(MoqError::Closed)?.encode(&pcm)?;
		Ok(MoqAudioPacket {
			data: packet.payload.to_vec(),
		})
	}

	/// Drain the encoder, optionally padding one final partial frame.
	pub fn finish(&self, pcm: Vec<f32>) -> Result<MoqAudioEncoderFinished, MoqError> {
		let encoder = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		let finish = encoder.finish(&pcm)?;
		Ok(MoqAudioEncoderFinished {
			discard_padding: finish.discard_padding() as u64,
			packets: audio_packets(finish.into_packets()),
		})
	}
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
/// Constructed from a broadcast's media catalog. Each
/// [`write`](Self::write) accepts an [`MoqAudioFrame`] whose `data`
/// is PCM in the format declared by the [`MoqAudioEncoderInput`]
/// passed at publish time.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqAudioProducer {
	inner: std::sync::Mutex<Option<moq_audio::encode::Producer>>,
	reservation: std::sync::Mutex<Option<Arc<MoqReservation>>>,
	/// Held so the reservation's registry outlives extra bandwidth handles.
	_bandwidth: Option<Arc<MoqAllocator>>,
	children: crate::producer::Children,
}

// Dropped or closed before its finish, the track ends the way a publisher leaving does.
impl crate::producer::Child for MoqAudioProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::runtime().enter();
		if let Some(producer) = self.inner.lock().unwrap().take() {
			self.reservation.lock().unwrap().take();
			producer.abort(err);
		}
	}
}

impl Drop for MoqAudioProducer {
	fn drop(&mut self) {
		crate::producer::Child::abort(self, moq_net::Error::Unroutable);
	}
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
	/// A watch-only handle to whether this audio track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		Ok(MoqTrackDemand::new(self.track_demand()?))
	}

	/// Re-anchor the timeline to the next frame's timestamp.
	///
	/// Call this before writing after an idle gap so the gap remains visible in
	/// the audio PTS instead of being compressed out by the running sample count.
	pub fn reanchor(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.reanchor();
		Ok(())
	}

	/// Encode and publish one PCM frame.
	pub fn write(&self, frame: MoqAudioFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let frame = moq_audio::Frame::try_from(frame)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.write(&frame)?;
		Ok(())
	}

	/// This encoder's bandwidth reservation, if it was published against a
	/// [`MoqAllocator`]. Dropping the handle does not release the claim; the
	/// producer holds it until [`finish`](Self::finish).
	pub fn reservation(&self) -> Option<Arc<MoqReservation>> {
		self.reservation.lock().unwrap().clone()
	}

	/// Drain the encoder and finish the audio track.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		// Held through the flush, so a racing broadcast close waits it out instead of
		// missing a track that still publishes.
		let mut inner = self.inner.lock().unwrap();
		let mut producer = inner.take().ok_or(MoqError::Closed)?;
		self.reservation.lock().unwrap().take();
		if let Err(err) = producer.finish() {
			// Readers see the failure rather than `Dropped`, as a failed video drain does.
			producer.abort(moq_net::Error::Transport(err.to_string()));
			return Err(err.into());
		}
		self.children.keep(producer);
		Ok(())
	}
}

#[uniffi::export]
impl MoqAudioProducer {
	/// Open an audio track on the catalog's broadcast. The catalog rendition is
	/// registered immediately so subscribers can find the track even
	/// before the first frame is written.
	///
	/// Set `options.bandwidth` to reserve this track's bitrate against the session's
	/// allocator. Following the grant waits on the Rust audio producer; this
	/// call only claims the share so a co-resident video encoder sizes itself
	/// against what is left.
	#[uniffi::constructor]
	pub fn new(
		catalog: &crate::media::MoqMediaCatalogProducer,
		options: MoqAudioProducerOptions,
	) -> Result<Arc<MoqAudioProducer>, MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let broadcast = catalog.broadcast()?;

		let MoqAudioProducerOptions {
			input,
			output,
			track,
			bandwidth,
		} = options;
		let format = input.format.into();
		let layout = moq_audio::Layout::from_channels(input.channels)?;
		let mut input = moq_audio::encode::Input::new(input.sample_rate, layout);
		input.format = format;
		// The binding surface takes an explicit track name, so pin it here rather
		// than letting the codec derive one.
		let mut options = moq_audio::encode::Options::default();
		options.track = track;
		options.config = moq_audio::encode::Config::from_input(output.codec.codec(), &input);
		if let Some(sample_rate) = output.sample_rate {
			options.config.sample_rate = sample_rate;
		}
		if let Some(channels) = output.channels {
			options.config.layout = moq_audio::Layout::from_channels(channels)?;
		}
		options.config.bitrate = output.bitrate.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		options.config = output.packetize(options.config);
		if output.frame_duration.is_none() && output.codec.codec() == moq_audio::encode::Codec::Aac {
			// from_input sized this at the input rate. The codec rate may be the
			// override above, and AAC's own frame is 1024 samples of that rate.
			let mut rated = input.clone();
			rated.sample_rate = options.config.sample_rate;
			options.config.frame_duration =
				moq_audio::encode::Config::from_input(moq_audio::encode::Codec::Aac, &rated).frame_duration;
		}
		if let Some(bandwidth) = &bandwidth {
			options.bandwidth = bandwidth.inner().clone();
		}

		let (producer, children) = broadcast.with_state(|state| {
			let producer =
				moq_audio::encode::Producer::new(&mut state.broadcast, state.catalog.clone(), input, &options)?;
			Ok((producer, state.children.clone()))
		})?;

		// Producer::new does not reserve today (only capture does), so the
		// binding holds the claim itself. Passing the allocator into Options
		// means the Rust producer will reserve and follow whenever it starts to.
		let reservation = bandwidth
			.as_ref()
			.map(|bandwidth| bandwidth.reserve_demand(&producer.demand(), producer.bitrate().as_bps()));

		children.adopt(Arc::new(MoqAudioProducer {
			inner: std::sync::Mutex::new(Some(producer)),
			reservation: std::sync::Mutex::new(reservation),
			_bandwidth: bandwidth,
			children: children.clone(),
		}))
	}
}

// ---- Consumer ----

struct ConsumerInner {
	consumer: moq_audio::decode::Consumer,
	delay: Arc<AtomicU64>,
}

impl ConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqAudioRead>, MoqError> {
		let read = self.consumer.read().await;
		self.delay.store(nanos(self.consumer.delay()), Ordering::Relaxed);
		read?.map(MoqAudioRead::try_from).transpose()
	}
}

/// The playout delay as the nanoseconds [`MoqAudioConsumer::delay`] reads back.
fn nanos(duration: Duration) -> u64 {
	crate::ffi::duration(duration).as_nanos() as u64
}

/// Consumer for a raw-audio track.
#[derive(uniffi::Object)]
pub struct MoqAudioConsumer {
	task: Task<ConsumerInner>,
	/// Mirrors the decoder's delay after each read, so it stays readable while
	/// [`Self::next`] holds the decoder.
	delay: Arc<AtomicU64>,
}

#[uniffi::export]
impl MoqAudioConsumer {
	/// The next decoded frame, gap, or encoder end, or `None` once the track ends.
	pub async fn next(&self) -> Result<Option<MoqAudioRead>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// The playout delay to hold ahead of the playhead: the fixed
	/// [`MoqAudioDecoderOutput::delay`], or the estimate from every frame read
	/// so far.
	///
	/// The estimate measures when [`Self::next`] returns each frame, so call it
	/// again as soon as the previous frame is handled rather than pacing reads
	/// to the speaker.
	pub fn delay(&self) -> Duration {
		Duration::from_nanos(self.delay.load(Ordering::Relaxed))
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
		hang::catalog::AudioCodec::Opus | hang::catalog::AudioCodec::AAC(_) | hang::catalog::AudioCodec::Pcm
	) {
		return Err(MoqError::Unsupported);
	}

	let mut config = hang::catalog::AudioConfig::new(codec, catalog_audio.sample_rate, catalog_audio.channel_count);
	config.label = catalog_audio.label;
	config.track = catalog_audio.track;
	config.bitrate = catalog_audio.bitrate;
	config.description = catalog_audio.description.map(Into::into);
	config.container = catalog_audio.container.into();
	config.jitter = catalog_audio.jitter;
	Ok(config)
}

#[uniffi::export]
impl MoqAudioConsumer {
	/// Subscribe to an audio track. `catalog_audio_config` comes from
	/// the catalog (see
	/// [`MoqMediaCatalogConsumer::next`](crate::media::MoqMediaCatalogConsumer::next));
	/// the codec is inferred from it. Only Opus, AAC-LC, and PCM are supported.
	///
	/// A [disabled](crate::media::MoqAudio::enabled) rendition subscribes too, and decodes once its
	/// publisher re-enables it: a muted speaker's listener stays subscribed, so the unmute plays
	/// from its first frame. Don't pick one, but keep one already playing while no enabled
	/// rendition is on offer.
	///
	/// `id` is the rendition's catalog key. Its [`track`](crate::media::MoqAudio::track), or `id`
	/// when unset, is subscribed in the broadcast its
	/// [`broadcast`](crate::media::MoqAudio::broadcast) reference names.
	#[uniffi::constructor]
	pub async fn subscribe(
		broadcast: &MoqBroadcastConsumer,
		id: String,
		catalog_audio: crate::media::MoqAudio,
		output: MoqAudioDecoderOutput,
	) -> Result<Arc<MoqAudioConsumer>, MoqError> {
		// Reject the codec before resolving: resolving reaches the origin, which can invoke a
		// dynamic handler and open an upstream subscription we would immediately drop.
		let reference = catalog_audio.broadcast.clone();
		let epoch = catalog_audio.epoch.clone();
		let cfg = audio_config(catalog_audio)?;
		let resolved = broadcast.resolve_inner(reference.as_deref(), epoch.as_deref()).await?;

		let mut config = moq_audio::decode::Options::default();
		config.output.format = output.format.into();
		config.output.sample_rate = output.sample_rate;
		config.output.layout = output.channels.map(moq_audio::Layout::from_channels).transpose()?;
		config.max_delay = output.max_delay.unwrap_or_default();
		config.delay = output.delay;
		config.start = match output.start.unwrap_or_default() {
			crate::media::MoqDecodeStart::Oldest => moq_audio::decode::Start::Oldest,
			crate::media::MoqDecodeStart::Latest => moq_audio::decode::Start::Latest,
		};

		let consumer = broadcast
			.driver
			.watch(async { Ok(moq_audio::decode::Consumer::new(&resolved, &cfg, id, config).await?) })
			.await?;

		let delay = Arc::new(AtomicU64::new(nanos(consumer.delay())));
		Ok(Arc::new(MoqAudioConsumer {
			task: Task::new(ConsumerInner {
				consumer,
				delay: delay.clone(),
			})
			.supervised(broadcast.driver.clone()),
			delay,
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
			epoch: None,
			track: None,
			codec: codec.to_string(),
			description: None,
			sample_rate: 48_000,
			channel_count: 2,
			bitrate: None,
			enabled: true,
			container: MoqContainer::Legacy,
			jitter: None,
		}
	}

	/// A consumer holds a disabled rendition it already plays, so one converts like any other.
	#[test]
	fn audio_config_accepts_disabled_renditions() {
		let mut config = catalog_audio("opus");
		config.enabled = false;
		assert!(audio_config(config).is_ok());
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
	fn audio_config_accepts_pcm() {
		let config = audio_config(catalog_audio("pcm")).unwrap();
		assert!(matches!(config.codec, hang::catalog::AudioCodec::Pcm));
	}

	#[test]
	fn audio_config_rejects_a_codec_the_decoder_lacks() {
		// In the catalog, and decodable in a browser, but not by moq-audio.
		let error = audio_config(catalog_audio("flac")).unwrap_err();
		assert!(matches!(error, MoqError::Unsupported));
	}

	/// The advertised jitter floors the estimated playout delay, so it must survive the trip
	/// through the FFI record.
	#[test]
	fn audio_config_keeps_the_advertised_jitter() {
		let mut audio = catalog_audio("opus");
		audio.jitter = Some(Duration::from_millis(60));
		let config = audio_config(audio).unwrap();
		assert_eq!(config.jitter, Some(Duration::from_millis(60)));
	}

	#[test]
	fn audio_config_rejects_unknown_codec() {
		let error = audio_config(catalog_audio("unknown")).unwrap_err();
		assert!(matches!(error, MoqError::Unsupported));
	}

	#[tokio::test(start_paused = true)]
	async fn retired_audio_producers_keep_clean_tails_for_late_readers() {
		let broadcast = crate::producer::MoqBroadcastProducer::new().unwrap();
		let catalog = crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		let reader = broadcast.consume_inner().unwrap();
		for index in 0..4 {
			let audio = MoqAudioProducer::new(
				&catalog,
				MoqAudioProducerOptions {
					input: MoqAudioEncoderInput {
						format: MoqAudioSampleFormat::F32,
						sample_rate: 48_000,
						channels: 1,
					},
					output: MoqAudioEncoderOutput {
						codec: MoqAudioCodec::opus(),
						sample_rate: None,
						channels: None,
						bitrate: None,
						frame_duration: None,
					},
					track: Some(format!("audio-{index}")),
					bandwidth: None,
				},
			)
			.unwrap();
			// A partial codec frame makes finish publish the final packet and lookahead.
			audio
				.write(MoqAudioFrame {
					timestamp_us: 0,
					data: vec![0.1_f32; 860].into_iter().flat_map(f32::to_ne_bytes).collect(),
				})
				.unwrap();
			audio.finish().unwrap();
			drop(audio);
			broadcast
				.with_state(|state| {
					assert_eq!(state.catalog.snapshot().audio.renditions.len(), index + 1);
					Ok(())
				})
				.unwrap();
		}
		catalog.finish().unwrap();
		for index in 0..4 {
			let mut track = reader
				.track(&format!("audio-{index}"))
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let mut frames = 0;
			while let Some(mut group) = track.recv_group().await.unwrap() {
				while group.read_frame().await.unwrap().is_some() {
					frames += 1;
				}
			}
			assert!(frames > 0);
		}
	}

	#[test]
	fn codec_only_encoder_uses_the_codec_frame_default() {
		let output = MoqAudioEncoderOutput {
			codec: MoqAudioCodec::opus(),
			sample_rate: None,
			channels: None,
			bitrate: None,
			frame_duration: None,
		};
		let encoder = MoqAudioEncoder::new(output).unwrap();
		let samples = encoder.frame_size().unwrap() as usize * 2;
		assert!(!encoder.encode(vec![0.0; samples]).unwrap().data.is_empty());
		encoder.finish(Vec::new()).unwrap();
		assert!(matches!(encoder.frame_size(), Err(MoqError::Closed)));
	}

	fn opus_output(frame_duration: Option<Duration>) -> MoqAudioEncoderOutput {
		MoqAudioEncoderOutput {
			codec: MoqAudioCodec::opus(),
			sample_rate: Some(48_000),
			channels: Some(2),
			bitrate: None,
			frame_duration,
		}
	}

	#[test]
	fn codec_only_encoder_packetizes_by_frame_duration() {
		for (frame_duration, frame_size) in [(None, 960), (Some(10), 480), (Some(20), 960)] {
			let encoder = MoqAudioEncoder::new(opus_output(frame_duration.map(Duration::from_millis))).unwrap();
			assert_eq!(encoder.frame_size().unwrap(), frame_size, "{frame_duration:?} ms");
		}
	}

	#[test]
	fn codec_only_encoder_reports_its_opus_head() {
		let encoder = MoqAudioEncoder::new(opus_output(None)).unwrap();
		let init = encoder.track_init().unwrap();
		assert!(matches!(init.format, crate::media::MoqAudioFormat::Opus));
		assert_eq!(&init.data[..8], b"OpusHead");
		// Channel count, then a nonzero pre-skip for the codec lookahead.
		assert_eq!(init.data[9], 2);
		assert_ne!(u16::from_le_bytes([init.data[10], init.data[11]]), 0);
	}

	#[test]
	fn codec_only_encoder_retunes_its_bitrate() {
		let encoder = MoqAudioEncoder::new(opus_output(None)).unwrap();
		encoder.set_bitrate(32_000).unwrap();
		let samples = encoder.frame_size().unwrap() as usize * 2;
		assert!(!encoder.encode(vec![0.0; samples]).unwrap().data.is_empty());
		encoder.finish(Vec::new()).unwrap();
		assert!(matches!(encoder.set_bitrate(64_000), Err(MoqError::Closed)));
		assert!(matches!(encoder.track_init(), Err(MoqError::Closed)));
	}
}

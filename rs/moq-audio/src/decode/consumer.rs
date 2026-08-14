//! Subscribe to an encoded audio track and emit raw PCM.

use bytes::Bytes;

use super::decoder::{Config, Decoder};
use crate::resample::{Resampler, remix, validate_channels};
use crate::{Error, Frame};

/// Subscribe to a moq-mux audio track and emit decoded PCM in the layout
/// declared by [`Config`].
///
/// The mirror of [`encode::Producer`](crate::encode::Producer): output format /
/// sample rate / channel count are fixed at construction, and
/// [`read`](Self::read) returns plain [`Frame`]s.
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	resampler: Option<Resampler>,
	config: Config,
	resolved_sample_rate: u32,
	resolved_channels: u32,
	/// One past the last sample handed to the resampler, so the tail it is still
	/// holding at end of track can be stamped. `None` until the first packet.
	tail: Option<moq_net::Timestamp>,
	/// Timestamp of the first encoded packet, used to interpret a terminal marker.
	epoch: Option<moq_net::Timestamp>,
	/// Codec-rate terminal frames emitted since `terminal_start`.
	frames_decoded: usize,
	/// Logical endpoint carried by an empty legacy frame before terminal packets.
	end: Option<moq_net::Timestamp>,
	/// Presentation time of the first decoded terminal frame.
	terminal_start: Option<moq_net::Timestamp>,
	/// Last container discontinuity applied to codec and resampler state.
	discontinuity: u64,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, using the catalog entry to pick the
	/// codec.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &hang::catalog::AudioConfig,
		name: impl Into<String>,
		config: Config,
	) -> Result<Self, Error> {
		let decoder = Decoder::new(catalog)?;
		let sample_rate = config.sample_rate.unwrap_or_else(|| decoder.sample_rate());
		let channels = config.channels.unwrap_or_else(|| decoder.channel_count());
		validate_channels(channels)?;

		let resampler = if sample_rate == decoder.sample_rate() {
			None
		} else {
			let chunk_frames = (decoder.sample_rate() as usize * 20) / 1000;
			Some(Resampler::new(
				decoder.sample_rate(),
				sample_rate,
				decoder.channel_count(),
				chunk_frames,
			)?)
		};

		let name = name.into();
		let track = broadcast
			.track(&name)?
			.subscribe(
				moq_net::track::Subscription::default()
					.with_priority(hang::catalog::PRIORITY.audio)
					.with_max_age(config.max_age),
			)
			.await?;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(&catalog.container)?;
		let track = moq_mux::container::Consumer::new(track, container);

		Ok(Self {
			decoder,
			track,
			resampler,
			config,
			resolved_sample_rate: sample_rate,
			resolved_channels: channels,
			tail: None,
			epoch: None,
			frames_decoded: 0,
			end: None,
			terminal_start: None,
			discontinuity: 0,
		})
	}

	/// The config this consumer was built with.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// Sample rate samples are actually delivered at, which is
	/// [`Config::sample_rate`] resolved against the catalog.
	pub fn sample_rate(&self) -> u32 {
		self.resolved_sample_rate
	}

	/// Channel count samples are actually delivered at, which is
	/// [`Config::channels`] resolved against the catalog.
	pub fn channels(&self) -> u32 {
		self.resolved_channels
	}

	/// Read the next decoded PCM frame, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			let mux_frame = self.track.read().await?;
			self.apply_discontinuity()?;
			let Some(mux_frame) = mux_frame else {
				return self.flush();
			};

			if let Some(end) = self.track.end()
				&& self.end != Some(end)
			{
				self.end = Some(end);
				self.frames_decoded = 0;
				self.terminal_start = None;
			}

			let rate = self.decoder.sample_rate();
			let epoch = *self.epoch.get_or_insert(mux_frame.timestamp);
			let mut decoded = self.decoder.decode(&mux_frame.payload)?;
			if let Some(end) = self.end {
				let terminal_start = *self
					.terminal_start
					.get_or_insert(rewind(mux_frame.timestamp, self.decoder.delay(), rate)?.max(epoch));
				let total = frames_between(terminal_start, end, rate)?;
				let remaining = total.saturating_sub(self.frames_decoded);
				decoded.truncate(remaining.saturating_mul(self.decoder.channel_count() as usize));
			}

			let frames = decoded.len() / self.decoder.channel_count().max(1) as usize;
			let decoded_at = if let Some(terminal_start) = self.terminal_start {
				advance(terminal_start, self.frames_decoded, rate)?
			} else {
				mux_frame.timestamp
			};
			if self.end.is_some() {
				self.frames_decoded += frames;
			}
			if decoded.is_empty() {
				continue;
			}

			let (pcm, timestamp) = match self.resampler.as_mut() {
				// The resampler works in fixed chunks, so it holds back whatever didn't
				// fill one. What comes out next starts with those held-back samples, which
				// arrived before this packet did: stamping it with this packet's timestamp
				// would place the audio late by up to a chunk, sawtoothing A/V sync.
				Some(r) => {
					let pending = r.pending_frames();
					let skipped = r.skipped();
					let pcm = r.process(&decoded)?;
					(pcm, self.starts_at(decoded_at, pending, skipped, rate)?)
				}
				None => (decoded, decoded_at),
			};

			self.tail = Some(advance(decoded_at, frames, rate)?);

			return Ok(Some(self.frame(pcm, timestamp)?));
		}
	}

	/// Reset every stateful decode stage before the first packet of a new epoch.
	fn apply_discontinuity(&mut self) -> Result<(), Error> {
		let discontinuity = self.track.discontinuity();
		if discontinuity == self.discontinuity {
			return Ok(());
		}

		self.discontinuity = discontinuity;
		self.decoder.reset()?;
		if let Some(resampler) = self.resampler.as_mut() {
			resampler.reset();
		}
		self.tail = None;
		self.epoch = None;
		self.frames_decoded = 0;
		self.end = None;
		self.terminal_start = None;
		Ok(())
	}

	/// The tail the resampler is still holding when the track ends, once.
	///
	/// Without it the last partial chunk is dropped, which is up to a chunk of
	/// audio missing from the end of every resampled track. Flushing consumes the
	/// resampler, which is what makes calling this on every later poll return
	/// `None` rather than more tails.
	fn flush(&mut self) -> Result<Option<Frame>, Error> {
		let (Some(resampler), Some(tail)) = (self.resampler.take(), self.tail) else {
			return Ok(None);
		};

		let pending = resampler.pending_frames();
		let skipped = resampler.skipped();
		let pcm = resampler.flush()?;
		if pcm.is_empty() {
			return Ok(None);
		}

		let timestamp = self.starts_at(tail, pending, skipped, self.decoder.sample_rate())?;
		Ok(Some(self.frame(pcm, timestamp)?))
	}

	/// Where the output the resampler is about to hand back actually begins.
	///
	/// Two things sit between a packet's timestamp and the audio that comes out of
	/// it. The resampler is holding `pending` input frames from before this packet,
	/// which the output starts with. And it has dropped `skipped` output frames of
	/// its own startup silence, so everything it emits from then on runs that much
	/// short of the input it was built from. Reach back over both, each in its own
	/// rate, or the output is stamped after the audio it contains.
	fn starts_at(
		&self,
		timestamp: moq_net::Timestamp,
		pending: usize,
		skipped: usize,
		rate: u32,
	) -> Result<moq_net::Timestamp, Error> {
		let timestamp = rewind(timestamp, pending, rate)?;
		rewind(timestamp, skipped, self.resolved_sample_rate)
	}

	/// Remix and pack decoded PCM into an output frame.
	fn frame(&self, pcm: Vec<f32>, timestamp: moq_net::Timestamp) -> Result<Frame, Error> {
		let pcm = if self.decoder.channel_count() == self.resolved_channels {
			pcm
		} else {
			remix(&pcm, self.decoder.channel_count(), self.resolved_channels)?
		};

		let bytes = self.config.format.from_interleaved_f32(&pcm, self.resolved_channels)?;
		Ok(Frame {
			timestamp,
			data: Bytes::from(bytes),
		})
	}
}

/// `timestamp` moved forward by `frames` at `sample_rate`, in its own timescale.
fn advance(timestamp: moq_net::Timestamp, frames: usize, sample_rate: u32) -> Result<moq_net::Timestamp, Error> {
	if frames == 0 {
		return Ok(timestamp);
	}

	let offset = moq_net::Timestamp::from_scale(frames as u64, sample_rate as u64)?.convert(timestamp.scale())?;
	Ok(timestamp.checked_add(offset)?)
}

/// Codec-rate frames in the interval, rounding a microsecond marker to the nearest frame.
fn frames_between(start: moq_net::Timestamp, end: moq_net::Timestamp, sample_rate: u32) -> Result<usize, Error> {
	let duration = end.checked_sub(start)?;
	let frames = (std::time::Duration::from(duration).as_nanos() * sample_rate as u128 + 500_000_000) / 1_000_000_000;
	usize::try_from(frames).map_err(|_| Error::Unsupported("audio duration does not fit in memory".into()))
}

/// `timestamp` moved back by `frames` at `sample_rate`, in its own timescale.
///
/// Saturates at zero rather than failing: a publisher whose first timestamps
/// don't advance is odd, but it isn't a reason to end the track.
fn rewind(timestamp: moq_net::Timestamp, frames: usize, sample_rate: u32) -> Result<moq_net::Timestamp, Error> {
	if frames == 0 {
		return Ok(timestamp);
	}

	let offset = moq_net::Timestamp::from_scale(frames as u64, sample_rate as u64)?.convert(timestamp.scale())?;
	Ok(timestamp
		.checked_sub(offset)
		.unwrap_or(moq_net::Timestamp::new(0, timestamp.scale())?))
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::Format;
	use crate::encode::{Encoder, Input, Options, Producer};

	#[tokio::test]
	async fn remixes_mono_stream_to_stereo_output() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let subscriber = broadcast.consume();
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input.clone(), &options).unwrap();
		let catalog = Encoder::new(&crate::encode::Config::new(input)).unwrap().catalog();
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				channels: Some(2),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let samples = vec![0.1f32; 960];
		let mut data = Vec::with_capacity(samples.len() * size_of::<f32>());
		for sample in samples {
			data.extend_from_slice(&sample.to_le_bytes());
		}
		producer
			.write(&Frame {
				timestamp: Timestamp::ZERO,
				data: data.into(),
			})
			.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		let samples = Format::F32.as_interleaved_f32(&frame.data, 2).unwrap();
		assert_eq!(samples.len(), (960 - 312) * 2);
		for pair in samples.chunks_exact(2) {
			assert_eq!(pair[0], pair[1]);
		}
	}

	/// A packet whose sample count isn't a multiple of the resampler's chunk leaves
	/// samples buffered, and the next output starts with those. Stamping that
	/// output with the packet that completed the chunk puts it up to a chunk late,
	/// which is a sawtooth in A/V sync rather than a constant offset. Any codec
	/// whose frame is not a whole number of chunks reaches it: a 1024-sample frame
	/// at 44.1 kHz never fills the 882-frame chunk evenly.
	#[tokio::test]
	async fn resampled_timestamps_follow_the_samples() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();

		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 44_100, 1);
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);

		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				sample_rate: Some(48_000),
				max_age: std::time::Duration::from_secs(1),
				..Config::new()
			},
		)
		.await
		.unwrap();

		// Two 1024-sample packets, back to back at the codec's own rate.
		const FRAMES: u64 = 1024;
		let payload: Bytes = vec![0u8; FRAMES as usize * size_of::<f32>()].into();
		for packet in 0..2 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_scale(packet * FRAMES, 44_100).unwrap(),
					duration: None,
					payload: payload.clone(),
					keyframe: true,
				})
				.unwrap();
		}

		let first = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(first.timestamp.as_micros(), 0);

		// Continuity, not a fixed number: the second frame starts where the first
		// one's samples end, whatever they came to. Within a few frames rather than
		// exactly, because the resampler emits whole frames and its count per chunk
		// wobbles around the nominal ratio; a real hole (the samples it held back, or
		// the startup silence it dropped) is twenty times this tolerance.
		let second = consumer.read().await.unwrap().expect("decoded frame");
		let first_frames = (first.data.len() / size_of::<f32>()) as u128;
		let ends_at = first_frames * 1_000_000 / 48_000;
		let gap = second.timestamp.as_micros().abs_diff(ends_at);
		assert!(gap < 100, "expected the frames to meet, got a {gap} us gap");
	}

	/// The resampler only converts whole chunks, so the last partial one has to be
	/// flushed at end of track or its audio is simply gone. A 1024-sample frame at
	/// 44.1 kHz guarantees a remainder, never filling the 882-frame chunk evenly.
	#[tokio::test]
	async fn resampled_tail_survives_the_end_of_the_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();

		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 44_100, 1);
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);

		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				sample_rate: Some(48_000),
				..Config::new()
			},
		)
		.await
		.unwrap();

		// One 1024-frame packet: 882 fill a chunk, 142 are left holding.
		const FRAMES: usize = 1024;
		let payload: Bytes = vec![0u8; FRAMES * size_of::<f32>()].into();
		producer
			.write(moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::ZERO,
				duration: None,
				payload,
				keyframe: true,
			})
			.unwrap();
		producer.finish().unwrap();

		let first = consumer.read().await.unwrap().expect("decoded frame");
		let first_frames = first.data.len() / size_of::<f32>();

		let tail = consumer.read().await.unwrap().expect("flushed tail");
		let tail_frames = tail.data.len() / size_of::<f32>();

		// The 142 held-back frames at 44.1 kHz are ~155 at 48 kHz, plus the 69 the
		// sinc filter still owes: it runs centred, so the end of the track only
		// emerges once the flush has fed it silence to push it out.
		assert!((215..=230).contains(&tail_frames), "unexpected tail: {tail_frames}");
		// It picks up where the first frame's samples ended, within the same few
		// frames of whole-frame rounding as above.
		let ends_at = (first_frames as u128) * 1_000_000 / 48_000;
		let gap = tail.timestamp.as_micros().abs_diff(ends_at);
		assert!(gap < 100, "expected the tail to meet the body, got a {gap} us gap");

		// Together they cover the packet and no more: 1024 frames at 44.1 kHz is
		// ~1114 at 48 kHz. The filter's delay does not extend the stream, because
		// what the drain adds here is what the start dropped off the front.
		let total = first_frames + tail_frames;
		assert!((1105..=1120).contains(&total), "unexpected total: {total}");
		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn reads_the_container_the_catalog_declares() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let observed = track.clone();
		let subscriber = broadcast.consume();

		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 1);
		catalog.container = hang::catalog::Container::Loc;

		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Loc);
		let max_age = std::time::Duration::from_millis(250);
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				format: Format::F32,
				max_age,
				..Config::new()
			},
		)
		.await
		.unwrap();
		assert_eq!(observed.subscription().unwrap().max_age, max_age);

		let samples = [0.25f32, -0.5, 0.75, -1.0];
		let payload: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::ZERO,
				duration: None,
				payload: payload.into(),
				keyframe: true,
			})
			.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(
			Format::F32.as_interleaved_f32(&frame.data, 1).unwrap().as_ref(),
			samples
		);
	}

	/// The catalog picks the framing, not this crate. Hardcoding the legacy wire
	/// read a CMAF fragment as a varint timestamp plus a payload, which handed the
	/// codec garbage instead of failing, so anything published by `moq import
	/// fmp4` was undecodable.
	#[tokio::test]
	async fn decodes_a_cmaf_framed_track() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 2,
		};

		// One real Opus packet, so a mis-framed read can't accidentally decode.
		let mut encoder = Encoder::new(&crate::encode::Config::new(input.clone())).unwrap();
		let mut catalog = encoder.catalog();
		let pcm = vec![0.0f32; encoder.frame_size() * encoder.codec_channels() as usize];
		let packet = encoder.encode(&pcm).unwrap();

		// Re-describe the same rendition as CMAF and publish it that way.
		let muxer = moq_mux::container::fmp4::Muxer::audio(&catalog).unwrap();
		let init = muxer.init().unwrap().expect("an out-of-band codec has an init segment");
		catalog.container = hang::catalog::Container::Cmaf { init };

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(&catalog.container).unwrap();
		let mut producer = moq_mux::container::Producer::new(track, container);

		let mut consumer = Consumer::new(&subscriber, &catalog, "audio", Config::new())
			.await
			.unwrap();

		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::ZERO,
				payload: packet,
				keyframe: true,
				duration: None,
			})
			.unwrap();
		producer.cut(None).unwrap();

		// The whole packet decodes: one 20 ms Opus frame at 48 kHz, less the pre-skip
		// trimmed off the first packet. Reading the fragment as legacy hands the codec
		// a slice of the moof instead, which still decodes, just to a shorter buffer.
		let frame = consumer.read().await.unwrap().expect("decoded frame");
		// `as_micros`, not `==`: the CMAF path carries the fmp4 timescale and
		// `Timestamp`'s equality is structural, so the scales would have to match too.
		assert_eq!(frame.timestamp.as_micros(), 0);
		let samples = Format::F32.as_interleaved_f32(&frame.data, 2).unwrap();
		assert_eq!(samples.len(), (960 - 312) * 2);
	}
}

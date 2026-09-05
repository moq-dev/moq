//! Subscribe to an encoded audio track and emit raw PCM.

use std::collections::VecDeque;

use bytes::Bytes;

use super::decoder::{Config, Decoder};
use crate::resample::{Resampler, remix, validate_channels};
use crate::{Activity, Error, Frame};

/// Subscribe to a moq-mux audio track and emit decoded PCM in the layout
/// declared by [`Config`].
///
/// The mirror of [`encode::Producer`](crate::encode::Producer): output format /
/// sample rate / channel count are fixed at construction, and
/// [`read`](Self::read) returns [`Frame`]s carrying the codec activity they
/// were decoded from.
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	resampler: Option<Resampler>,
	config: Config,
	max_age: std::time::Duration,
	resolved_sample_rate: u32,
	resolved_channels: u32,
	/// One past the last sample handed to the resampler, so the tail it is still
	/// holding at end of track can be stamped. `None` until the first packet.
	tail: Option<moq_net::Timestamp>,
	/// Where the next packet's timestamp should land: the last packet's timestamp
	/// plus the media it covered, including the codec delay the decoder trimmed off
	/// the front. A packet that misses it is a hole nobody declared.
	next_start: Option<moq_net::Timestamp>,
	/// Frames decoded and not yet handed back, so a gap's tail can be returned
	/// ahead of the packet that exposed it.
	ready: VecDeque<Frame>,
	/// Codec activity spans the resampler's buffered output still covers.
	spans: VecDeque<ActivitySpan>,
	/// Activity of the last span the output ran past, for the rounding samples the
	/// filter leaves beyond the final input boundary.
	trailing: Activity,
	/// Timestamp of the first encoded packet in this decoder epoch, used to
	/// interpret codec delay and a terminal marker.
	epoch: Option<moq_net::Timestamp>,
	/// Codec delay trimmed since the current decoder epoch began.
	delay_trimmed: usize,
	/// Codec-rate terminal frames emitted since `terminal_start`.
	frames_decoded: usize,
	/// Logical endpoint carried by an empty legacy frame before terminal packets.
	end: Option<moq_net::Timestamp>,
	/// Presentation time of the first decoded terminal frame.
	terminal_start: Option<moq_net::Timestamp>,
	/// Last container discontinuity applied to codec and resampler state.
	discontinuity: u64,
}

struct ActivitySpan {
	end: moq_net::Timestamp,
	activity: Activity,
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
		let max_age = config.max_age.min(track.info().max_age);
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
			max_age,
			resolved_sample_rate: sample_rate,
			resolved_channels: channels,
			tail: None,
			next_start: None,
			ready: VecDeque::new(),
			spans: VecDeque::new(),
			trailing: Activity::Active,
			epoch: None,
			delay_trimmed: 0,
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

	/// The effective age budget after clamping to the publisher's retention window.
	pub fn max_age(&self) -> std::time::Duration {
		self.max_age
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
	///
	/// [`Frame::activity`] reports whether the packet these samples came from
	/// coded audio. It describes where the frame begins, so a resampled
	/// frame that straddles a change carries the activity its first sample came
	/// from and the next frame carries the new one.
	///
	/// A timestamp that doesn't continue the previous packet is a hole in the
	/// output, not a splice: nothing is carried across it, and the frames on either
	/// side stay anchored to their own packet timeline, so the hole is there to
	/// see. "Doesn't continue" allows for the quantization the stamps carry, which
	/// on a millisecond-stamped ingest is most of a millisecond.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			if let Some(frame) = self.ready.pop_front() {
				return Ok(Some(frame));
			}

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

			// Undeclared holes are routine: a skipped stalled group, a packet the
			// decoder refused, an ingest that resynced. Drop every stage's state at
			// the edge, before the packet after it goes anywhere near the decoder.
			//
			// Skipped once an end marker arrives, because from there the terminal
			// phase reconstructs each batch's time from the marker rather than
			// reading it off the packet, so there is nothing left to compare.
			if self.end.is_none()
				&& self
					.next_start
					.is_some_and(|next| discontinuous(next, mux_frame.timestamp))
				&& let Some(frame) = self.gap()?
			{
				self.ready.push_back(frame);
			}

			let rate = self.decoder.sample_rate();
			let epoch = *self.epoch.get_or_insert(mux_frame.timestamp);
			let delay = self.decoder.delay_remaining();
			let decoded = self.decoder.decode(&mux_frame.payload)?;
			// Codec delay trimmed off the front is media this packet covered even
			// though no samples came out, so it still moves the packet after it along.
			let trimmed = delay - self.decoder.delay_remaining();
			self.delay_trimmed += trimmed;
			let activity = decoded.activity;
			let mut decoded = decoded.samples;
			if let Some(end) = self.end {
				let terminal_start = *self
					.terminal_start
					.get_or_insert(rewind(mux_frame.timestamp, self.delay_trimmed, rate)?.max(epoch));
				let total = frames_between(terminal_start, end, rate)?;
				let remaining = total.saturating_sub(self.frames_decoded);
				decoded.truncate(remaining.saturating_mul(self.decoder.channel_count() as usize));
			}

			let frames = decoded.len() / self.decoder.channel_count().max(1) as usize;
			let decoded_at = if let Some(terminal_start) = self.terminal_start {
				advance(terminal_start, self.frames_decoded, rate)?
			} else {
				// The codec delay is padding before the epoch, not a hole after the
				// first short frame. Keep later output contiguous by moving it back over
				// everything trimmed since this decoder epoch began.
				rewind(mux_frame.timestamp, self.delay_trimmed, rate)?.max(epoch)
			};
			if self.end.is_some() {
				self.frames_decoded += frames;
			}
			// Packet continuity stays on the encoded timeline. `decoded_at` may be
			// earlier because codec pre-skip is padding before the decoded epoch.
			self.next_start = Some(advance(mux_frame.timestamp, frames + trimmed, rate)?);
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

			let decoded_end = advance(decoded_at, frames, rate)?;
			self.tail = Some(decoded_end);

			// The resampler hands back samples it was holding from earlier packets,
			// so what comes out starts before the packet that filled its chunk. Track
			// where each packet's activity ends so the output can be labelled by
			// where it actually begins, not by the packet just submitted.
			let resampled = self.resampler.is_some();
			if resampled {
				self.spans.push_back(ActivitySpan {
					end: decoded_end,
					activity,
				});
			}

			// A packet shorter than the resampler's chunk leaves nothing to hand
			// over yet. Read on rather than returning a frame with no samples, which
			// a caller would otherwise see as audio arriving.
			if pcm.is_empty() {
				continue;
			}

			let activity = if resampled {
				self.activity_at(timestamp)
			} else {
				activity
			};
			// Queued rather than returned, so a tail drained at a gap earlier in this
			// same iteration still comes out first. The next turn of the loop pops it.
			let frame = self.frame(pcm, timestamp, activity)?;
			self.ready.push_back(frame);
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
		self.next_start = None;
		self.spans.clear();
		self.trailing = Activity::Active;
		self.epoch = None;
		self.delay_trimmed = 0;
		self.frames_decoded = 0;
		self.end = None;
		self.terminal_start = None;
		Ok(())
	}

	/// Reset codec prediction and resampling state at a hole, returning whatever the
	/// resampler was still holding from before it.
	///
	/// Those samples arrived before the hole and belong before it, so they come
	/// out as their own frame rather than being filtered together with the audio
	/// on the far side. The resampler starts over from there, which is what makes
	/// the next packet's output stamp from the packet itself: nothing is buffered
	/// to reach back over.
	fn gap(&mut self) -> Result<Option<Frame>, Error> {
		self.decoder.reset_prediction()?;

		let drained = match (self.resampler.as_mut(), self.tail) {
			(Some(resampler), Some(tail)) => {
				let pending = resampler.pending_frames();
				let skipped = resampler.skipped();
				let pcm = resampler.drain()?;
				(!pcm.is_empty()).then_some((pcm, tail, pending, skipped))
			}
			_ => None,
		};

		let frame = match drained {
			Some((pcm, tail, pending, skipped)) => {
				let timestamp = self.starts_at(tail, pending, skipped, self.decoder.sample_rate())?;
				let activity = self.activity_at(timestamp);
				Some(self.frame(pcm, timestamp, activity)?)
			}
			None => None,
		};

		self.tail = None;
		self.next_start = None;
		self.spans.clear();
		self.trailing = Activity::Active;
		self.epoch = None;
		self.delay_trimmed = 0;
		Ok(frame)
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
		let activity = self.activity_at(timestamp);
		Ok(Some(self.frame(pcm, timestamp, activity)?))
	}

	/// The codec activity covering `timestamp`, dropping the spans it has passed.
	fn activity_at(&mut self, timestamp: moq_net::Timestamp) -> Activity {
		while let Some(span) = self.spans.front().filter(|span| span.end <= timestamp) {
			self.trailing = span.activity;
			self.spans.pop_front();
		}

		self.spans.front().map_or(self.trailing, |span| span.activity)
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
	fn frame(&self, pcm: Vec<f32>, timestamp: moq_net::Timestamp, activity: Activity) -> Result<Frame, Error> {
		let pcm = if self.decoder.channel_count() == self.resolved_channels {
			pcm
		} else {
			remix(&pcm, self.decoder.channel_count(), self.resolved_channels)?
		};

		let bytes = self.config.format.from_interleaved_f32(&pcm, self.resolved_channels)?;
		Ok(Frame {
			timestamp,
			data: Bytes::from(bytes),
			activity,
		})
	}
}

/// Whether `timestamp` fails to continue `expected`, leaving a hole (or an
/// overlap) rather than the next packet in line.
///
/// Exact contiguity cannot be the test. RTMP stamps in whole milliseconds while a
/// 1024-sample AAC frame at 44.1 kHz runs 23.22 ms, so on the most common ingest
/// path every packet lands beside where its predecessor ended.
///
/// The slack is the quantization the stamps carry, and nothing else. A frame
/// duration would be far too much: a single lost packet lands exactly one frame
/// off, and Opus packets run anywhere from 2.5 ms to 60 ms with no duration
/// declared in the catalog, so a half-frame rule read off a 20 ms neighbour would
/// splice straight across a lost 2.5 ms one.
///
/// So a packet is discontinuous when it misses `expected` by more than one unit of
/// the coarsest timescale on the path, plus one unit of the stamp's own scale for
/// the rounding in the arithmetic that produced `expected`. The coarsest timescale
/// is the stamp's own scale floored at [`Timescale::default`](moq_net::Timescale):
/// the legacy hang container re-stamps every frame in microseconds whatever the
/// source used, and a wire that cannot carry a timescale at all (moq-lite before
/// 05, IETF moq-transport) falls back to milliseconds, so a millisecond is the
/// finest quantization a packet can be assumed to have kept. That floor stays under
/// the shortest packet anything here can send, 2.5 ms of Opus, so it never
/// swallows a lost one.
fn discontinuous(expected: moq_net::Timestamp, timestamp: moq_net::Timestamp) -> bool {
	let scale = expected.scale().max(timestamp.scale());
	let quantum = scale.min(moq_net::Timescale::default());
	let tolerance = (scale.as_u64() as u128).div_ceil(quantum.as_u64() as u128) + 1;
	expected.as_scale(scale).abs_diff(timestamp.as_scale(scale)) > tolerance
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
		producer.write(&Frame::new(data.into(), Timestamp::ZERO)).unwrap();

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
	async fn resampling_keeps_the_activity_boundary_on_its_source() {
		let mut encoder = Encoder::new(&crate::encode::Config {
			dtx: true,
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(24_000)),
			frame_duration: std::time::Duration::from_millis(10),
			..crate::encode::Config::new(Input {
				channels: 1,
				..Input::default()
			})
		})
		.unwrap();
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 1);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				sample_rate: Some(44_100),
				max_age: std::time::Duration::from_secs(1),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let active = vec![0.5; encoder.frame_size()];
		let silence = vec![0.0; encoder.frame_size()];
		let mut first_dtx = None;
		for index in 0..40u64 {
			let packet = encoder.encode(if index == 0 { &active } else { &silence }).unwrap();
			let timestamp = Timestamp::from_scale(index * encoder.frame_size() as u64, 48_000).unwrap();
			if first_dtx.is_none() && packet.activity.is_dtx() {
				first_dtx = Some(timestamp);
			}
			producer
				.write(moq_mux::container::Frame {
					timestamp,
					payload: packet.payload,
					keyframe: true,
					duration: None,
				})
				.unwrap();
			producer.cut(None).unwrap();
		}
		producer.finish().unwrap();

		let expected = first_dtx.expect("silence should enter Opus DTX");
		let mut actual = None;
		while let Some(frame) = consumer.read().await.unwrap() {
			// 10 ms packets do not fill the 20 ms chunk, so the resampler hands back
			// nothing every other packet. Those must not surface as frames: a frame
			// with no samples reads as audio arriving, and carries an activity
			// describing samples that are not there.
			assert!(!frame.data.is_empty(), "read returned a frame with no samples");
			if frame.activity.is_dtx() {
				actual = Some(frame.timestamp);
				break;
			}
		}
		let actual = actual.expect("consumer should report Opus DTX");

		// Each frame carries the activity its first sample came from, so the label
		// can lag its source by up to the frame it lands in, but it must never lead
		// it: leading means samples that are still active got labelled DTX. That is
		// what labelling by the packet most recently submitted does, since the
		// resampler is handing back audio from before that packet. It puts the
		// boundary a chunk early instead of a fraction of a chunk late.
		let delay = actual.as_micros() as i128 - expected.as_micros() as i128;
		let chunk_us = 20_000i128;
		assert!(
			(0..chunk_us).contains(&delay),
			"DTX label landed {delay} us from its source, outside [0, {chunk_us})"
		);
	}

	/// Publish PCM packets of `frames` samples each at the given stamps, and read
	/// back every decoded frame as `(microseconds, output frames)`.
	async fn pcm_gaps(rate: u32, out_rate: u32, frames: usize, stamps: &[Timestamp]) -> Vec<(u128, usize)> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();

		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, rate, 1);
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				sample_rate: Some(out_rate),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let payload: Bytes = vec![0u8; frames * size_of::<f32>()].into();
		for stamp in stamps {
			producer
				.write(moq_mux::container::Frame {
					timestamp: *stamp,
					duration: None,
					payload: payload.clone(),
					keyframe: true,
				})
				.unwrap();
		}
		producer.finish().unwrap();

		let mut read = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			read.push((frame.timestamp.as_micros(), frame.data.len() / size_of::<f32>()));
		}
		read
	}

	/// A packet that doesn't continue the last one is a hole, not a splice: the
	/// resampler hands back what it was holding from before the gap as its own
	/// frame, and the audio after it is stamped from the packet that carried it
	/// rather than rewound over samples that no longer exist.
	#[tokio::test]
	async fn a_missing_packet_leaves_a_hole() {
		const FRAMES: usize = 1024;
		// Packets at sample 0 and sample 2048: the one at 1024 never arrived.
		let stamps = [
			Timestamp::from_scale(0, 44_100).unwrap(),
			Timestamp::from_scale(2 * FRAMES as u64, 44_100).unwrap(),
		];
		let read = pcm_gaps(44_100, 48_000, FRAMES, &stamps).await;

		// The first packet's chunk, then the tail drained at the gap, then the
		// second packet's chunk. The flush at end of track adds the last tail.
		assert_eq!(read.len(), 4, "unexpected frames: {read:?}");

		// Everything the first packet carried comes out before the hole: 1024 frames
		// at 44.1 kHz is ~1114 at 48 kHz, whole-frame rounding aside.
		let before: usize = read[..2].iter().map(|(_, frames)| frames).sum();
		assert!((1105..=1120).contains(&before), "unexpected pre-gap audio: {before}");

		// The audio after the hole is stamped by its own packet. Rewinding over the
		// resampler's buffer instead would put it ~3 ms early, in the middle of the
		// hole, and splice the two sides together through the filter.
		assert_eq!(read[2].0, stamps[1].as_micros());

		// And the hole is the packet that never arrived: 1024 frames at 44.1 kHz.
		let ends_at = read[1].0 + (read[1].1 as u128) * 1_000_000 / 48_000;
		let hole = read[2].0 - ends_at;
		assert!((23_100..=23_350).contains(&hole), "unexpected hole: {hole} us");
	}

	/// Every packet on the RTMP path lands beside where the last one ended: FLV
	/// stamps in whole milliseconds and a 1024-sample AAC frame at 44.1 kHz runs
	/// 23.22 ms, so the stamps drift up to a millisecond either way. Reading that as
	/// a hole would reset the codec and the resampler on nearly every packet.
	///
	/// PCM stands in for AAC, which needs an encoder this crate doesn't have: the
	/// arithmetic that matters is the packet length and the millisecond stamps.
	#[tokio::test]
	async fn millisecond_stamps_are_not_a_gap() {
		const FRAMES: u64 = 1024;
		const PACKETS: u64 = 32;

		// What an FLV ingest sends: each packet stamped in whole milliseconds.
		let stamps: Vec<_> = (0..PACKETS)
			.map(|packet| Timestamp::from_millis(packet * FRAMES * 1_000 / 44_100).unwrap())
			.collect();
		let read = pcm_gaps(44_100, 48_000, FRAMES as usize, &stamps).await;

		// One frame per packet, since 1024 frames always fill at least one 882-frame
		// chunk, plus the flush at the end of the track. Reading a gap would drain
		// the resampler as well, adding a frame at every packet it fired on.
		assert_eq!(read.len(), stamps.len() + 1, "unexpected frames: {read:?}");

		// And the output stays continuous across all of them, within the millisecond
		// the stamps themselves are quantized to.
		for pair in read.windows(2) {
			let ends_at = pair[0].0 + (pair[0].1 as u128) * 1_000_000 / 48_000;
			assert!(
				pair[1].0.abs_diff(ends_at) <= 1_100,
				"frames at {} and {} do not meet",
				pair[0].0,
				pair[1].0
			);
		}
	}

	/// The tolerance can't come from a frame duration. Opus packets run from 2.5 ms
	/// to 60 ms with nothing in the catalog to say which, so a rule read off the
	/// 20 ms packet before it would splice straight across a lost 2.5 ms one.
	#[tokio::test]
	async fn a_lost_opus_packet_shorter_than_its_neighbour_is_a_gap() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let mut encoder = Encoder::new(&crate::encode::Config::new(input)).unwrap();
		let catalog = encoder.catalog();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		let mut consumer = Consumer::new(&subscriber, &catalog, "audio", Config::new())
			.await
			.unwrap();

		// A 20 ms packet at 0, then the next one at 22.5 ms: the 2.5 ms packet
		// between them was lost.
		let pcm = vec![0.25f32; encoder.frame_size()];
		for timestamp in [
			Timestamp::from_micros(0).unwrap(),
			Timestamp::from_micros(22_500).unwrap(),
			Timestamp::from_micros(42_500).unwrap(),
		] {
			producer
				.write(moq_mux::container::Frame {
					timestamp,
					duration: None,
					payload: encoder.encode(&pcm).unwrap().payload,
					keyframe: true,
				})
				.unwrap();
			producer.cut(None).unwrap();
		}

		// The pre-skip is trimmed off the first packet, so it decodes short. That
		// shortfall is codec delay, not a hole: without counting it the packet after
		// every stream start would read as a gap.
		let first = consumer.read().await.unwrap().expect("decoded frame");
		let frames = first.data.len() / size_of::<f32>();
		assert!(frames < 960, "the pre-skip should be trimmed, got {frames} frames");

		// The hole is real, so codec prediction starts over but stream-level pre-skip
		// does not. The audio is stamped where the packet says rather than 2.5 ms early.
		let second = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(second.timestamp.as_micros(), 22_500);
		assert_eq!(second.data.len() / size_of::<f32>(), 960, "pre-skip was reapplied");

		let third = consumer.read().await.unwrap().expect("decoded frame after gap");
		let second_frames = second.data.len() / size_of::<f32>();
		assert_eq!(
			third.timestamp,
			advance(second.timestamp, second_frames, 48_000).unwrap()
		);
	}

	#[tokio::test]
	async fn max_age_is_clamped_to_publisher_retention() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let info = hang::container::track_info(hang::catalog::PRIORITY.audio)
			.with_max_age(std::time::Duration::from_millis(100));
		let _track = broadcast.create_track("audio", info).unwrap();
		let subscriber = broadcast.consume();
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 1);

		let consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				max_age: std::time::Duration::from_millis(500),
				..Config::new()
			},
		)
		.await
		.unwrap();

		assert_eq!(consumer.max_age(), std::time::Duration::from_millis(100));
	}

	/// Opus pre-skip is padding before the decoded epoch, not missing media after
	/// the first short frame. The second frame must meet the first or playback
	/// fills the codec delay with silence and creates a startup glitch.
	#[tokio::test]
	async fn opus_pre_skip_does_not_leave_a_timestamp_hole() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let mut encoder = Encoder::new(&crate::encode::Config::new(input)).unwrap();
		let catalog = encoder.catalog();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		let mut consumer = Consumer::new(&subscriber, &catalog, "audio", Config::new())
			.await
			.unwrap();

		let pcm = vec![0.25f32; encoder.frame_size()];
		for packet in 0..2 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_scale(packet * encoder.frame_size() as u64, 48_000).unwrap(),
					duration: None,
					payload: encoder.encode(&pcm).unwrap().payload,
					keyframe: true,
				})
				.unwrap();
			producer.cut(None).unwrap();
		}

		let first = consumer.read().await.unwrap().expect("first decoded frame");
		let second = consumer.read().await.unwrap().expect("second decoded frame");
		let first_frames = first.data.len() / size_of::<f32>();
		let expected = advance(first.timestamp, first_frames, 48_000).unwrap();
		assert_eq!(second.timestamp, expected);
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
				payload: packet.payload,
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

//! Encode raw PCM and publish it as a moq audio track.

use bytes::Bytes;

use moq_mux::catalog::hang::CatalogExt;
use moq_mux::container::Frame as MuxFrame;
use moq_net::Timestamp;

use super::encoded::Encoded;
use super::encoder::{Encoder, Input, Settings};
use crate::resample::{Resampler, remix, validate_remix};
use crate::{Activity, Error, Frame};

/// Encode and publication policy for [`Producer`].
///
/// `#[non_exhaustive]`: construct via [`Options::default`] and set fields, so
/// new knobs can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
	/// Track name to publish under. `None` derives a unique one from the codec
	/// (`0.opus`, then `1.opus`, ...), matching how the video side names its
	/// track. Subscribers find it through the catalog either way.
	pub track: Option<String>,
	/// Codec input and encoded output settings.
	pub settings: Settings,
	/// The connection's bandwidth, as an allocator over
	/// [`Session::send_bandwidth`](moq_net::Session::send_bandwidth).
	///
	/// The audio track reserves its bitrate against it, so the video encoder sharing
	/// the connection sizes itself against what's actually left rather than against
	/// the whole uplink. Pass the same allocator to both.
	///
	/// Defaults to [`Allocator::unlimited`](moq_net::bandwidth::Allocator::unlimited),
	/// which reserves nothing and leaves every sender at its configured rate.
	///
	/// Audio reserves but does not follow its share: Opus can retune live and PCM
	/// can't at all, and at `hang`'s priorities audio outranks video, so it is only
	/// ever squeezed on a link that can't carry audio alone.
	pub bandwidth: moq_net::bandwidth::Allocator,
}

impl Default for Options {
	fn default() -> Self {
		Self {
			track: None,
			settings: Settings::default(),
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
		}
	}
}

/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// The input PCM layout is fixed at construction via [`Input`]; the codec
/// settings via [`Options`]. Subsequent [`write`](Self::write) calls just pass a
/// [`Frame`]: payload bytes and a timestamp.
///
/// The catalog rendition is registered at construction (not on first write), so
/// a subscriber that opens the catalog before any frames arrive still sees the
/// track.
pub struct Producer<E: CatalogExt = ()> {
	encoder: Encoder,
	input: Input,
	resampler: Option<Resampler>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
	_ext: std::marker::PhantomData<fn() -> E>,
	pending: Vec<f32>,
	/// Samples emitted since the current epoch (reset by [`reset_epoch`](Self::reset_epoch)).
	frames_produced: u64,
	/// Wall-clock anchor in microseconds, taken from the first frame after each
	/// (re)start. Emitted PTS = `epoch + frames_produced / codec_rate`. `None`
	/// until the first write so the next frame re-anchors to its timestamp.
	epoch_us: Option<u64>,
	/// An encoder reset that still needs a marker group before its next packet.
	pending_discontinuity: bool,
	/// Whether a marker group already separates the next packet from prior codec state.
	decoder_boundary: bool,
	/// How the encoder classified the packet it published most recently.
	activity: Activity,
	/// Set after a successful [`finish`](Self::finish). Writes then fail with
	/// [`moq_net::Error::Closed`]; [`abort`](Self::abort) can still run.
	finished: bool,
}

struct Terminal {
	packets: Vec<Encoded>,
	end: Timestamp,
	start: Timestamp,
	frame_size: usize,
	codec_rate: u32,
}

/// A published track whose PCM layout is not known yet.
///
/// The track exists in the broadcast immediately, so its name and subscriber
/// state are available, while the catalog rendition waits for
/// [`encode`](Self::encode) to supply the layout it describes. Unlike a catalog
/// [`Reserved`](moq_mux::catalog::Reserved) this does not withhold the catalog:
/// subscribers see the broadcast without this rendition until it resolves.
pub(crate) struct Reserved<E: CatalogExt = ()> {
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
	_ext: std::marker::PhantomData<fn() -> E>,
}

impl<E: CatalogExt> Reserved<E> {
	pub(crate) fn new(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: &Options,
	) -> Result<Self, Error> {
		let track = match &options.track {
			// The catalog's info carries the microsecond timescale audio hang frames stamp, so
			// Lite05 subscribers know what scale to expect and the model layer accepts
			// Frame::timestamp on append, plus whatever retention the broadcast declared.
			Some(name) => broadcast.create_track(name.clone(), catalog.track_info(hang::catalog::PRIORITY.audio))?,
			// Mirrors the video side, which derives a unique name from the codec
			// rather than making every caller invent one.
			None => broadcast.unique_track(
				&format!(".{}", options.settings.codec),
				catalog.track_info(hang::catalog::PRIORITY.audio),
			)?,
		};
		let track = catalog.audio(
			track,
			moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio),
			None,
		)?;

		Ok(Self {
			track,
			_ext: std::marker::PhantomData,
		})
	}

	/// Build the encoder for `input` and register the rendition describing it.
	///
	/// Separate from [`encode`](Self::encode), which cannot fail, so a layout the
	/// codec rejects leaves the reservation intact for another input.
	pub(crate) fn register(&mut self, input: Input, options: &Options) -> Result<Registered, Error> {
		validate_remix(input.layout, options.settings.layout)?;
		let encoder = Encoder::new(&options.settings)?;

		let resampler = if input.sample_rate == encoder.codec_rate() {
			None
		} else {
			// Use microsecond precision so 2.5 ms frame_duration (supported by
			// libopus) doesn't truncate to 2 ms.
			let chunk_frames =
				((input.sample_rate as u128 * encoder.settings().frame_duration.as_micros()) / 1_000_000) as usize;
			Some(Resampler::new(
				input.sample_rate,
				encoder.codec_rate(),
				encoder.codec_channels(),
				chunk_frames,
			)?)
		};

		self.track.set(encoder.catalog())?;

		Ok(Registered {
			encoder,
			input,
			resampler,
		})
	}

	/// Spend the reservation on a registered encoder, publishing through the track.
	pub(crate) fn encode(self, registered: Registered) -> Producer<E> {
		Producer {
			encoder: registered.encoder,
			input: registered.input,
			resampler: registered.resampler,
			track: self.track,
			_ext: self._ext,
			pending: Vec::new(),
			frames_produced: 0,
			epoch_us: None,
			pending_discontinuity: false,
			decoder_boundary: true,
			activity: Activity::Active,
			finished: false,
		}
	}
}

/// A registered rendition, waiting for its [`Reserved`] to be spent on it.
///
/// Proof that [`Reserved::register`] succeeded, so [`Reserved::encode`] takes no
/// fallible step and cannot strand the track it consumes.
pub(crate) struct Registered {
	encoder: Encoder,
	input: Input,
	resampler: Option<Resampler>,
}

/// What a capture publication needs while its layout is still undiscovered.
#[cfg(feature = "capture")]
impl<E: CatalogExt> Reserved<E> {
	/// The resolved track name, available before the layout is.
	pub(crate) fn name(&self) -> &str {
		self.track.name()
	}

	/// The underlying track producer, e.g. to watch subscriber state.
	pub(crate) fn track(&self) -> &moq_net::track::Producer {
		self.track.track()
	}

	/// Finalize a track that never got a rendition.
	pub(crate) fn finish(&mut self) -> Result<(), Error> {
		self.track.finish()?;
		Ok(())
	}

	/// Abort a track that never got a rendition, so subscribers see `err`.
	pub(crate) fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

impl<E: CatalogExt> Producer<E> {
	/// Publish a track encoding `input` into `broadcast`, registering its
	/// rendition in `catalog` immediately.
	pub fn new(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		input: Input,
		options: &Options,
	) -> Result<Self, Error> {
		let mut reserved = Reserved::new(broadcast, catalog, options)?;
		let registered = reserved.register(input, options)?;
		Ok(reserved.encode(registered))
	}

	/// The name of the published track, which is [`Options::track`] resolved.
	pub fn track_name(&self) -> &str {
		self.track.name()
	}

	/// A watch-only handle to the track's subscriber demand, created eagerly so
	/// subscription state is observable before any frames arrive. Watch it via
	/// [`used`](moq_net::track::Demand::used) / [`unused`](moq_net::track::Demand::unused).
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	#[cfg(feature = "capture")]
	pub(crate) fn track(&self) -> &moq_net::track::Producer {
		self.track.track()
	}

	/// Whether the packet published most recently coded audio, or withheld it
	/// because the input was silent.
	///
	/// A local "am I talking" indicator without running a second voice detector
	/// over the microphone, though a silent run is punctuated by coded frames
	/// that read [`Activity::Active`], so hold the indicator across those rather
	/// than following it packet by packet. [`Activity::Active`] until the first
	/// packet, and for codecs without a discontinuous mode.
	pub fn activity(&self) -> Activity {
		self.activity
	}

	/// Current encoder target bitrate.
	pub fn bitrate(&self) -> moq_net::bandwidth::Rate {
		self.encoder.bitrate()
	}

	/// Retune the live encoder to `bitrate`.
	pub fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
		self.encoder.set_bitrate(bitrate)
	}

	/// Re-anchor the timeline to the next frame's timestamp, dropping any
	/// buffered samples. Call this when resuming after an idle gap (e.g. a
	/// released-then-reopened microphone) so the gap appears in the PTS and
	/// audio stays aligned with a wall-clock video track, rather than the gap
	/// being compressed out by the running sample count. Mirrors moq-boy's
	/// `reset_epoch`. If the codec had started, a marker group is published before
	/// the next packet so subscribers jump the playhead.
	pub fn reset_epoch(&mut self) {
		if self.encoder.started() && !self.decoder_boundary {
			self.pending_discontinuity = true;
		}
		self.reset_state();
	}

	fn reset_state(&mut self) {
		self.epoch_us = None;
		self.activity = Activity::Active;
		self.frames_produced = 0;
		self.pending.clear();
		self.encoder.reset();
		// The resampler holds samples of its own, plus filter state primed by them.
		// Left alone, `finish` would flush that pre-reset audio onto the track
		// (stamped at an epoch that no longer exists), and the next write would run
		// the new audio through a filter still ringing with the old.
		if let Some(resampler) = self.resampler.as_mut() {
			resampler.reset();
		}
	}

	/// Push one [`Frame`] of PCM in the layout declared by [`Input`]. Encodes and
	/// publishes as many packets as the input contains; any partial trailing
	/// frame is carried to the next call.
	///
	/// The first frame after construction (or [`reset_epoch`](Self::reset_epoch))
	/// anchors the timeline: its timestamp becomes the epoch, and emitted PTS
	/// then advances purely by the running sample count, so subsequent frames'
	/// timestamps are ignored. An idle gap is only reflected in the PTS if you
	/// call [`reset_epoch`](Self::reset_epoch) on resume (which re-anchors from
	/// the next frame's wall-clock stamp); writing straight across a gap without
	/// resetting compresses it out.
	///
	/// [`Frame::activity`] is ignored: the encoder classifies what it actually
	/// produced, which [`activity`](Self::activity) reports.
	///
	/// Writes after a successful [`finish`](Self::finish) fail with
	/// [`moq_net::Error::Closed`].
	pub fn write(&mut self, frame: &Frame) -> Result<(), Error> {
		if self.finished {
			return Err(moq_net::Error::Closed.into());
		}
		if self.pending_discontinuity {
			self.track.discontinuity()?;
			self.pending_discontinuity = false;
			self.decoder_boundary = true;
		}

		let timestamp_us = u64::try_from(frame.timestamp.as_micros())
			.map_err(|_| Error::Unsupported(format!("frame timestamp {:?} out of range", frame.timestamp)))?;
		let epoch_us = *self.epoch_us.get_or_insert(timestamp_us);

		let input = &self.input;
		let (format, channels) = (input.format, input.layout.channels());
		let pcm = format.as_interleaved_f32(frame.data.as_ref(), channels)?;
		let pcm = remix(&pcm, input.layout, self.encoder.settings().layout)?;
		let pcm: Vec<f32> = match self.resampler.as_mut() {
			Some(r) => r.process(&pcm, frame.timestamp)?,
			None => pcm,
		};

		self.pending.extend(pcm);

		self.publish_full_frames(epoch_us)
	}

	/// Encode and publish every full frame in `pending`, keeping any partial
	/// trailing frame for the next call.
	fn publish_full_frames(&mut self, epoch_us: u64) -> Result<(), Error> {
		let frame_samples = self.encoder.frame_size() * self.encoder.codec_channels() as usize;
		while self.pending.len() >= frame_samples {
			let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
			let packet = self.encoder.encode(&chunk)?;

			let timestamp = Self::timestamp(epoch_us, self.frames_produced, self.encoder.codec_rate())?;
			self.frames_produced += self.encoder.frame_size() as u64;
			self.activity = packet.activity;
			Self::publish(&mut self.track, packet, timestamp)?;
			self.decoder_boundary = false;
		}

		Ok(())
	}

	/// PTS of the next frame: the epoch plus the samples emitted since it.
	fn timestamp(epoch_us: u64, frames_produced: u64, codec_rate: u32) -> Result<Timestamp, Error> {
		let offset_us = (frames_produced * 1_000_000) / codec_rate as u64;
		Ok(Timestamp::from_micros(epoch_us + offset_us)?)
	}

	fn publish(
		track: &mut moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
		encoded: Encoded,
		timestamp: Timestamp,
	) -> Result<(), Error> {
		// Publish each audio packet as its own moq-lite group: write it as a keyframe, then cut
		// (below) so the relay forwards it without waiting for the next. Codecs can recover
		// independently after a dropped group.
		let mux_frame = MuxFrame {
			timestamp,
			payload: encoded.payload,
			keyframe: true,
			duration: None,
		};
		track.write(mux_frame)?;
		// No boundary to give: the next packet bounds this one, and Opus frames have a
		// deterministic duration anyway.
		track.cut(None)?;
		Ok(())
	}

	/// Publish terminal packets after an empty frame that carries their logical endpoint.
	fn publish_terminal(
		track: &mut moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
		terminal: Terminal,
	) -> Result<(), Error> {
		track.write(MuxFrame {
			timestamp: terminal.end,
			payload: Bytes::new(),
			keyframe: true,
			duration: None,
		})?;

		for (index, packet) in terminal.packets.into_iter().enumerate() {
			let offset = Timestamp::from_scale((index * terminal.frame_size) as u64, terminal.codec_rate as u64)?
				.convert(terminal.start.scale())?;
			track.write(MuxFrame {
				timestamp: terminal.start.checked_add(offset)?,
				payload: packet.payload,
				keyframe: false,
				duration: None,
			})?;
		}

		track.cut(Some(terminal.end))?;
		Ok(())
	}

	/// Mark a break in the published timeline and reset codec state.
	///
	/// Call this when capture stops rather than merely gapping between packets: going idle,
	/// switching source, or anything else that resumes on a re-anchored epoch. Buffered samples
	/// are dropped, and the next frame anchors a fresh codec epoch. See
	/// [`Producer::discontinuity`](moq_mux::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) -> Result<(), Error> {
		self.track.discontinuity()?;
		self.pending_discontinuity = false;
		self.decoder_boundary = true;
		self.reset_state();
		Ok(())
	}

	/// Flush pending samples, resampler output, and codec lookahead, then finalize
	/// the track.
	///
	/// Borrows rather than consumes, so a later [`abort`](Self::abort) can still
	/// run after a successful finish. Writes after this fail with
	/// [`moq_net::Error::Closed`].
	pub fn finish(&mut self) -> Result<(), Error> {
		if self.finished {
			return Ok(());
		}
		// Whatever the resampler still holds belongs to this track: its last partial
		// chunk, plus the audio its filter is running behind on. Dropping it here
		// would publish a track that ends before its source did.
		if let Some(resampler) = self.resampler.take() {
			self.pending.extend(resampler.flush()?);
		}

		// The drained resampler tail can span multiple frames. Publish those first
		// so only the final partial frame reaches the encoder's terminal drain.
		let epoch_us = self.epoch_us.unwrap_or(0);
		self.publish_full_frames(epoch_us)?;

		let frame_size = self.encoder.frame_size();
		let codec_rate = self.encoder.codec_rate();
		let channels = self.encoder.codec_channels() as usize;
		let source_frames = self.pending.len() / channels;
		let start = Self::timestamp(epoch_us, self.frames_produced, codec_rate)?;
		let end = Self::timestamp(epoch_us, self.frames_produced + source_frames as u64, codec_rate)?;
		let finish = self.encoder.drain(&self.pending)?;
		let discard_padding = finish.discard_padding();
		let packets = finish.into_packets();

		if discard_padding > 0 {
			Self::publish_terminal(
				&mut self.track,
				Terminal {
					packets,
					end,
					start,
					frame_size,
					codec_rate,
				},
			)?;
		} else {
			for packet in packets {
				let timestamp = Self::timestamp(epoch_us, self.frames_produced, codec_rate)?;
				self.activity = packet.activity;
				Self::publish(&mut self.track, packet, timestamp)?;
				self.frames_produced += frame_size as u64;
			}
		}

		self.track.finish()?;
		self.finished = true;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it, so subscribers see the
	/// real cause rather than [`moq_net::Error::Dropped`]. Pending samples are dropped.
	///
	/// Consumes the producer. Still callable after [`finish`](Self::finish).
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::decode::{Consumer as AudioConsumer, Options as DecodeOptions};
	use crate::{Activity, Format, Layout};

	#[tokio::test]
	async fn demand_follows_subscribers_and_closes_with_the_producer() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let options = Options {
			track: Some("audio".into()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, Input::default(), &options).unwrap();
		let demand = producer.demand();

		assert_eq!(demand.name(), "audio");
		assert!(!demand.is_used());
		let subscriber = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		tokio::time::timeout(Duration::from_secs(1), demand.used())
			.await
			.expect("subscription demand")
			.unwrap();
		assert!(demand.is_used());

		drop(subscriber);
		drop(consumer);
		tokio::time::timeout(Duration::from_secs(1), demand.unused())
			.await
			.expect("subscription released")
			.unwrap();
		assert!(!demand.is_used());

		producer.finish().unwrap();
		drop(producer);
		let closed = tokio::time::timeout(Duration::from_secs(1), demand.closed())
			.await
			.expect("producer closed");
		assert!(matches!(closed, moq_net::Error::Dropped));
	}

	/// Terminal Opus lookahead samples survive both exact-frame and partial-frame input.
	#[tokio::test]
	async fn finish_publishes_the_opus_lookahead_tail() {
		for frames in [960, 860] {
			let input = Input {
				format: Format::F32,
				sample_rate: 48_000,
				layout: Layout::Mono,
			};
			let options = Options {
				track: Some("audio".to_string()),
				settings: Settings {
					layout: Layout::Mono,
					bitrate: Some(moq_net::bandwidth::Rate::from_bps(128_000)),
					..Settings::default()
				},
				..Options::default()
			};
			let decoder_config = Encoder::new(&options.settings).unwrap().catalog();

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			let mut audio = AudioConsumer::new(
				&consumer,
				&decoder_config,
				"audio",
				DecodeOptions {
					max_age: Duration::from_secs(1),
					..DecodeOptions::new()
				},
			)
			.await
			.unwrap();

			let mut pcm = vec![0.0f32; frames];
			let impulse = pcm.len() - 100;
			pcm[impulse] = 1.0;
			let data: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
			producer.write(&Frame::new(Bytes::from(data), Timestamp::ZERO)).unwrap();
			producer.finish().unwrap();

			let mut decoded = Vec::new();
			while let Some(frame) = audio.read().await.unwrap() {
				let pcm = Format::F32.as_interleaved_f32(&frame.data, 1).unwrap();
				decoded.extend_from_slice(&pcm);
			}
			assert_eq!(decoded.len(), frames, "terminal padding extended the source");
			let peak = decoded.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
			assert!(peak > 0.1, "the {frames}-frame Opus tail lost the impulse: peak {peak}");
		}
	}

	/// A resampled publisher used to end its track early: `finish` flushed the
	/// encoder's own buffer but left the resampler holding its last partial chunk,
	/// plus the audio its filter runs behind on.
	#[tokio::test]
	async fn finish_publishes_the_resampled_tail() {
		let input = Input {
			format: Format::F32,
			sample_rate: 44_100,
			layout: Layout::Mono,
		};

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();
		let options = Options {
			track: Some("audio".to_string()),
			settings: Settings::new(48_000, Layout::Mono),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input.clone(), &options).unwrap();

		// Subscribe before the track ends, or there is nothing left to subscribe to.
		let mut track = moq_mux::container::Consumer::new(
			consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(1)))
				.await
				.unwrap(),
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio),
		);

		// Chosen so the tail decides a whole packet: 8838 frames at 44.1 kHz is ~9620
		// at 48 kHz, just past ten 960-sample Opus frames. Losing the resampler's
		// remainder and its filter delay drops back under ten, costing a packet.
		let data: Vec<u8> = vec![0.25f32; 8_838].iter().flat_map(|s| s.to_le_bytes()).collect();
		producer
			.write(&Frame::new(data.into(), moq_net::Timestamp::ZERO))
			.unwrap();
		producer.finish().unwrap();

		let mut packets = 0;
		while track.read().await.unwrap().is_some() {
			packets += 1;
		}
		assert_eq!(packets, 11);
	}

	/// `reset_epoch` promises to drop buffered samples, and the resampler buffers
	/// samples of its own. Leaving those behind let `finish` flush pre-reset audio
	/// onto the track, stamped at an epoch that no longer exists.
	#[tokio::test]
	async fn reset_epoch_drops_the_resampler_buffer_too() {
		let input = Input {
			format: Format::F32,
			sample_rate: 44_100,
			layout: Layout::Mono,
		};

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input.clone(), &options).unwrap();

		let mut track = moq_mux::container::Consumer::new(
			consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default())
				.await
				.unwrap(),
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio),
		);

		// Too little to publish a packet, so it all sits in the resampler.
		let data: Vec<u8> = vec![0.25f32; 441].iter().flat_map(|s| s.to_le_bytes()).collect();
		producer
			.write(&Frame::new(data.into(), moq_net::Timestamp::ZERO))
			.unwrap();

		producer.reset_epoch();
		producer.finish().unwrap();

		// The reset dropped everything, so the track ends without a packet.
		assert!(track.read().await.unwrap().is_none());
	}

	/// Resetting after a full frame drops codec lookahead as well as producer buffers.
	#[tokio::test]
	async fn reset_epoch_drops_the_encoder_lookahead() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(
			&mut broadcast,
			catalog,
			Input {
				layout: Layout::Mono,
				..Input::default()
			},
			&options,
		)
		.unwrap();
		let mut track = moq_mux::container::Consumer::new(
			consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default())
				.await
				.unwrap(),
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio),
		);

		producer.write(&full_frame(1_000_000)).unwrap();
		producer.reset_epoch();
		producer.finish().unwrap();

		assert!(track.read().await.unwrap().is_some());
		assert!(track.read().await.unwrap().is_none());
	}

	/// A codec reset starts a new pre-skip interval at the receiver too.
	#[tokio::test]
	async fn reset_epoch_restarts_the_decoder() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			layout: Layout::Mono,
		};
		let options = Options {
			track: Some("audio".to_string()),
			settings: Settings::new(48_000, Layout::Mono),
			..Options::default()
		};
		let decoder_config = Encoder::new(&options.settings).unwrap().catalog();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
		let mut audio = AudioConsumer::new(
			&subscriber,
			&decoder_config,
			"audio",
			DecodeOptions {
				max_age: Duration::from_millis(500),
				..DecodeOptions::new()
			},
		)
		.await
		.unwrap();

		producer.write(&full_frame(0)).unwrap();
		let first = audio.read().await.unwrap().expect("first epoch packet");
		assert_eq!(first.data.len() / size_of::<f32>(), 960 - 312);

		producer.reset_epoch();
		producer.write(&full_frame(1_000_000)).unwrap();
		producer.finish().unwrap();

		let mut resumed_frames = 0;
		while let Some(frame) = audio.read().await.unwrap() {
			if frame.timestamp.as_micros() >= 1_000_000 {
				resumed_frames += frame.data.len() / size_of::<f32>();
			}
		}
		assert!(resumed_frames > 0, "the resumed epoch still decodes");
	}

	#[tokio::test]
	async fn producer_and_consumer_keep_activity_on_the_audio_stream() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			layout: Layout::Mono,
		};
		let options = Options {
			track: Some("audio".to_string()),
			settings: Settings {
				layout: Layout::Mono,
				bitrate: Some(moq_net::bandwidth::Rate::from_bps(24_000)),
				dtx: true,
				..Settings::default()
			},
			..Options::default()
		};
		let decoder_config = Encoder::new(&options.settings).unwrap().catalog();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
		let mut consumer = AudioConsumer::new(&subscriber, &decoder_config, "audio", DecodeOptions::new())
			.await
			.unwrap();

		let silence = vec![0.0; 960];
		let mut entered_dtx = false;
		for index in 0..100 {
			producer.write(&pcm_frame(&silence, index * 20_000)).unwrap();
			let consumed = consumer.read().await.unwrap().expect("one decoded frame");
			assert_eq!(producer.activity(), consumed.activity);
			if consumed.activity.is_dtx() {
				entered_dtx = true;
				break;
			}
		}
		assert!(entered_dtx, "silence should enter Opus DTX");

		let active: Vec<f32> = (0..960)
			.map(|sample| {
				let phase = sample as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0;
				phase.sin() * 0.5
			})
			.collect();
		producer.write(&pcm_frame(&active, 2_000_000)).unwrap();
		let consumed = consumer.read().await.unwrap().expect("one decoded frame");
		assert_eq!(producer.activity(), Activity::Active);
		assert_eq!(consumed.activity, Activity::Active);
	}

	// One 20 ms Opus frame at 48 kHz mono is exactly 960 f32 samples, so each
	// `write` of this drains precisely one packet (no resampler, no leftover).
	fn full_frame(timestamp_us: u64) -> Frame {
		pcm_frame(&vec![0.1; 960], timestamp_us)
	}

	fn pcm_frame(samples: &[f32], timestamp_us: u64) -> Frame {
		let data: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
		Frame::new(Bytes::from(data), Timestamp::from_micros(timestamp_us).unwrap())
	}

	/// Publish each frame and read back the resulting packet PTS (microseconds).
	/// If `reset_before` contains an index, `reset_epoch()` is called before that
	/// frame's `write`.
	async fn published_pts(frames: &[Frame], reset_before: Option<usize>) -> Vec<u128> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();

		// Input rate == Opus codec rate, so there's no resampler and sample
		// counts stay exact, making the PTS assertions deterministic.
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			layout: Layout::Mono,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();

		let track = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		let mut reader =
			moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio));

		let mut pts = Vec::new();
		for (i, frame) in frames.iter().enumerate() {
			if reset_before == Some(i) {
				producer.reset_epoch();
			}
			producer.write(frame).unwrap();
			let read = reader.read().await.unwrap().expect("a packet per full frame");
			pts.push(read.timestamp.as_micros());
		}
		pts
	}

	#[tokio::test]
	async fn epoch_anchors_to_first_frame_timestamp() {
		// The first frame's timestamp becomes the epoch (regression guard: the
		// old code derived PTS purely from the sample count, always near 0).
		let pts = published_pts(&[full_frame(1_000_000)], None).await;
		assert_eq!(pts, vec![1_000_000]);
	}

	/// The encoder needs no correction for the resampler's own delay: it anchors
	/// the epoch to the first input timestamp and advances by emitted samples,
	/// while `Resampler::process` drops its startup silence rather than passing it
	/// on, so the first sample published is the first sample written. Anything
	/// that let that delay through would shift every PTS on a resampled track,
	/// which no `reset_epoch` would ever correct.
	#[tokio::test]
	async fn resampling_does_not_shift_the_first_pts() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();

		// 44.1 kHz in, and Opus only runs at 48 kHz, so this one resamples.
		let input = Input {
			format: Format::F32,
			sample_rate: 44_100,
			layout: Layout::Mono,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();

		// A whole second goes out before the first read, so the default budget of zero
		// would shed all but the last packet and the first PTS read would be the tail's.
		let track = consumer
			.track("audio")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(1)))
			.await
			.unwrap();
		let mut reader =
			moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio));

		// A second of audio, so the filter's delay is nowhere near the whole write.
		producer.write(&pcm_frame(&vec![0.1; 44_100], 1_000_000)).unwrap();

		let first = reader.read().await.unwrap().expect("a packet");
		assert_eq!(first.timestamp.as_micros(), 1_000_000);
	}

	#[tokio::test]
	async fn pts_advances_by_frame_duration_ignoring_later_timestamps() {
		// Second frame's own timestamp (way ahead) is ignored; PTS advances by
		// exactly one 20 ms frame from the epoch.
		let pts = published_pts(&[full_frame(1_000), full_frame(999_999)], None).await;
		assert_eq!(pts, vec![1_000, 1_000 + 20_000]);
	}

	#[tokio::test]
	async fn reset_epoch_reanchors_so_the_gap_lands_in_pts() {
		// Frame at t=0, then reset_epoch + a frame at t=5s: the 5 s idle gap must
		// appear in the PTS (otherwise audio drifts behind a wall-clock video track).
		let pts = published_pts(&[full_frame(0), full_frame(5_000_000)], Some(1)).await;
		assert_eq!(pts, vec![0, 5_000_000]);
	}

	/// Finish leaves the handle, so abort can still run.
	#[tokio::test]
	async fn abort_after_finish() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(
			&mut broadcast,
			catalog,
			Input {
				layout: Layout::Mono,
				..Input::default()
			},
			&options,
		)
		.unwrap();
		let mut track = moq_mux::container::Consumer::new(
			consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default())
				.await
				.unwrap(),
			moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio),
		);

		producer.write(&full_frame(0)).unwrap();
		producer.finish().unwrap();
		assert!(track.read().await.unwrap().is_some());
		producer.abort(moq_net::Error::Cancel);
	}

	/// A sub-frame write never reaches the closed track, so the producer must
	/// refuse it itself rather than buffering samples that cannot be published.
	#[tokio::test]
	async fn write_after_finish_is_closed() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(
			&mut broadcast,
			catalog,
			Input {
				layout: Layout::Mono,
				..Input::default()
			},
			&options,
		)
		.unwrap();

		producer.finish().unwrap();
		let err = producer.write(&pcm_frame(&[0.1; 100], 0)).unwrap_err();
		assert!(matches!(err, Error::Net(moq_net::Error::Closed)));
		producer.abort(moq_net::Error::Cancel);
	}

	/// `Options::track = None` derives a codec-suffixed name rather than making
	/// the caller invent one, mirroring the video side. Pins the exact name the
	/// docs promise, and that a second producer doesn't collide with the first.
	#[tokio::test]
	async fn default_options_derive_the_track_name() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let first = Producer::new(&mut broadcast, catalog.clone(), Input::default(), &Options::default()).unwrap();
		assert_eq!(first.track_name(), "0.opus");

		let second = Producer::new(&mut broadcast, catalog, Input::default(), &Options::default()).unwrap();
		assert_eq!(second.track_name(), "1.opus");
	}
}

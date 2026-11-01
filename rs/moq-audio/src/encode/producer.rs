//! Encode raw PCM and publish it as a moq audio track.

use bytes::Bytes;

use moq_mux::catalog::hang::CatalogExt;
use moq_mux::container::Frame as MuxFrame;
use moq_net::Timestamp;

use super::encoded::Encoded;
use super::encoder::{Config, Encoder, Input};
use crate::resample::{Remix, Resampler};
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
	/// Codec input and encoded output config.
	pub config: Config,
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
	/// The minimum audio carried by each group. The packet that reaches it closes
	/// the group, and the next packet opens a new one.
	///
	/// Defaults to 20 ms; zero puts every packet in its own group. Each group costs the relay a stream
	/// and its bookkeeping, so raising this trades per-group overhead for coarser
	/// loss: a viewer that falls behind skips a whole group, and a lost packet
	/// stalls the rest of its group until retransmitted. Packets still forward as
	/// they are encoded rather than buffered until the group fills. A longer
	/// [`Config::frame_duration`] (up to 60 ms for Opus) also packs more audio
	/// per group, at the cost of encoder latency. An unfinished group remains open
	/// across a pause or [`Producer::reanchor`] until the next write; use
	/// [`Producer::discontinuity`] to close it when capture stops.
	pub group_duration: std::time::Duration,
}

impl Default for Options {
	fn default() -> Self {
		Self {
			track: None,
			config: Config::default(),
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			group_duration: std::time::Duration::from_millis(20),
		}
	}
}

/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// The input PCM layout is fixed at construction via [`Input`]; the codec
/// config via [`Options`]. Subsequent [`write`](Self::write) calls just pass a
/// [`Frame`]: payload bytes and a timestamp.
///
/// The catalog rendition is registered at construction (not on first write), so
/// a subscriber that opens the catalog before any frames arrive still sees the
/// track.
pub struct Producer {
	// None after a successful finish: keep the track and catalog rendition, not the codec.
	encoder: Option<Encoder>,
	bitrate: moq_net::bandwidth::Rate,
	input: Input,
	/// Converts the input layout to the codec's, when they differ.
	remix: Option<Remix>,
	resampler: Option<Resampler>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
	pending: Vec<f32>,
	/// Codec samples a group carries before the packet reaching it closes the group.
	group_samples: u64,
	/// Codec samples in the open group.
	grouped: u64,
	/// Samples emitted since the current anchor (reset by [`reanchor`](Self::reanchor)).
	frames_produced: u64,
	/// Wall-clock anchor in microseconds, taken from the first frame after each
	/// (re)start. Emitted PTS = `anchor + frames_produced / codec_rate`. `None`
	/// until the first write so the next frame re-anchors to its timestamp.
	anchor_us: Option<u64>,
	/// An encoder reset that still needs a marker group before its next packet.
	pending_discontinuity: bool,
	/// Whether a marker group already separates the next packet from prior codec state.
	decoder_boundary: bool,
	/// How the encoder classified the packet it published most recently.
	activity: Activity,
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
pub(crate) struct Reserved {
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire, hang::catalog::AudioConfig>,
}

impl Reserved {
	pub(crate) fn new<E: CatalogExt>(
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
				&format!(".{}", options.config.codec),
				catalog.track_info(hang::catalog::PRIORITY.audio),
			)?,
		};
		let track = catalog.audio(
			track,
			moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio),
			None,
		)?;

		Ok(Self { track })
	}

	/// Build the encoder for `input` and register the rendition describing it.
	///
	/// Separate from [`encode`](Self::encode), which cannot fail, so a layout the
	/// codec rejects leaves the reservation intact for another input.
	pub(crate) fn register(&mut self, input: Input, options: &Options) -> Result<Registered, Error> {
		let remix = (input.layout != options.config.layout)
			.then(|| Remix::new(input.layout, options.config.layout))
			.transpose()?;
		let encoder = Encoder::new(&options.config)?;

		let resampler = if input.sample_rate == encoder.codec_rate() {
			None
		} else {
			// Use microsecond precision so 2.5 ms frame_duration (supported by
			// libopus) doesn't truncate to 2 ms.
			let chunk_frames =
				((input.sample_rate as u128 * encoder.config().frame_duration.as_micros()) / 1_000_000) as usize;
			Some(Resampler::new(
				input.sample_rate,
				encoder.codec_rate(),
				encoder.codec_channels(),
				chunk_frames,
			)?)
		};

		self.track.set(encoder.catalog())?;

		let group_samples = options.group_duration.as_nanos() * u128::from(encoder.codec_rate());
		let group_samples = u64::try_from(group_samples.div_ceil(1_000_000_000)).unwrap_or(u64::MAX);

		Ok(Registered {
			encoder,
			input,
			remix,
			resampler,
			group_samples,
		})
	}

	/// Spend the reservation on a registered encoder, publishing through the track.
	pub(crate) fn encode(self, registered: Registered) -> Producer {
		Producer {
			bitrate: registered.encoder.bitrate(),
			encoder: Some(registered.encoder),
			input: registered.input,
			remix: registered.remix,
			resampler: registered.resampler,
			track: self.track,
			pending: Vec::new(),
			group_samples: registered.group_samples,
			grouped: 0,
			frames_produced: 0,
			anchor_us: None,
			pending_discontinuity: false,
			decoder_boundary: true,
			activity: Activity::Active,
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
	remix: Option<Remix>,
	resampler: Option<Resampler>,
	group_samples: u64,
}

/// What a capture publication needs while its layout is still undiscovered.
#[cfg(feature = "capture")]
impl Reserved {
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

impl Producer {
	/// Publish a track encoding `input` into `broadcast`, registering its
	/// rendition in `catalog` immediately.
	pub fn new<E: CatalogExt>(
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
	/// [`wanted`](moq_net::track::Demand::wanted) / [`wanted_changed`](moq_net::track::Demand::wanted_changed).
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
		self.bitrate
	}

	/// Retune the live encoder to `bitrate`.
	pub fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
		let encoder = self.encoder.as_mut().ok_or(moq_net::Error::Closed)?;
		let previous = encoder.bitrate();
		encoder.set_bitrate(bitrate)?;
		// Advertise the configured target in place, so encoded-byte estimates cannot replace it.
		if let Err(err) = self.track.set_bitrate(encoder.bitrate().as_bps()) {
			// Keep the encoder on what the catalog still advertises. It accepted that rate before,
			// so the publish error is the one to report.
			let _ = encoder.set_bitrate(previous);
			return Err(err.into());
		}
		self.bitrate = encoder.bitrate();
		Ok(())
	}

	fn encoder(&self) -> Result<&Encoder, Error> {
		self.encoder.as_ref().ok_or(moq_net::Error::Closed.into())
	}

	/// Re-anchor the timeline to the next frame's timestamp, dropping any
	/// buffered samples. Call this when resuming after an idle gap (e.g. a
	/// released-then-reopened microphone) so the gap appears in the PTS and
	/// audio stays aligned with a wall-clock video track, rather than the gap
	/// being compressed out by the running sample count. Mirrors moq-boy's
	/// `reanchor`. If the codec had started, a marker group is published before
	/// the next packet so subscribers jump the playhead.
	pub fn reanchor(&mut self) {
		if self.encoder.as_ref().is_some_and(Encoder::started) && !self.decoder_boundary {
			self.pending_discontinuity = true;
		}
		self.reset_state();
	}

	fn reset_state(&mut self) {
		self.anchor_us = None;
		self.activity = Activity::Active;
		self.frames_produced = 0;
		self.pending.clear();
		if let Some(encoder) = self.encoder.as_mut() {
			encoder.reset();
		}
		// The resampler holds samples of its own, plus filter state primed by them.
		// Left alone, `finish` would flush that pre-reset audio onto the track
		// (stamped at an anchor that no longer exists), and the next write would run
		// the new audio through a filter still ringing with the old.
		if let Some(resampler) = self.resampler.as_mut() {
			resampler.reset();
		}
	}

	/// Push one [`Frame`] of PCM in the layout declared by [`Input`]. Encodes and
	/// publishes as many packets as the input contains; any partial trailing
	/// frame is carried to the next call.
	///
	/// The first frame after construction (or [`reanchor`](Self::reanchor))
	/// anchors the timeline: its timestamp becomes the anchor, and emitted PTS
	/// then advances purely by the running sample count, so subsequent frames'
	/// timestamps are ignored. An idle gap is only reflected in the PTS if you
	/// call [`reanchor`](Self::reanchor) on resume (which re-anchors from
	/// the next frame's wall-clock stamp); writing straight across a gap without
	/// resetting compresses it out.
	///
	/// [`Frame::activity`] is ignored: the encoder classifies what it actually
	/// produced, which [`activity`](Self::activity) reports.
	///
	/// Writes after a successful [`finish`](Self::finish) fail with
	/// [`moq_net::Error::Closed`].
	pub fn write(&mut self, frame: &Frame) -> Result<(), Error> {
		if self.encoder.is_none() {
			return Err(moq_net::Error::Closed.into());
		}
		if self.pending_discontinuity {
			self.track.discontinuity()?;
			self.pending_discontinuity = false;
			self.decoder_boundary = true;
		}

		let timestamp_us = u64::try_from(frame.timestamp.as_micros())
			.map_err(|_| Error::Unsupported(format!("frame timestamp {:?} out of range", frame.timestamp)))?;
		let anchor_us = *self.anchor_us.get_or_insert(timestamp_us);

		let input = &self.input;
		let (format, channels) = (input.format, input.layout.channels());
		let pcm = format.as_interleaved_f32(frame.data.as_ref(), channels)?;
		let pcm = match &self.remix {
			Some(remix) => remix.process(&pcm),
			None => pcm.into_owned(),
		};
		let pcm: Vec<f32> = match self.resampler.as_mut() {
			Some(r) => r.process(&pcm, frame.timestamp)?,
			None => pcm,
		};

		self.pending.extend(pcm);

		self.publish_full_frames(anchor_us)
	}

	/// Encode and publish every full frame in `pending`, keeping any partial
	/// trailing frame for the next call.
	fn publish_full_frames(&mut self, anchor_us: u64) -> Result<(), Error> {
		let frame_samples = self.encoder()?.frame_size() * self.encoder()?.codec_channels() as usize;
		while self.pending.len() >= frame_samples {
			let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
			let packet = self.encoder.as_mut().ok_or(moq_net::Error::Closed)?.encode(&chunk)?;

			let timestamp = Self::timestamp(
				anchor_us,
				self.frames_produced,
				self.encoder()?.folded_delay(),
				self.encoder()?.codec_rate(),
			)?;
			self.frames_produced += self.encoder()?.frame_size() as u64;
			self.activity = packet.activity;
			self.publish(packet, timestamp)?;
			self.decoder_boundary = false;
		}

		Ok(())
	}

	/// PTS of the frame `frames` samples past the anchor, stamped `delay` samples
	/// earlier to fold in codec priming the catalog can't signal.
	///
	/// Priming that would land before a zero anchor is stamped at zero instead: it
	/// decodes to the codec's warm-up rather than to input, so only its spacing is
	/// lost.
	fn timestamp(anchor_us: u64, frames: u64, delay: usize, codec_rate: u32) -> Result<Timestamp, Error> {
		let frames = i128::from(frames) - delay as i128;
		let offset_us = (frames * 1_000_000).div_euclid(i128::from(codec_rate));
		let micros = (i128::from(anchor_us) + offset_us).max(0);
		let micros = u64::try_from(micros).map_err(|_| moq_net::TimeOverflow)?;
		Ok(Timestamp::from_micros(micros)?)
	}

	fn publish(&mut self, encoded: Encoded, timestamp: Timestamp) -> Result<(), Error> {
		// Every packet decodes on its own, so a group may start at any of them: the first packet
		// after a cut opens one. Codecs recover independently after a dropped group.
		let keyframe = self.track.needs_keyframe();
		if keyframe {
			self.grouped = 0;
		}
		let mux_frame = MuxFrame {
			timestamp,
			payload: encoded.payload,
			keyframe,
			duration: None,
		};
		self.track.write(mux_frame)?;
		self.grouped += self.encoder()?.frame_size() as u64;
		// Cut as soon as the group is long enough rather than when the next packet arrives, so
		// the relay sees it finish without waiting. No boundary to give: the next packet bounds
		// this one, and codec frames have a deterministic duration anyway.
		if self.grouped >= self.group_samples {
			self.track.cut(None)?;
		}
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
			let timestamp = terminal.start.checked_add(offset)?;
			track.write(MuxFrame {
				timestamp,
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
	/// switching source, or anything else that resumes on a new anchor. Buffered samples
	/// are dropped, and the next frame sets a fresh anchor. See
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
	/// run after a successful finish. The codec and input scratch are released, while the
	/// track and its catalog rendition stay published. Writes and bitrate changes then
	/// fail with [`moq_net::Error::Closed`].
	pub fn finish(&mut self) -> Result<(), Error> {
		if self.encoder.is_none() {
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
		let anchor_us = self.anchor_us.unwrap_or(0);
		self.publish_full_frames(anchor_us)?;

		let frame_size = self.encoder()?.frame_size();
		let codec_rate = self.encoder()?.codec_rate();
		let channels = self.encoder()?.codec_channels() as usize;
		let source_frames = self.pending.len() / channels;
		let delay = self.encoder()?.folded_delay();
		let start = Self::timestamp(anchor_us, self.frames_produced, delay, codec_rate)?;
		// The source ends where it ends: priming only moves the packets carrying it.
		let end = Self::timestamp(anchor_us, self.frames_produced + source_frames as u64, 0, codec_rate)?;
		let finish = self
			.encoder
			.as_mut()
			.ok_or(moq_net::Error::Closed)?
			.drain(&self.pending)?;
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
				let timestamp = Self::timestamp(anchor_us, self.frames_produced, delay, codec_rate)?;
				self.activity = packet.activity;
				self.publish(packet, timestamp)?;
				self.frames_produced += frame_size as u64;
			}
		}

		self.track.finish()?;
		self.encoder = None;
		self.pending = Vec::new();
		self.remix = None;
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
	async fn decoded_frame(consumer: &mut crate::decode::Consumer) -> Result<Option<Frame>, Error> {
		loop {
			match consumer.read().await? {
				Some(crate::decode::Read::Frame(frame)) => return Ok(Some(frame)),
				Some(crate::decode::Read::Gap { .. } | crate::decode::Read::End { .. }) => {}
				None => return Ok(None),
			}
		}
	}

	use std::time::Duration;

	use super::*;
	use crate::decode::{Consumer as AudioConsumer, Options as DecodeOptions};
	use crate::{Activity, Format, Layout};

	/// The next published packet, passing over gaps and ends.
	async fn packet<F: moq_mux::container::Container>(reader: &mut moq_mux::container::Consumer<F>) -> Option<MuxFrame>
	where
		F::Error: std::fmt::Debug,
	{
		loop {
			if let moq_mux::container::Read::Frame { frame, .. } = reader.read().await.unwrap()? {
				return Some(frame);
			}
		}
	}

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
		let mut demand = producer.demand();

		assert_eq!(demand.name(), "audio");
		assert_eq!(demand.wanted(), None);
		let subscriber = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		let wanted = async {
			while demand.wanted_changed().await?.is_none() {}
			moq_net::Result::Ok(())
		};
		kio::clock::timeout(Duration::from_secs(1), wanted)
			.await
			.expect("subscription demand")
			.unwrap();
		assert_eq!(demand.wanted(), Some(0));

		drop(subscriber);
		drop(consumer);
		let unwanted = async {
			while demand.wanted_changed().await?.is_some() {}
			moq_net::Result::Ok(())
		};
		kio::clock::timeout(Duration::from_secs(1), unwanted)
			.await
			.expect("subscription released")
			.unwrap();
		assert_eq!(demand.wanted(), None);

		producer.finish().unwrap();
		drop(producer);
		let closed = kio::clock::timeout(Duration::from_secs(1), demand.closed())
			.await
			.expect("producer closed");
		assert!(closed.is_ok(), "{closed:?}");
	}

	/// A finished publication may live as long as its broadcast; the codec and input
	/// allocation count must stay flat as more tracks retire on it.
	#[tokio::test(start_paused = true)]
	async fn finished_publications_release_encoding_resources() {
		let backend = crate::encode::backend::stub::install();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut retired = Vec::new();
		for index in 0..16 {
			let input = Input {
				format: Format::F32,
				sample_rate: 44_100,
				layout: Layout::Stereo,
			};
			let options = Options {
				track: Some(format!("audio-{index}")),
				config: Config::from_input(crate::encode::Codec::Aac, &Input::new(48_000, Layout::Mono)),
				..Options::default()
			};
			let mut producer = Producer::new(&mut broadcast, catalog.clone(), input, &options).unwrap();
			assert_eq!(backend.resources(), 1);
			producer.write(&pcm_frame(&vec![0.1; 4096], 0)).unwrap();
			assert!(producer.pending.capacity() > 0);
			assert!(producer.remix.is_some());
			let bitrate = producer.bitrate();
			producer.finish().unwrap();
			assert_eq!(backend.resources(), 0, "retired {index} codecs");
			assert_eq!(producer.pending.capacity(), 0);
			assert!(producer.remix.is_none());
			assert!(producer.resampler.is_none());
			assert_eq!(producer.bitrate(), bitrate);
			producer.finish().unwrap();
			assert!(matches!(
				producer.set_bitrate(bitrate),
				Err(Error::Net(moq_net::Error::Closed))
			));
			retired.push(producer);

			// Retention still owns both the published track and its catalog rendition.
			let mut reader = consumer
				.track(&format!("audio-{index}"))
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let mut frames = 0;
			while let Some(mut group) = reader.recv_group().await.unwrap() {
				while group.read_frame().await.unwrap().is_some() {
					frames += 1;
				}
			}
			assert!(frames > 0);
			assert_eq!(catalog.snapshot().audio.renditions.len(), index + 1);
		}
		assert_eq!(backend.resources(), 0);
		assert_eq!(retired.len(), 16);
	}

	/// Terminal Opus lookahead samples survive both exact-frame and partial-frame input.
	#[tokio::test]
	async fn finish_publishes_the_opus_lookahead_tail() {
		for (group_ms, frames) in [(0, 960), (0, 860), (100, 2_880), (100, 2_780)] {
			let input = Input {
				format: Format::F32,
				sample_rate: 48_000,
				layout: Layout::Mono,
			};
			let options = Options {
				track: Some("audio".to_string()),
				group_duration: Duration::from_millis(group_ms),
				config: Config {
					layout: Layout::Mono,
					bitrate: Some(moq_net::bandwidth::Rate::from_bps(128_000)),
					..Config::default()
				},
				..Options::default()
			};
			let mut decoder_config = Encoder::new(&options.config).unwrap().catalog();
			// Every packet counts here; the consumer's warmup tests cover trimming.
			decoder_config.warmup = None;

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			let mut audio = AudioConsumer::new(
				&consumer,
				&decoder_config,
				"audio",
				DecodeOptions {
					max_delay: Duration::from_secs(1),
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
			while let Some(frame) = decoded_frame(&mut audio).await.unwrap() {
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
			config: Config::new(48_000, Layout::Mono),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input.clone(), &options).unwrap();

		// Subscribe before the track ends, or there is nothing left to subscribe to.
		let mut track = moq_mux::container::Consumer::new(
			consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
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
		while packet(&mut track).await.is_some() {
			packets += 1;
		}
		assert_eq!(packets, 11);
	}

	/// `reanchor` promises to drop buffered samples, and the resampler buffers
	/// samples of its own. Leaving those behind let `finish` flush pre-reset audio
	/// onto the track, stamped at an anchor that no longer exists.
	#[tokio::test]
	async fn reanchor_drops_the_resampler_buffer_too() {
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

		producer.reanchor();
		producer.finish().unwrap();

		// The reset dropped everything, so the track ends without a packet.
		assert!(track.read().await.unwrap().is_none());
	}

	/// Resetting after a full frame drops codec lookahead as well as producer buffers.
	#[tokio::test]
	async fn reanchor_drops_the_encoder_lookahead() {
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
		producer.reanchor();
		producer.finish().unwrap();

		assert!(packet(&mut track).await.is_some());
		assert!(packet(&mut track).await.is_none());
	}

	/// A codec reset starts a new pre-skip interval at the receiver too.
	#[tokio::test]
	async fn reanchor_restarts_the_decoder() {
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			layout: Layout::Mono,
		};
		let options = Options {
			track: Some("audio".to_string()),
			config: Config::new(48_000, Layout::Mono),
			..Options::default()
		};
		let mut decoder_config = Encoder::new(&options.config).unwrap().catalog();
		// Every packet counts here; the consumer's warmup tests cover trimming.
		decoder_config.warmup = None;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
		let mut audio = AudioConsumer::new(
			&subscriber,
			&decoder_config,
			"audio",
			DecodeOptions {
				max_delay: Duration::from_millis(500),
				..DecodeOptions::new()
			},
		)
		.await
		.unwrap();

		producer.write(&full_frame(0)).unwrap();
		let first = decoded_frame(&mut audio)
			.await
			.unwrap()
			.expect("first packet before the reanchor");
		assert_eq!(first.data.len() / size_of::<f32>(), 960 - 312);

		producer.reanchor();
		producer.write(&full_frame(1_000_000)).unwrap();
		producer.finish().unwrap();

		let mut resumed_frames = 0;
		while let Some(frame) = decoded_frame(&mut audio).await.unwrap() {
			if frame.timestamp.as_micros() >= 1_000_000 {
				resumed_frames += frame.data.len() / size_of::<f32>();
			}
		}
		assert!(resumed_frames > 0, "the reanchored audio still decodes");
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
			config: Config {
				layout: Layout::Mono,
				bitrate: Some(moq_net::bandwidth::Rate::from_bps(24_000)),
				dtx: true,
				..Config::default()
			},
			..Options::default()
		};
		let mut decoder_config = Encoder::new(&options.config).unwrap().catalog();
		// Every packet counts here; the consumer's warmup tests cover trimming.
		decoder_config.warmup = None;

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
			let consumed = decoded_frame(&mut consumer).await.unwrap().expect("one decoded frame");
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
		let consumed = decoded_frame(&mut consumer).await.unwrap().expect("one decoded frame");
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
	/// If `reset_before` contains an index, `reanchor()` is called before that
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
				producer.reanchor();
			}
			producer.write(frame).unwrap();
			let read = packet(&mut reader).await.expect("a packet per full frame");
			pts.push(read.timestamp.as_micros());
		}
		pts
	}

	/// The packet that reaches the group duration closes its group, so a minimum that
	/// isn't a whole number of packets rounds up.
	#[tokio::test]
	async fn group_duration_packs_packets_per_group() {
		for (group_ms, expected) in [(0, [1, 1]), (30, [2, 2]), (100, [5, 5])] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();
			let input = Input {
				format: Format::F32,
				sample_rate: 48_000,
				layout: Layout::Mono,
			};
			let options = Options {
				track: Some("audio".to_string()),
				group_duration: Duration::from_millis(group_ms),
				..Options::default()
			};
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			let mut track = consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
				.await
				.unwrap();

			for index in 0..10 {
				producer.write(&full_frame(index * 20_000)).unwrap();
			}

			let mut sizes = Vec::new();
			for _ in expected {
				let mut group = track.recv_group().await.unwrap().expect("a group");
				let mut frames = 0;
				while group.read_frame().await.unwrap().is_some() {
					frames += 1;
				}
				sizes.push(frames);
			}
			assert_eq!(sizes, expected, "group duration {group_ms} ms");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn default_groups_carry_twenty_milliseconds() {
		for (frame_us, packets) in [(2_500, 8), (10_000, 2), (20_000, 1), (40_000, 1)] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let input = Input {
				format: Format::F32,
				sample_rate: 48_000,
				layout: Layout::Mono,
			};
			let mut options = Options {
				track: Some("audio".into()),
				..Default::default()
			};
			options.config.layout = Layout::Mono;
			options.config.frame_duration = Duration::from_micros(frame_us);
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			let mut track = consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
				.await
				.unwrap();
			for index in 0..packets * 2 {
				producer
					.write(&pcm_frame(
						&vec![0.1; (frame_us * 48_000 / 1_000_000) as usize],
						1_000_000 + index * frame_us,
					))
					.unwrap();
			}
			for _ in 0..2 {
				let mut group = track.recv_group().await.unwrap().unwrap();
				let mut count = 0;
				while group.read_frame().await.unwrap().is_some() {
					count += 1;
				}
				assert_eq!(count, packets, "{frame_us} us packets");
			}
			producer.finish().unwrap();
		}
	}

	/// Timeline breaks close a partial group and reset the resumed group's packet count.
	#[tokio::test]
	async fn group_duration_restarts_after_a_timeline_break() {
		for deferred in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();
			let input = Input {
				format: Format::F32,
				sample_rate: 48_000,
				layout: Layout::Mono,
			};
			let options = Options {
				track: Some("audio".to_string()),
				group_duration: Duration::from_millis(100),
				..Options::default()
			};
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			let mut track = consumer.track("audio").unwrap().subscribe(None).await.unwrap();

			for index in 0..2 {
				producer.write(&full_frame(index * 20_000)).unwrap();
			}
			let mut partial = track.recv_group().await.unwrap().expect("partial group");
			for _ in 0..2 {
				assert!(
					partial.read_frame().await.unwrap().is_some(),
					"packets forward before the group fills"
				);
			}
			assert!(partial.poll_read_frame(&moq_net::kio::Waiter::noop()).is_pending());

			if deferred {
				producer.reanchor();
				assert!(partial.poll_read_frame(&moq_net::kio::Waiter::noop()).is_pending());
			} else {
				producer.discontinuity().unwrap();
				assert!(partial.read_frame().await.unwrap().is_none());
			}
			for index in 0..5 {
				producer.write(&full_frame(1_000_000 + index * 20_000)).unwrap();
			}
			assert!(partial.read_frame().await.unwrap().is_none());
			let mut resumed = track.recv_group().await.unwrap().expect("resumed group");
			assert_eq!(
				resumed.sequence,
				partial.sequence + 2,
				"one marker separates the anchors"
			);
			for _ in 0..5 {
				assert!(resumed.read_frame().await.unwrap().is_some());
			}
			assert!(resumed.read_frame().await.unwrap().is_none());
		}
	}

	#[tokio::test]
	async fn anchors_to_first_frame_timestamp() {
		// The first frame's timestamp becomes the anchor (regression guard: the
		// old code derived PTS purely from the sample count, always near 0).
		let pts = published_pts(&[full_frame(1_000_000)], None).await;
		assert_eq!(pts, vec![1_000_000]);
	}

	/// AAC can't signal its encoder delay, so each packet is stamped that much
	/// earlier and the first input sample still decodes at the anchor.
	#[tokio::test]
	async fn aac_folds_the_encoder_delay_into_timestamps() {
		async fn pts(anchor_us: u64) -> Vec<u128> {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();

			let input = Input::new(48_000, Layout::Mono);
			let options = Options {
				track: Some("audio".to_string()),
				config: Config::from_input(crate::encode::Codec::Aac, &input),
				..Options::default()
			};
			let stub = crate::encode::backend::stub::install();
			let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();
			drop(stub);

			let track = consumer
				.track("audio")
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
				.await
				.unwrap();
			let mut reader = moq_mux::container::Consumer::new(
				track,
				moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio),
			);

			producer.write(&pcm_frame(&[0.1; 4 * 1024], anchor_us)).unwrap();
			let mut pts = Vec::new();
			for _ in 0..4 {
				pts.push(packet(&mut reader).await.expect("a packet").timestamp.as_micros());
			}
			pts
		}

		// 2112 frames of delay at 48 kHz is 44 ms, rounded down per packet.
		assert_eq!(pts(1_000_000).await, vec![956_000, 977_333, 998_666, 1_020_000]);
		// Priming before a zero anchor stamps at zero; the input still starts on time.
		assert_eq!(pts(0).await, vec![0, 0, 0, 20_000]);
	}

	/// The encoder needs no correction for the resampler's own delay: it anchors
	/// the anchor to the first input timestamp and advances by emitted samples,
	/// while `Resampler::process` drops its startup silence rather than passing it
	/// on, so the first sample published is the first sample written. Anything
	/// that let that delay through would shift every PTS on a resampled track,
	/// which no `reanchor` would ever correct.
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
			.await
			.unwrap();
		let mut reader =
			moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio));

		// A second of audio, so the filter's delay is nowhere near the whole write.
		producer.write(&pcm_frame(&vec![0.1; 44_100], 1_000_000)).unwrap();

		let first = packet(&mut reader).await.expect("a packet");
		assert_eq!(first.timestamp.as_micros(), 1_000_000);
	}

	#[tokio::test]
	async fn pts_advances_by_frame_duration_ignoring_later_timestamps() {
		// Second frame's own timestamp (way ahead) is ignored; PTS advances by
		// exactly one 20 ms frame from the anchor.
		let pts = published_pts(&[full_frame(1_000), full_frame(999_999)], None).await;
		assert_eq!(pts, vec![1_000, 1_000 + 20_000]);
	}

	#[tokio::test]
	async fn reanchor_lets_the_gap_land_in_pts() {
		// Frame at t=0, then reanchor + a frame at t=5s: the 5 s idle gap must
		// appear in the PTS (otherwise audio drifts behind a wall-clock video track).
		let pts = published_pts(&[full_frame(0), full_frame(5_000_000)], Some(1)).await;
		assert_eq!(pts, vec![0, 5_000_000]);
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

	#[tokio::test(start_paused = true)]
	async fn retuning_updates_the_published_bitrate_in_place() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let options = Options {
			track: Some("audio".into()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog.clone(), Input::default(), &options).unwrap();
		let before = catalog.snapshot().audio.renditions["audio"].clone();
		let bitrate = moq_net::bandwidth::Rate::from_bps(64_000);
		producer.set_bitrate(bitrate).unwrap();
		let after = catalog.snapshot();
		assert_eq!(after.audio.renditions.len(), 1);
		let mut expected = before;
		expected.bitrate = Some(64_000);
		assert_eq!(after.audio.renditions["audio"], expected);
		assert_eq!(producer.track_name(), "audio");
		assert_eq!(producer.bitrate(), bitrate);
		// Encoded-byte detection must not replace the newly configured target.
		for timestamp in [0, 20_000, 40_000] {
			producer.write(&pcm_frame(&vec![0.1; 1920], timestamp)).unwrap();
		}
		assert_eq!(catalog.snapshot().audio.renditions["audio"].bitrate, Some(64_000));
	}

	/// The rendition a producer publishes is the encoder's own catalog, so neither can drift.
	#[tokio::test]
	async fn publishes_the_encoder_catalog() {
		for codec in [crate::encode::Codec::Opus, crate::encode::Codec::Pcm] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let options = Options {
				track: Some("audio".into()),
				config: Config {
					codec,
					..Config::default()
				},
				..Options::default()
			};
			let _producer = Producer::new(&mut broadcast, catalog.clone(), Input::default(), &options).unwrap();
			let published = catalog.snapshot().audio.renditions.remove("audio").unwrap();
			assert_eq!(published, Encoder::new(&options.config).unwrap().catalog(), "{codec:?}");
		}
	}
}

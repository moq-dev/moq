//! Subscribe to an encoded H.264, H.265, or AV1 track and emit decoded frames.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use hang::catalog::VideoConfig;
use kio::clock::Instant;
use moq_mux::container::Read as Encoded;

use super::decoder::Config;
use super::sink::Sink;
use crate::Error;
use crate::Frame;
pub use moq_net::track::Delay;

/// One item of a [`Consumer`] read, in delivery order: a decoded picture, or a break
/// between them.
///
/// The decoded counterpart of [`moq_mux::container::Read`], and the same shape
/// `moq_audio::decode::Read` has, so a player handles loss and ends alike for
/// every media type.
pub enum Read {
	/// A decoded picture.
	Frame(Frame),
	/// Media after what was already read was lost: a group was skipped or never arrived.
	/// The decoder was drained first, so every picture from before the loss precedes it.
	///
	/// The span is an upper bound, in the scale of the media that follows.
	Gap {
		/// The furthest presentation point already read.
		timestamp: moq_net::Timestamp,
		/// How long until the next picture resumes.
		duration: moq_net::Timestamp,
	},
	/// The publisher stopped its encoder: it paused, switched source, or finished.
	///
	/// The decoder was drained first, so the stopped encoder's last pictures precede it
	/// rather than waiting for the next one. What follows comes from a fresh encoder.
	End {
		/// Where the stopped encoder's media stops.
		end: moq_net::Timestamp,
	},
}

/// Where a consumer starts on a track that already holds groups.
///
/// A track keeps its groups for a while after they are read, so a decoder does
/// not always open on an empty one: a player rebuilding its decoder subscribes
/// while its predecessor still holds groups, and a rendition switched away from
/// and back to stays warm on the origin for the track's idle linger (cached
/// groups, not an upstream subscription). What to do with that
/// backlog depends on the consumer, and the two answers are opposites, so it is
/// asked rather than guessed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Start {
	/// The oldest group the track still holds, decoding everything cached.
	///
	/// What a recorder, an export, or anything reading a complete track wants,
	/// and the default because dropping media a caller has not asked to drop is
	/// the worse mistake.
	#[default]
	Oldest,
	/// The newest group, skipping whatever is already cached.
	///
	/// What a live player wants. Without it a rebuilt decoder walks the whole
	/// backlog at decode speed before reaching live media, which a viewer sees
	/// as playback jumping backwards and then sprinting to catch up. Opening
	/// waits for the track's live edge, or its end.
	Latest,
}

/// How a [`Consumer`] subscribes to its track, and the decoder it feeds.
///
/// The subscription half is what a bare [`Decoder`](super::Decoder) has no use
/// for: where to start on a cached track, and how far to fall behind live
/// before skipping. The decoder half is passed through as it is.
///
/// `#[non_exhaustive]`: build via [`Options::new`] (or `default()`) and set the
/// fields, so future knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
	/// The decoder: backend, output representation, scaling hint.
	pub decoder: Config,
	/// Where to start on a track that already holds groups.
	pub start: Start,
	/// How far behind the live edge playback runs. [`Delay::min`] is the
	/// player's playout delay, where a cold join starts; [`Delay::max`] is how
	/// far playback may drift before a stalled group is skipped. Both default to
	/// zero: start at the newest group and skip aggressively. Set `min` to your
	/// playout delay and `max` at or past it for a softer skip. The max is
	/// applied to the transport subscription, and both reach
	/// [`moq_mux::container::Consumer`].
	pub delay: Delay,
}

impl Options {
	/// Defaults: a default [`Config`], every cached group, real-time latency.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Subscribe to a moq-mux video track and emit decoded frames.
///
/// The codec/backend are fixed at construction; [`read`](Self::read) returns
/// [`Read`] events carrying pictures in the representation
/// [`Config::output`](super::Config::output) asked for. The direct mirror of
/// `moq_audio::decode::Consumer`.
pub struct Consumer {
	slot: Slot,
	name: String,
	/// When the access unit behind the latest picture returned arrived.
	arrival: Option<Instant>,
}

/// Access units a [`Reader`] remembers the arrival of while its decoder holds them.
///
/// An entry goes once the decoder returns its picture, or a picture fed after it
/// at a later timestamp, so the queue holds only what the decoder has in flight:
/// one entry without reordering, a few with B-frames. H.264 and H.265 hold at
/// most 16 pictures, and this leaves room for a hardware pipeline beyond that.
/// The cap bounds what a malformed stream strands, such as timestamps that run
/// backwards within a group; the oldest go first.
const ARRIVALS: usize = 32;

/// The decoder, idle between reads or decoding.
///
/// A read in flight is kept across a cancelled [`Consumer::read`] and finished by
/// the next one, since a cancelled [`Sink`] call poisons the sink.
enum Slot {
	Idle(Box<Reader>),
	Busy(Step),
}

/// A [`Reader::read`] in flight, handing the reader back with its result.
type Step = Pin<Box<dyn Future<Output = (Box<Reader>, Result<Option<Item>, Error>)> + Send>>;

/// A read, with when the access unit behind a picture arrived.
type Item = (Read, Option<Instant>);

/// One subscription and the decoder it feeds.
struct Reader {
	/// A [`Sink`] rather than a bare `Decoder`: the read loop below is held
	/// across `.await` by every caller (libmoq's spawned task, moq-transcode),
	/// so the codec would otherwise migrate between executor workers and
	/// unbalance the per-thread COM apartment the Windows backend opens.
	decoder: Sink,
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	/// Pictures a single access unit or drain decoded to, and the boundary behind
	/// them, that `read` hasn't returned yet. One AU yields one picture in the
	/// low-delay path, but a backend may hand back more, so we buffer to keep
	/// `read` one-item-per-call.
	pending: VecDeque<Item>,
	/// The presentation time, in microseconds, and arrival of each access unit
	/// fed to the decoder that no picture has come back for, in decode order.
	/// Keyed by timestamp, because a decoder may return pictures in presentation
	/// or decode order. Microseconds, because a backend may hand a picture back
	/// at another scale than its access unit's.
	arrivals: VecDeque<(u128, Instant)>,
	/// Whether the ended track's decoder has already been drained.
	drained: bool,
	/// The first recovery group lacks the preceding GOP's references.
	recover: bool,
	leading_floor: Option<moq_net::Timestamp>,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, decoding it per the catalog entry.
	/// Errors if the rendition's codec is not supported by a native backend.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: impl Into<String>,
		options: Options,
	) -> Result<Self, Error> {
		let reader = Reader::open(broadcast, catalog, &name.into(), &options).await?;
		Ok(Self {
			name: reader.decoder.name().to_owned(),
			slot: Slot::Idle(Box::new(reader)),
			arrival: None,
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// When the access unit behind the latest picture [`read`](Self::read) returned
	/// landed in this node's cache ([`moq_mux::container::Consumer::arrival`]).
	///
	/// Pair it with when the picture was read to tell network delay from decode
	/// delay: it excludes the time the decoder held or reordered the picture.
	/// A gap or an end leaves it unchanged, as `moq_audio::decode::Consumer::arrival`
	/// does. `None` before the first picture, and for a picture that no access unit,
	/// or more than one, matches by timestamp: one a backend returned at a timestamp
	/// it was never handed, or a stream that repeats a timestamp while the decoder
	/// holds both.
	pub fn arrival(&self) -> Option<Instant> {
		self.arrival
	}

	/// Read the next decoded picture or boundary, or `None` after the track ends and
	/// the decoder's buffered tail has been drained.
	///
	/// Join and delivery-gap recovery trim leading pictures whose previous GOP is absent.
	///
	/// Cancel-safe: a decode in flight when the read is dropped is kept and
	/// finished by the next read. A codec error fails the read.
	pub async fn read(&mut self) -> Result<Option<Read>, Error> {
		std::future::poll_fn(|cx| self.poll_read(cx)).await
	}

	fn poll_read(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Read>, Error>> {
		if matches!(self.slot, Slot::Idle(_)) {
			// A pending placeholder holds the slot only while the reader moves into its step.
			let Slot::Idle(mut reader) =
				std::mem::replace(&mut self.slot, Slot::Busy(Box::pin(std::future::pending())))
			else {
				unreachable!("matched above");
			};
			self.slot = Slot::Busy(Box::pin(async move {
				let result = reader.read().await;
				(reader, result)
			}));
		}
		let Slot::Busy(step) = &mut self.slot else {
			unreachable!("started above");
		};
		let Poll::Ready((reader, result)) = step.as_mut().poll(cx) else {
			return Poll::Pending;
		};
		// Left idle after an error, so the next read asks it again and gets the same answer.
		self.slot = Slot::Idle(reader);
		let Some((read, arrival)) = result? else {
			return Poll::Ready(Ok(None));
		};
		// A gap or an end leaves the arrival unchanged.
		if let Read::Frame(_) = read {
			self.arrival = arrival;
		}
		Poll::Ready(Ok(Some(read)))
	}
}

#[cfg(test)]
impl Consumer {
	/// The next picture, skipping gaps and ends.
	async fn next_frame(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			match self.read().await? {
				Some(Read::Frame(frame)) => return Ok(Some(frame)),
				Some(Read::Gap { .. } | Read::End { .. }) => {}
				None => return Ok(None),
			}
		}
	}

	/// The decoder between reads.
	fn reader(&mut self) -> &mut Reader {
		let Slot::Idle(reader) = &mut self.slot else {
			panic!("the decoder is not idle");
		};
		reader
	}
}

impl Reader {
	/// Subscribe per `options`, starting at the live edge when the caller asked for
	/// [`Start::Latest`].
	async fn open(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: &str,
		options: &Options,
	) -> Result<Self, Error> {
		let decoder = Sink::open(catalog, &options.decoder).await.map_err(|err| match err {
			// Only a codec or description this build cannot decode is a refusal of the
			// rendition. Backend and worker faults keep their own variants.
			Error::UnsupportedCodec(reason) => Error::UnsupportedConfig(reason),
			err => err,
		})?;

		let playout = moq_mux::container::Playout::new(options.delay, std::time::Duration::ZERO);
		let track = broadcast.track(hang::catalog::Reference::track(catalog, name))?;
		// The whole delay from the first request, as the container keeps it, so the publisher
		// never sees a stated zero min before the container's update.
		let mut subscription =
			playout.subscription(moq_net::track::Subscription::default().with_priority(hang::catalog::PRIORITY.video));
		// A decoder often opens on a track that is already cached: a replacement decoder
		// subscribes while its predecessor still holds groups, and a rendition switched away from
		// and back to stays warm on the origin for `moq_net::model::track::IDLE_LINGER` (cached
		// groups, not an upstream subscription). A caller that asked for `Start::Latest` wants none
		// of that backlog, which a cursor starting at the oldest group would replay at decode
		// speed before reaching live media: on a thirty-second retention, half a minute of pictures
		// raced through.
		//
		// A live start counts from the latest group each hop holds from its live feed, never from
		// what an ended feed left cached, so it names no stale group that would ask the publisher
		// to rewind for everyone reading. The group before a live edge is the one containing the
		// key just below it, since starts are sparse. Every wire carries a time start, where a
		// relative one past the edge is refused before lite-07; there it reaches the edge alone.
		if options.start == Start::Latest {
			subscription = subscription.with_ranges([moq_net::track::Range::live()]);
		}
		let mut subscriber = track.subscribe(subscription).await?;
		// The live edge is known only once the feed is live: a warm copy still waiting on its
		// source reports nothing yet. Opening waits for it, or the track's end, as `Start::Latest`
		// promises.
		if options.start == Start::Latest {
			subscriber.largest().await;
		}
		let track = subscriber;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(catalog)?;
		// On a cold network join the live edge's stream often lands first. The container starts
		// at the group covering the live edge less the min delay, waiting for it.
		let track = playout.consumer(track, container);

		let reader = Self {
			decoder,
			track,
			pending: VecDeque::new(),
			arrivals: VecDeque::new(),
			drained: false,
			recover: true,
			leading_floor: None,
		};
		Ok(reader)
	}

	/// The next item this decoder alone produces, with its access unit's arrival
	/// for a picture; see [`Consumer::read`].
	async fn read(&mut self) -> Result<Option<Item>, Error> {
		loop {
			if let Some(item) = self.pending.pop_front() {
				return Ok(Some(item));
			}
			if self.drained {
				return Ok(None);
			}

			let mux_frame = match self.track.read().await? {
				Some(Encoded::Frame { frame, .. }) => Some(frame),
				// Drained so the pictures from before the boundary all come out ahead of it,
				// rather than whenever the media after it pushes them through. The decoder
				// stays usable and restarts on the next group's keyframe.
				// The marker is consumed, so a failed drain still queues it and arms recovery
				// before reporting, or the next read would never see it.
				Some(Encoded::Gap { timestamp, duration }) => {
					let drained = self.drain().await;
					self.pending.push_back((Read::Gap { timestamp, duration }, None));
					self.recover = true;
					drained?;
					continue;
				}
				// A fresh encoder has no preceding GOP to reference, so no recovery.
				Some(Encoded::End { end, .. }) => {
					let drained = self.drain().await;
					self.pending.push_back((Read::End { end }, None));
					drained?;
					continue;
				}
				None => None,
			};

			let Some(mux_frame) = mux_frame else {
				// The flag goes up only once the tail is in hand, so a read
				// dropped before the drain ran retries it rather than reporting
				// an end the stream has not reached. Flushing twice is safe: the
				// second hands back nothing.
				let drained = self.drain().await;
				// Set before the error is returned, not after. A flush that
				// failed once fails the same way every time, and the track has
				// ended either way, so leaving the flag down turns one bad
				// drain into a caller that reads, fails, and reads again with
				// nothing in between to wait on. A caller that treats a codec
				// error as one lost picture and carries on then spins.
				self.drained = true;
				drained?;
				continue;
			};

			if mux_frame.keyframe {
				self.leading_floor = self.recover.then_some(mux_frame.timestamp);
				self.recover = false;
			}
			if self.leading_floor.is_some_and(|floor| mux_frame.timestamp < floor) {
				continue;
			}

			let arrived = self.track.arrival();
			// A frame with no time has no picture to match an arrival to.
			let queued = arrived.zip(mux_frame.timestamp.as_micros().ok());
			if let Some((arrived, micros)) = queued {
				if self.arrivals.len() == ARRIVALS {
					self.arrivals.pop_front();
				}
				self.arrivals.push_back((micros, arrived));
			}
			let decoded = self
				.decoder
				.decode(mux_frame.payload, mux_frame.timestamp, mux_frame.keyframe)
				.await;
			if decoded.is_err() && queued.is_some() {
				// No picture will come back for a refused access unit, and left queued its
				// arrival would be reported for a later one at the same timestamp.
				self.arrivals.pop_back();
			}
			decoded?;
			self.receive()?;
		}
	}

	/// Queue every picture the decoder has ready, each with its access unit's arrival.
	fn receive(&mut self) -> Result<(), Error> {
		while let Some(picture) = self.decoder.receive()? {
			// A frame with no time has no picture to match an arrival to.
			let arrival = picture.timestamp.as_micros().ok().and_then(|at| {
				// A timestamp queued twice is ambiguous: the picture may belong to either access
				// unit, so it reports none rather than a guess.
				let index = {
					let mut matches = self
						.arrivals
						.iter()
						.enumerate()
						.filter(|(_, (timestamp, _))| *timestamp == at);
					matches
						.next()
						.filter(|_| matches.next().is_none())
						.map(|(index, _)| index)?
				};
				let (_, arrived) = self.arrivals.remove(index)?;
				// An access unit fed before this one at an earlier time is a picture the
				// decoder already returned or dropped, whichever order it returns them in.
				let mut position = 0;
				self.arrivals.retain(|(timestamp, _)| {
					position += 1;
					position > index || *timestamp > at
				});
				Some(arrived)
			});
			self.pending.push_back((Read::Frame(picture), arrival));
		}
		Ok(())
	}

	/// Queue every picture the decoder still holds. A flush that fails still
	/// queues what it readied, so a later read hands those pictures over.
	async fn drain(&mut self) -> Result<(), Error> {
		let flushed = self.decoder.flush().await;
		let received = self.receive();
		// Whatever the flush did not return never comes back.
		self.arrivals.clear();
		flushed.and(received)
	}
}

#[cfg(test)]
mod tests {
	#![cfg_attr(
		not(feature = "openh264"),
		allow(dead_code, unused_imports, reason = "only the `openh264` feature uses it")
	)]

	use bytes::Bytes;
	use moq_net::Timestamp;

	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}
	use super::*;
	use crate::decode::Kind;
	use crate::decode::backend::probe;
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind, Publisher as EncodePublisher};
	use moq_mux::container::Container as _;

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn reads_cmaf_container_declared_by_catalog() {
		let mut source_broadcast = moq_net::broadcast::Info::new().produce();
		let source_subscriber = source_broadcast.consume();
		let source_catalog =
			moq_mux::catalog::Producer::new(&mut source_broadcast, moq_mux::catalog::Config::default()).unwrap();
		let config = EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, crate::FrameRate::new(30, 1).unwrap())
		};
		let rendition = config.probe().await.unwrap();
		let reserved = source_catalog.reserve().with_interval(config.effective_join());
		let track = source_broadcast.unique_track(".avc3", reserved.track_info(hang::catalog::PRIORITY.video));
		let mut producer = EncodePublisher::new(track, reserved, rendition).unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		for index in 0..2 {
			encoder.cut().unwrap();
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			for encoded in encoder.encode_all(&frame).unwrap() {
				producer.publish(&encoded).unwrap();
			}
		}

		let origin = produce_origin();
		let catalog = moq_mux::catalog::Consumer::<()>::new(&source_subscriber, moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let source = moq_mux::Source::new(origin.consume(), source_subscriber.clone());
		// Both frames are encoded before the export runs, so the exporter needs a budget
		// wide enough to read them: its REAL_TIME default keeps only the live edge, and
		// the second `next()` would then block forever waiting for a group that was
		// skipped.
		let mut export = moq_mux::container::fmp4::Export::new(source, catalog)
			.with_delay(moq_net::track::Delay::default().with_max(std::time::Duration::from_secs(30)));
		let init = export.next().await.unwrap().expect("CMAF init");
		let fragment = export.next().await.unwrap().expect("CMAF fragment");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve()).with_cmaf(true);
		import.decode(&init).unwrap();
		import.decode(&fragment).unwrap();

		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.video.renditions.iter().next().expect("video rendition");
		assert!(matches!(config.container, hang::catalog::Container::Cmaf { .. }));
		let mut consumer = Consumer::new(
			&subscriber,
			config,
			name,
			Options {
				decoder: Config {
					kind: Kind::Software,
					..Config::new()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let frame = consumer.next_frame().await.unwrap().expect("decoded frame");
		assert_eq!(frame.size(), crate::Size::new(320, 240));
	}

	/// A decoder opened on a track that already holds groups starts at the
	/// newest one, not at the oldest still cached.
	///
	/// A player rebuilding its decoder (a backend change, a rendition pin) opens
	/// a second consumer while the first still holds the groups it has not
	/// released. Starting those at sequence zero replays the whole retention at
	/// decode speed before the picture reaches live media.
	#[tokio::test]
	async fn a_second_consumer_starts_at_the_live_edge() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// Kept so the aggregated subscription can be read back below.
		let published = track.clone();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		// A keyframe opens a group, so this is three groups a second apart.
		for index in 0..3u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 1_000_000).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: true,
				})
				.unwrap();
		}
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::BUFFERED_NAME.into()),
					..Config::new()
				},
				start: Start::Latest,
				// Wide enough to keep every group fresh: the age budget on its own
				// delivers only the live edge, which would pass this test without
				// `Start::Latest` doing anything.
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		// The buffered probe stamps each picture with the access unit's own
		// timestamp, so this says which group the read started from. It is the
		// backend to use here rather than the plain probe, whose event log is
		// process-wide and belongs to the thread-affinity test.
		let frame = consumer.next_frame().await.unwrap().expect("a decoded frame");
		assert_eq!(
			frame.timestamp,
			Timestamp::from_micros(2_000_000).unwrap(),
			"a fresh consumer replayed the groups an earlier reader still holds",
		);

		// The skip names no cached group. The ranges are aggregated across every live
		// subscriber and tell the publisher what to send, so a start at a cached group would
		// rewind the track for everyone reading it. A rendition switched away from and back to
		// is the case that bites, because its cached groups are stale by then and the publisher
		// resends the broadcast from them. The live start counts from each hop's live feed.
		assert_eq!(
			published.subscription().map(|sub| sub.ranges),
			Some(vec![moq_net::track::Range::live()]),
			"the publisher was asked to rewind the track",
		);
	}

	/// The default reads everything the track holds.
	///
	/// `Start::Latest` is a player's policy and not the API's: a recorder, an
	/// export, or a test decoding a track that was written before it subscribed
	/// wants every group, and dropping media nobody asked to drop is the worse
	/// of the two mistakes. This is the half that a live-edge default breaks,
	/// so it is pinned beside the other one.
	#[tokio::test]
	async fn the_default_reads_every_cached_group() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for index in 0..3u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 1_000_000).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: true,
				})
				.unwrap();
		}
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					// The buffered probe rather than the plain one: the plain probe's
					// event log is process-wide and belongs to the thread-affinity test.
					kind: Kind::Named(probe::BUFFERED_NAME.into()),
					..Config::new()
				},
				// A budget that keeps every group fresh, so the start policy is the
				// only thing deciding what is read.
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let mut seen = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			seen.push(frame.timestamp);
		}
		assert_eq!(
			seen,
			vec![
				Timestamp::from_micros(0).unwrap(),
				Timestamp::from_micros(1_000_000).unwrap(),
				Timestamp::from_micros(2_000_000).unwrap(),
			],
			"the default dropped groups the caller never asked to drop",
		);
	}

	/// The age budget is the subscription's and not the decoder's: it reaches
	/// the publisher through the track subscription, while the decoder opens
	/// with exactly the config it was handed.
	///
	/// Driven by `pollster` rather than tokio: the probe's guard is a plain
	/// mutex, and holding one across an `.await` is what clippy rightly flags.
	#[test]
	fn max_delay_reaches_the_subscription_and_not_the_decoder() {
		let _probe = probe::native_exclusive();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let published = track.clone();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::from_micros(0).unwrap(),
				duration: None,
				payload: Bytes::from_static(b"access unit"),
				keyframe: true,
			})
			.unwrap();
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let decoder = Config {
			kind: Kind::Named(probe::NATIVE_NAME.into()),
			output: crate::Output::Cpu,
			scale_hint: Some(crate::Size::new(160, 120)),
			..Default::default()
		};
		let max_delay = std::time::Duration::from_secs(10);
		let mut consumer = pollster::block_on(Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: decoder.clone(),
				delay: Delay {
					max: Some(max_delay),
					..Delay::default()
				},
				..Options::new()
			},
		))
		.unwrap();

		let subscription = published.subscription().expect("the consumer subscribed");
		assert_eq!(
			subscription.delay.max,
			Some(max_delay),
			"the age budget did not reach the publisher"
		);

		let opened = probe::native_opened().expect("the decoder opened");
		assert_eq!(opened.output, decoder.output);
		assert_eq!(opened.scale_hint, decoder.scale_hint);

		let frame = pollster::block_on(consumer.next_frame())
			.unwrap()
			.expect("a decoded frame");
		assert!(
			matches!(frame.surface, crate::Surface::I420(_)),
			"CPU output was not enforced"
		);
	}

	#[tokio::test]
	async fn leading_pictures_require_a_continuous_previous_group() {
		tokio::time::pause();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::BUFFERED_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();
		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let write = |sequence, stamps: &[u64]| {
			let mut group = track
				.create_group(moq_net::group::Info { start: sequence })
				.unwrap()
				.unwrap();
			let frames: Vec<_> = stamps
				.iter()
				.enumerate()
				.map(|(index, stamp)| moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(*stamp).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				})
				.collect();
			wire.write(&mut group, &frames).unwrap();
			group.finish().unwrap();
		};
		write(10, &[1000, 800, 900, 1100]);
		write(11, &[2000, 1800, 1900, 2100]);
		let mut got = Vec::new();
		for _ in 0..5 {
			got.push(
				consumer
					.next_frame()
					.await
					.unwrap()
					.unwrap()
					.timestamp
					.as_micros()
					.unwrap(),
			);
		}
		assert_eq!(
			got,
			[1000, 1100, 2000, 1800, 1900],
			"join trims, continuous delivery keeps every leading picture"
		);
		consumer.reader().track.set_delay(Delay::default());
		write(12, &[3000]);
		write(13, &[4000, 3800, 3900, 4100]);
		let _track = track.finish().unwrap();
		got.clear();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			got.push(frame.timestamp.as_micros().unwrap());
		}
		assert_eq!(
			got,
			[2100, 4000, 4100],
			"a later latency skip must trim its recovery group too"
		);
	}

	/// A track ends before a decoder that reorders pictures does. The consumer
	/// drains the backend once and returns its tail before reporting the end,
	/// each picture with the arrival of its own access unit rather than of the
	/// access unit that pushed it out.
	#[tokio::test(start_paused = true)]
	async fn track_end_drains_buffered_decoder() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		let mut written = Vec::new();
		for index in 0..2u64 {
			written.push(Instant::now());
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 33_333).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				})
				.unwrap();
			tokio::time::advance(std::time::Duration::from_millis(10)).await;
		}
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::BUFFERED_NAME.into()),
					..Config::new()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		assert_eq!(consumer.arrival(), None, "nothing read yet");
		let mut timestamps = Vec::new();
		let mut arrivals = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros().unwrap());
			arrivals.push(consumer.arrival());
		}
		assert_eq!(timestamps, vec![0, 33_333]);
		assert_eq!(arrivals, written.into_iter().map(Some).collect::<Vec<_>>());
		assert!(
			consumer.next_frame().await.unwrap().is_none(),
			"the decoder was drained twice"
		);
	}

	/// A decoder that reorders returns pictures in presentation order, so each one's
	/// arrival is looked up by its timestamp rather than taken in decode order.
	#[tokio::test(start_paused = true)]
	async fn arrival_follows_a_reordered_picture() {
		let (shown, landed) = decode_bframes(probe::REORDER_NAME).await;
		let expected: Vec<_> = [0, 1, 2, 3].map(|index| (index, Some(landed[&index]))).into();
		assert_eq!(shown, expected);
	}

	/// A pipelined decoder that returns pictures in decode order, so a B-frame's access
	/// unit is fed before the P-frame ahead of it comes out, still maps each picture
	/// to its own access unit.
	#[tokio::test(start_paused = true)]
	async fn arrival_follows_a_picture_in_decode_order() {
		let (shown, landed) = decode_bframes(probe::BUFFERED_NAME).await;
		let expected: Vec<_> = [0, 3, 1, 2].map(|index| (index, Some(landed[&index]))).into();
		assert_eq!(shown, expected);
	}

	/// Decode I0 P3 B1 B2 with the decoder named `decoder`, each access unit landing
	/// 10 ms after the last. Returns each picture shown, by frame index, with its
	/// reported arrival, and when each access unit landed.
	async fn decode_bframes(decoder: &str) -> (Vec<(u64, Option<Instant>)>, std::collections::BTreeMap<u64, Instant>) {
		let (broadcast, track) = live_track();
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog(),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(decoder.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap().unwrap();
		let mut landed = std::collections::BTreeMap::new();
		for (index, frame) in [0u64, 3, 1, 2].into_iter().enumerate() {
			landed.insert(frame, Instant::now());
			wire.write(
				&mut group,
				&[moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(frame * 33_333).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				}],
			)
			.unwrap();
			tokio::time::advance(std::time::Duration::from_millis(10)).await;
		}
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let mut shown = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			let index = u64::try_from(frame.timestamp.as_micros().unwrap()).unwrap() / 33_333;
			shown.push((index, consumer.arrival()));
		}
		(shown, landed)
	}

	/// A decoder holding more access units than the queue keeps drops the oldest
	/// arrivals, so their pictures report none rather than a guess.
	#[tokio::test(start_paused = true)]
	async fn arrivals_are_bounded() {
		const FRAMES: u64 = ARRIVALS as u64 + 8;
		let (broadcast, track) = live_track();
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog(),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::HOLDING_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap().unwrap();
		let frames: Vec<_> = (0..FRAMES)
			.map(|index| moq_mux::container::Frame {
				timestamp: Timestamp::from_micros(index * 33_333).unwrap(),
				duration: None,
				payload: Bytes::from_static(b"access unit"),
				keyframe: index == 0,
			})
			.collect();
		wire.write(&mut group, &frames).unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let mut sourced = Vec::new();
		while consumer.next_frame().await.unwrap().is_some() {
			assert!(consumer.reader().arrivals.len() <= ARRIVALS);
			sourced.push(consumer.arrival().is_some());
		}
		let mut expected = vec![false; FRAMES as usize - ARRIVALS];
		expected.extend(vec![true; ARRIVALS]);
		assert_eq!(sourced, expected);
	}

	/// A declared end drains the decoder, so the stopped encoder's last picture
	/// comes out ahead of the end while the publisher is still paused, rather
	/// than when the fresh encoder's first picture pushes it through.
	#[tokio::test]
	async fn an_end_drains_the_pictures_before_it() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let picture = |micros: u64, payload: &'static [u8]| moq_mux::container::Frame {
			timestamp: Timestamp::from_micros(micros).unwrap(),
			duration: Some(Timestamp::from_micros(33_333).unwrap()),
			payload: Bytes::from_static(payload),
			keyframe: true,
		};
		producer.write(picture(100_000, b"old access unit")).unwrap();
		producer.cut(None).unwrap();
		producer.discontinuity().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::BUFFERED_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let mut reads = Vec::new();
		let mut read = async |consumer: &mut Consumer| {
			reads.push(match consumer.read().await.unwrap() {
				Some(Read::Frame(frame)) => format!("frame {}", frame.timestamp.as_micros().unwrap()),
				Some(Read::Gap { timestamp, duration }) => {
					format!(
						"gap {}+{}",
						timestamp.as_micros().unwrap(),
						duration.as_micros().unwrap()
					)
				}
				Some(Read::End { end }) => format!("end {}", end.as_micros().unwrap()),
				None => "none".to_string(),
			});
		};
		// Nothing follows the end yet.
		read(&mut consumer).await;
		read(&mut consumer).await;

		producer.write(picture(200_000, b"new access unit")).unwrap();
		let _finished = producer.finish().unwrap();
		read(&mut consumer).await;
		read(&mut consumer).await;
		// The discontinuity marker starts a microsecond before the content's end, so content
		// resuming there still starts a later group.
		assert_eq!(reads, ["frame 100000", "end 133332", "frame 200000", "none"]);
	}

	/// Every read until the track is done, as text: a codec error is one read too.
	async fn read_all(consumer: &mut Consumer) -> Vec<String> {
		let mut reads = Vec::new();
		loop {
			reads.push(match consumer.read().await {
				Ok(Some(Read::Frame(frame))) => format!("frame {}", frame.timestamp.as_micros().unwrap()),
				Ok(Some(Read::Gap { timestamp, .. })) => format!("gap {}", timestamp.as_micros().unwrap()),
				Ok(Some(Read::End { end })) => format!("end {}", end.as_micros().unwrap()),
				Ok(None) => return reads,
				Err(_) => "error".to_string(),
			});
		}
	}

	/// A drain that fails at an end still delivers the pictures it readied ahead
	/// of the end, and the end itself, after reporting the error.
	#[tokio::test]
	async fn a_failed_drain_keeps_its_end() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let picture = |micros: u64| moq_mux::container::Frame {
			timestamp: Timestamp::from_micros(micros).unwrap(),
			duration: Some(Timestamp::from_micros(33_333).unwrap()),
			payload: Bytes::from_static(b"access unit"),
			keyframe: true,
		};
		producer.write(picture(100_000)).unwrap();
		producer.cut(None).unwrap();
		producer.discontinuity().unwrap();
		producer.write(picture(200_000)).unwrap();
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::FAILING_FLUSH_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		assert_eq!(
			read_all(&mut consumer).await,
			["error", "frame 100000", "end 133332", "error", "frame 200000"]
		);
	}

	/// A drain that fails at a gap still delivers the gap after the pictures it
	/// readied, and still trims the leading pictures of the recovery group.
	#[tokio::test(start_paused = true)]
	async fn a_failed_drain_keeps_its_gap_and_recovery() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::FAILING_FLUSH_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();
		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let write = |sequence, stamps: &[u64]| {
			let mut group = track
				.create_group(moq_net::group::Info { start: sequence })
				.unwrap()
				.unwrap();
			let frames: Vec<_> = stamps
				.iter()
				.enumerate()
				.map(|(index, stamp)| moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(*stamp).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				})
				.collect();
			wire.write(&mut group, &frames).unwrap();
			group.finish().unwrap();
		};
		write(10, &[1000, 1100]);
		let first = consumer.next_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp.as_micros().unwrap(), 1000);

		consumer.reader().track.set_delay(Delay::default());
		write(11, &[2000]);
		write(12, &[3000, 2800, 2900, 3100]);
		let _track = track.finish().unwrap();
		let reads = read_all(&mut consumer).await;
		assert_eq!(
			reads[..3],
			["error", "frame 1100", "gap 1100"],
			"the drained picture, then its gap: {reads:?}"
		);
		assert_eq!(
			reads[3..],
			["frame 3000", "error", "frame 3100"],
			"recovery trims the leading pictures: {reads:?}"
		);
	}

	/// A read dropped while a threaded flush is in flight loses nothing: the
	/// next read finishes that same flush rather than issuing another into a
	/// poisoned sink, and only then reports the end.
	#[cfg(not(apple))]
	#[tokio::test]
	async fn a_cancelled_read_resumes_the_flush_in_flight() {
		probe::prepare_blocking_flush();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		let _finished = producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::BLOCKING_FLUSH_NAME.into()),
					..Config::new()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let mut read = Box::pin(consumer.next_frame());
		let entered = async {
			tokio::select! {
				_result = &mut read => panic!("flush returned before cancellation"),
				_ = probe::flush_entered() => {}
			}
		};
		if kio::clock::timeout(std::time::Duration::from_secs(1), entered)
			.await
			.is_err()
		{
			probe::release_flush();
			panic!("flush never reached the codec thread");
		}
		drop(read);
		probe::release_flush();

		assert!(
			consumer.next_frame().await.unwrap().is_none(),
			"the cancelled read's flush was not resumed"
		);
		assert!(consumer.next_frame().await.unwrap().is_none());
	}

	/// VAAPI returns its buffered tail before the consumer reports track end.
	#[cfg(all(target_os = "linux", feature = "vaapi", feature = "openh264"))]
	#[tokio::test]
	async fn the_track_ending_drains_the_decoder() {
		const FRAMES: u64 = 5;
		let config = EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, crate::FrameRate::new(30, 1).unwrap())
		};
		let catalog = config.probe().await.expect("probe the software encoder");

		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);

		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		// When each access unit was written, bracketing when it landed.
		let mut written = Vec::new();
		for index in 0..FRAMES {
			if index == 0 {
				encoder.cut().unwrap();
			}
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			for encoded in encoder.encode_all(&frame).unwrap() {
				let before = Instant::now();
				producer
					.write(moq_mux::container::Frame {
						timestamp: encoded.timestamp,
						duration: None,
						payload: encoded.payload,
						keyframe: index == 0,
					})
					.unwrap();
				written.push((before, Instant::now()));
			}
		}
		let _finished = producer.finish().unwrap();

		let decode = Options {
			decoder: Config {
				kind: Kind::Named("vaapi".into()),
				..Config::new()
			},
			..Options::new()
		};
		// The hardware gate: no libva, no render node, or no H.264 decode
		// entrypoint and the named backend refuses to open.
		let Ok(mut consumer) = Consumer::new(&subscriber, &catalog, "video", decode).await else {
			return;
		};

		let mut timestamps = Vec::new();
		let mut arrivals = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros().unwrap());
			arrivals.push(consumer.arrival().expect("the picture kept its arrival"));
		}
		let expected: Vec<u128> = (0..FRAMES as u128).map(|index| index * 33_333).collect();
		assert_eq!(timestamps, expected, "the track ended before the stream did");
		for (index, (arrived, (before, after))) in arrivals.iter().zip(&written).enumerate() {
			assert!(
				before <= arrived && arrived <= after,
				"picture {index} reports another access unit's arrival"
			);
		}

		// The end stays the end: the drain runs once, so a caller that keeps
		// reading past it does not get the tail a second time.
		assert!(consumer.next_frame().await.unwrap().is_none());
	}

	/// One frame per second.
	const SECOND: u64 = 1_000_000;

	fn catalog() -> VideoConfig {
		VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		})
	}

	fn live_track() -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		(broadcast, track)
	}

	/// The first request already states the min delay: a latest start waits for the live edge
	/// with its subscription open, before the container could update it.
	#[tokio::test]
	async fn the_first_subscription_states_the_min_delay() {
		let (broadcast, mut track) = live_track();
		let delay = Delay {
			min: Some(std::time::Duration::from_millis(300)),
			max: Some(std::time::Duration::from_millis(500)),
		};
		let consume = broadcast.consume();
		let catalog = catalog();
		let mut open = Box::pin(Consumer::new(
			&consume,
			&catalog,
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::IMMEDIATE_NAME.into()),
					..Config::new()
				},
				start: Start::Latest,
				delay,
			},
		));
		// The open waits for the live edge with its subscription open, so the first aggregate
		// the publisher sees is the request itself.
		let first = tokio::select! {
			opened = &mut open => panic!("the open finished with no live edge (ok: {})", opened.is_ok()),
			first = track.subscription_changed() => first.unwrap(),
		};
		assert_eq!(first.map(|sub| sub.delay), Some(delay));
	}

	/// A device that cannot decode the stream refuses the rendition, so a caller
	/// can pick another one, the same as for a codec this build lacks.
	#[tokio::test]
	async fn a_device_refusal_is_an_unsupported_config() {
		let (broadcast, _track) = live_track();
		let result = Consumer::new(
			&broadcast.consume(),
			&catalog(),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::REFUSING_NAME.into()),
					..Config::new()
				},
				..Options::new()
			},
		)
		.await;
		match result {
			Err(Error::UnsupportedConfig(reason)) => assert!(reason.contains("cannot decode"), "{reason}"),
			Err(other) => panic!("expected UnsupportedConfig, got {other:?}"),
			Ok(_) => panic!("the consumer opened a decoder its device refused"),
		}
	}

	/// A decoder that refuses an access unit never returns its picture, so a later
	/// access unit at the same timestamp reports its own arrival, not the refused one's.
	#[tokio::test(start_paused = true)]
	async fn a_refused_access_unit_does_not_lend_its_arrival() {
		let (broadcast, track) = live_track();
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog(),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::IMMEDIATE_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		// A group's first frame is its keyframe, and a decoder that refused one drops the
		// deltas behind it, so each access unit opens a group of its own.
		let mut landed = Vec::new();
		for (sequence, payload) in [probe::CORRUPT, b"access unit".as_slice()].into_iter().enumerate() {
			landed.push(Instant::now());
			let mut group = track
				.create_group(moq_net::group::Info { start: sequence as u64 })
				.unwrap()
				.unwrap();
			wire.write(
				&mut group,
				&[moq_mux::container::Frame {
					// Group starts strictly increase, so each access unit presents a second later.
					timestamp: Timestamp::from_micros(sequence as u64 * SECOND).unwrap(),
					duration: None,
					payload: Bytes::from_static(payload),
					keyframe: true,
				}],
			)
			.unwrap();
			group.finish().unwrap();
			tokio::time::advance(std::time::Duration::from_millis(10)).await;
		}
		let _track = track.finish().unwrap();

		let Err(err) = consumer.next_frame().await else {
			panic!("the refused access unit did not fail the read");
		};
		assert!(err.to_string().contains("corrupt"), "unexpected error: {err}");
		consumer
			.next_frame()
			.await
			.unwrap()
			.expect("the next access unit decodes");
		assert_eq!(consumer.arrival(), Some(landed[1]));
		assert_eq!(consumer.reader().arrivals.len(), 0, "nothing stays queued");
	}

	/// A timestamp two access units share, both still held by the decoder, matches either
	/// of them, so the picture reports none rather than a guess.
	#[tokio::test(start_paused = true)]
	async fn a_repeated_timestamp_reports_no_arrival() {
		let (broadcast, track) = live_track();
		let mut consumer = Consumer::new(
			&broadcast.consume(),
			&catalog(),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::HOLDING_NAME.into()),
					..Config::new()
				},
				delay: Delay {
					max: Some(std::time::Duration::from_secs(10)),
					..Delay::default()
				},
				..Options::new()
			},
		)
		.await
		.unwrap();

		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap().unwrap();
		for keyframe in [true, false] {
			wire.write(
				&mut group,
				&[moq_mux::container::Frame {
					timestamp: Timestamp::ZERO,
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe,
				}],
			)
			.unwrap();
			tokio::time::advance(std::time::Duration::from_millis(10)).await;
		}
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let mut shown = 0;
		while consumer.next_frame().await.unwrap().is_some() {
			assert_eq!(consumer.arrival(), None, "picture {shown} guessed an arrival");
			shown += 1;
		}
		assert!(shown > 0, "the decoder returned no picture");
	}
}

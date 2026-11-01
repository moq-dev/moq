//! Subscribe to an encoded H.264, H.265, or AV1 track and emit decoded frames.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use hang::catalog::VideoConfig;
use moq_clock::Instant;
use moq_mux::container::Read as Encoded;

use super::decoder::Config;
use super::select::{Lane, Next, Selector};
use super::sink::Sink;
use crate::Error;
use crate::Frame;

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
	/// as playback jumping backwards and then sprinting to catch up. A
	/// rendition with a catalog `warmup` also decodes the group before it, for
	/// the max-delay decoder (see [`Consumer`]). Opening waits for the track's
	/// live edge, or its end.
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
	/// How far playback may drift from the live edge before a stalled group is
	/// skipped. Defaults to [`std::time::Duration::ZERO`](std::time::Duration::ZERO)
	/// (skip aggressively); set it to your playout buffer for a softer skip.
	/// Applied to the transport subscription and inherited by
	/// [`moq_mux::container::Consumer`]. A rendition with a catalog `warmup`
	/// runs a second decoder at this plus the warmup (see [`Consumer`]).
	pub max_delay: std::time::Duration,
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
///
/// A rendition with a catalog `warmup` (intra refresh, so no keyframes) runs
/// two decoders. One skips at [`Options::max_delay`] and may show refresh
/// stripes after a join or a skip. The other tolerates `warmup` more and joins
/// a group earlier, so its pictures stay clean, and they replace the first
/// decoder's once it catches up. Both subscriptions share one upstream request.
pub struct Consumer {
	/// The min-delay and max-delay decoders, indexed by [`Lane`].
	lanes: [Slot; 2],
	name: String,
	selector: Selector,
}

/// A decoder that is idle, decoding, or gone.
///
/// A decode in flight is kept across [`Consumer::read`] calls rather than
/// cancelled when the other lane wins, because a cancelled [`Sink`] call
/// poisons the sink.
enum Slot {
	Idle(Box<Reader>),
	Busy(Step),
	Closed,
}

/// A [`Reader::read`] in flight, handing the reader back with its result.
type Step = Pin<Box<dyn Future<Output = (Box<Reader>, Result<Option<Read>, Error>)> + Send>>;

/// One subscription and the decoder it feeds.
struct Reader {
	/// A [`Sink`] rather than a bare `Decoder`: the read loop below is held
	/// across `.await` by every caller (moq-c's spawned task, moq-transcode),
	/// so the codec would otherwise migrate between executor workers and
	/// unbalance the per-thread COM apartment the Windows backend opens.
	decoder: Sink,
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	/// Pictures a single access unit or drain decoded to, and the boundary behind
	/// them, that `read` hasn't returned yet. One AU yields one picture in the
	/// low-delay path, but a backend may hand back more, so we buffer to keep
	/// `read` one-item-per-call.
	pending: VecDeque<Read>,
	/// Whether the ended track's decoder has already been drained.
	drained: bool,
	/// The first recovery group lacks the preceding GOP's references.
	recover: bool,
	leading_floor: Option<moq_net::Timestamp>,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, decoding it per the catalog entry.
	/// Errors if the rendition's codec is not supported by a native backend, or
	/// if a `warmup` rendition's second decoder cannot open.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: impl Into<String>,
		options: Options,
	) -> Result<Self, Error> {
		let name = name.into();
		let min = Reader::open(broadcast, catalog, &name, &options, options.max_delay, 0).await?;

		let warmup = catalog.warmup.unwrap_or_default();
		let max = if warmup.is_zero() {
			None
		} else {
			// One more sweep of budget, joining a group earlier, so this decoder
			// starts at a recovery point a whole sweep before the live edge.
			let budget = options.max_delay.saturating_add(warmup);
			Some(Reader::open(broadcast, catalog, &name, &options, budget, 1).await?)
		};

		// The catalog's rate when it has one, else whatever the pictures show.
		let interval = catalog
			.framerate
			.filter(|rate| rate.is_finite() && *rate > 0.0)
			.and_then(|rate| std::time::Duration::try_from_secs_f64(rate.recip()).ok());

		Ok(Self {
			name: min.decoder.name().to_owned(),
			selector: Selector::new(max.is_some(), interval),
			lanes: [
				Slot::Idle(Box::new(min)),
				max.map_or(Slot::Closed, |max| Slot::Idle(Box::new(max))),
			],
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Read the next decoded picture or boundary, or `None` after the track ends and
	/// the decoder's buffered tail has been drained.
	///
	/// Join and delivery-gap recovery trim leading pictures whose previous GOP is absent.
	/// Returned picture timestamps strictly increase.
	///
	/// Cancel-safe: a decode in flight when the read is dropped is kept and
	/// finished by the next read. A codec error from either decoder fails the
	/// read.
	pub async fn read(&mut self) -> Result<Option<Read>, Error> {
		let mut sleep = None;
		std::future::poll_fn(|cx| self.poll_read(cx, &mut sleep)).await
	}

	fn poll_read(
		&mut self,
		cx: &mut Context<'_>,
		sleep: &mut Option<Pin<Box<moq_clock::Sleep>>>,
	) -> Poll<Result<Option<Read>, Error>> {
		loop {
			let deadline = match self.selector.next(Instant::now()) {
				Next::Read(read) => return Poll::Ready(Ok(Some(read))),
				Next::Done => return Poll::Ready(Ok(None)),
				Next::Wait(deadline) => deadline,
			};

			let mut progress = false;
			for lane in [Lane::Min, Lane::Max] {
				let slot = &mut self.lanes[lane as usize];
				if matches!(slot, Slot::Idle(_)) && self.selector.wants(lane) {
					let Slot::Idle(mut reader) = std::mem::replace(slot, Slot::Closed) else {
						unreachable!("matched above");
					};
					*slot = Slot::Busy(Box::pin(async move {
						let result = reader.read().await;
						(reader, result)
					}));
				}
				let Slot::Busy(step) = slot else {
					continue;
				};
				let Poll::Ready((reader, result)) = step.as_mut().poll(cx) else {
					continue;
				};
				progress = true;
				*slot = Slot::Idle(reader);
				match result {
					Ok(Some(read)) => self.selector.push(lane, read, Instant::now()),
					Ok(None) => {
						*slot = Slot::Closed;
						self.selector.finish(lane);
					}
					// Either decoder failing fails the read. Left idle, so the next
					// read asks it again and gets the same answer, like a lone decoder.
					Err(err) => return Poll::Ready(Err(err)),
				}
			}
			if progress {
				continue;
			}

			let Some(deadline) = deadline else {
				return Poll::Pending;
			};
			let sleep = sleep.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(deadline)));
			sleep.as_mut().reset(deadline);
			ready!(sleep.as_mut().poll(cx));
		}
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

	/// The lane's decoder between reads; a lane decoding in the background is busy.
	fn reader(&mut self, lane: Lane) -> &mut Reader {
		let Slot::Idle(reader) = &mut self.lanes[lane as usize] else {
			panic!("the {lane:?} decoder is not idle");
		};
		reader
	}
}

impl Reader {
	/// Subscribe with `max_delay`, starting `lookback` groups before the live
	/// edge when the caller asked for [`Start::Latest`].
	async fn open(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: &str,
		options: &Options,
		max_delay: std::time::Duration,
		lookback: u64,
	) -> Result<Self, Error> {
		let decoder = Sink::open(catalog, &options.decoder).await?;

		let track = broadcast.track(hang::catalog::Reference::track(catalog, name))?;
		let mut subscriber = track
			.subscribe(
				moq_net::track::Subscription::default()
					.with_priority(hang::catalog::PRIORITY.video)
					.with_max_delay(max_delay),
			)
			.await?;
		// A decoder often opens on a track that is already cached: a replacement
		// decoder subscribes while its predecessor still holds groups, and a
		// rendition switched away from and back to stays warm on the origin for
		// `moq_net::model::track::IDLE_LINGER` (cached groups, not an upstream subscription). A
		// caller that asked for `Start::Latest` wants none of that backlog,
		// because a cursor starting at sequence zero replays every cached group
		// at decode speed before reaching live media, which on a thirty-second
		// retention is half a minute of pictures raced through.
		//
		// This moves the local read cursor and deliberately not
		// `Subscription::group_start`. That field is a request to the publisher,
		// aggregated across every live subscriber, so naming a stale cached
		// sequence there asks the publisher to rewind the track for everyone
		// reading it. What a player wants is to skip what it already has.
		//
		// The live edge is known only once the feed is live: a warm copy still waiting on
		// its source reports nothing yet, so wait for it rather than read the backlog. The
		// wait ends with the track.
		if options.start == Start::Latest
			&& let Some(live_edge) = subscriber.largest().await
		{
			subscriber = subscriber.with_range(live_edge.group.saturating_sub(lookback)..);
		}
		let track = subscriber;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(catalog)?;
		let track = moq_mux::container::Consumer::new(track, container);

		Ok(Self {
			decoder,
			track,
			pending: VecDeque::new(),
			drained: false,
			recover: true,
			leading_floor: None,
		})
	}

	/// The next item this decoder alone produces; see [`Consumer::read`].
	async fn read(&mut self) -> Result<Option<Read>, Error> {
		loop {
			if let Some(read) = self.pending.pop_front() {
				return Ok(Some(read));
			}
			if self.drained {
				return Ok(None);
			}

			let mux_frame = match self.track.read().await? {
				Some(Encoded::Frame { frame, .. }) => Some(frame),
				// Drained so the pictures from before the boundary all come out ahead of it,
				// rather than whenever the media after it pushes them through. The decoder
				// stays usable and restarts on the next group's keyframe.
				Some(Encoded::Gap { timestamp, duration }) => {
					self.drain().await?;
					self.pending.push_back(Read::Gap { timestamp, duration });
					self.recover = true;
					continue;
				}
				// A fresh encoder has no preceding GOP to reference, so no recovery.
				Some(Encoded::End { end, .. }) => {
					self.drain().await?;
					self.pending.push_back(Read::End { end });
					continue;
				}
				None => None,
			};

			let Some(mux_frame) = mux_frame else {
				// The flag goes up only once the tail is in hand, so a read
				// dropped before the drain ran retries it rather than reporting
				// an end the stream has not reached. Flushing twice is safe: the
				// second hands back nothing.
				let tail = self.decoder.flush().await;
				// Set before the error is returned, not after. A flush that
				// failed once fails the same way every time, and the track has
				// ended either way, so leaving the flag down turns one bad
				// drain into a caller that reads, fails, and reads again with
				// nothing in between to wait on. A caller that treats a codec
				// error as one lost picture and carries on then spins.
				self.drained = true;
				self.pending.extend(tail?.into_iter().map(Read::Frame));
				continue;
			};

			if mux_frame.keyframe {
				self.leading_floor = self.recover.then_some(mux_frame.timestamp);
				self.recover = false;
			}
			if self.leading_floor.is_some_and(|floor| mux_frame.timestamp < floor) {
				continue;
			}

			let pictures = self
				.decoder
				.decode(mux_frame.payload, mux_frame.timestamp, mux_frame.keyframe)
				.await?;
			self.pending.extend(pictures.into_iter().map(Read::Frame));
		}
	}

	/// Queue every picture the decoder still holds.
	async fn drain(&mut self) -> Result<(), Error> {
		let pictures = self.decoder.flush().await?;
		self.pending.extend(pictures.into_iter().map(Read::Frame));
		Ok(())
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
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind, Producer as EncodeProducer};
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
		let reserved = source_catalog
			.reserve()
			.with_hls_hint(config.gop.duration(config.framerate));
		let mut producer = EncodeProducer::new(source_broadcast, reserved, rendition).unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		for index in 0..2 {
			encoder.cut().unwrap();
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
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
		let mut export =
			moq_mux::container::fmp4::Export::new(source, catalog).with_max_delay(std::time::Duration::from_secs(30));
		let init = export.next().await.unwrap().expect("CMAF init");
		let fragment = export.next().await.unwrap().expect("CMAF fragment");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(true);
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
		producer.finish().unwrap();

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
				max_delay: std::time::Duration::from_secs(10),
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

		// The skip is the local read cursor and nothing else. Asking for it
		// through `Subscription::ranges` would look equivalent and is not: the
		// floor is aggregated across every live subscriber and tells the
		// publisher what to send, so naming a cached sequence there rewinds the
		// track for everyone reading it. A rendition switched away from and back
		// to is the case that bites, because its cached sequence is stale by
		// then and the publisher resends the broadcast from it.
		assert_eq!(
			published.subscription().map(|sub| sub.ranges),
			Some(vec![moq_net::track::Range::oldest()]),
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
		producer.finish().unwrap();

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
				max_delay: std::time::Duration::from_secs(10),
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
		producer.finish().unwrap();

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
		};
		let max_delay = std::time::Duration::from_secs(10);
		let mut consumer = pollster::block_on(Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Options {
				decoder: decoder.clone(),
				max_delay,
				..Options::new()
			},
		))
		.unwrap();

		let subscription = published.subscription().expect("the consumer subscribed");
		assert_eq!(
			subscription.max_delay,
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
				max_delay: std::time::Duration::from_secs(10),
				..Options::new()
			},
		)
		.await
		.unwrap();
		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let write = |sequence, stamps: &[u64]| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
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
			got.push(consumer.next_frame().await.unwrap().unwrap().timestamp.as_micros());
		}
		assert_eq!(
			got,
			[1000, 1100, 2000, 1800, 1900],
			"join trims, continuous delivery keeps every leading picture"
		);
		consumer
			.reader(Lane::Min)
			.track
			.set_max_delay(std::time::Duration::ZERO);
		write(12, &[3000]);
		write(13, &[4000, 3800, 3900, 4100]);
		track.finish().unwrap();
		got.clear();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			got.push(frame.timestamp.as_micros());
		}
		assert_eq!(
			got,
			[2100, 4000, 4100],
			"a later latency skip must trim its recovery group too"
		);
	}

	/// A track ends before a decoder that reorders pictures does. The consumer
	/// drains the backend once and returns its tail before reporting the end.
	#[tokio::test]
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
		for index in 0..2u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 33_333).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				})
				.unwrap();
		}
		producer.finish().unwrap();

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

		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		assert_eq!(timestamps, vec![0, 33_333]);
		assert!(
			consumer.next_frame().await.unwrap().is_none(),
			"the decoder was drained twice"
		);
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
				max_delay: std::time::Duration::from_secs(10),
				..Options::new()
			},
		)
		.await
		.unwrap();

		let mut reads = Vec::new();
		let mut read = async |consumer: &mut Consumer| {
			reads.push(match consumer.read().await.unwrap() {
				Some(Read::Frame(frame)) => format!("frame {}", frame.timestamp.as_micros()),
				Some(Read::Gap { timestamp, duration }) => {
					format!("gap {}+{}", timestamp.as_micros(), duration.as_micros())
				}
				Some(Read::End { end }) => format!("end {}", end.as_micros()),
				None => "none".to_string(),
			});
		};
		// Nothing follows the end yet.
		read(&mut consumer).await;
		read(&mut consumer).await;

		producer.write(picture(200_000, b"new access unit")).unwrap();
		producer.finish().unwrap();
		read(&mut consumer).await;
		read(&mut consumer).await;
		assert_eq!(reads, ["frame 100000", "end 133333", "frame 200000", "none"]);
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
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		producer.finish().unwrap();

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
		let deadline = moq_clock::Instant::now() + std::time::Duration::from_secs(1);
		loop {
			tokio::select! {
				_result = &mut read => panic!("flush returned before cancellation"),
				_ = moq_clock::sleep(std::time::Duration::from_millis(1)) => {
					if probe::flush_entered() {
						break;
					}
					if moq_clock::Instant::now() >= deadline {
						probe::release_flush();
						panic!("flush never reached the codec thread");
					}
				}
			}
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
		for index in 0..FRAMES {
			if index == 0 {
				encoder.cut().unwrap();
			}
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				producer
					.write(moq_mux::container::Frame {
						timestamp: encoded.timestamp,
						duration: None,
						payload: encoded.payload,
						keyframe: index == 0,
					})
					.unwrap();
			}
		}
		producer.finish().unwrap();

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
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		let expected: Vec<u128> = (0..FRAMES as u128).map(|index| index * 33_333).collect();
		assert_eq!(timestamps, expected, "the track ended before the stream did");

		// The end stays the end: the drain runs once, so a caller that keeps
		// reading past it does not get the tail a second time.
		assert!(consumer.next_frame().await.unwrap().is_none());
	}

	/// One frame per second, so the frame interval the selector holds a picture
	/// for dwarfs a decode thread's round trip even on a loaded host.
	const SECOND: u64 = 1_000_000;

	/// A sweep of four frames.
	const WARMUP: std::time::Duration = std::time::Duration::from_secs(4);

	fn warmup_catalog(warmup: Option<std::time::Duration>) -> VideoConfig {
		let mut catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		catalog.warmup = warmup;
		catalog
	}

	fn live_track() -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		(broadcast, track)
	}

	/// Append frames at `seconds` to `group`, the first frame of a group being its recovery point.
	fn write(group: &mut moq_net::group::Producer, seconds: &[u64]) {
		let wire = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let frames: Vec<_> = seconds
			.iter()
			.map(|second| moq_mux::container::Frame {
				timestamp: Timestamp::from_micros(second * SECOND).unwrap(),
				duration: None,
				payload: Bytes::from_static(b"access unit"),
				keyframe: *second % 4 == 0,
			})
			.collect();
		wire.write(group, &frames).unwrap();
	}

	fn group(track: &mut moq_net::track::Producer, sequence: u64, seconds: &[u64]) -> moq_net::group::Producer {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		write(&mut group, seconds);
		group
	}

	async fn dual(
		broadcast: &moq_net::broadcast::Producer,
		start: Start,
		max_delay: std::time::Duration,
		kind: &str,
	) -> Consumer {
		Consumer::new(
			&broadcast.consume(),
			&warmup_catalog(Some(WARMUP)),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(kind.into()),
					..Config::new()
				},
				start,
				max_delay,
			},
		)
		.await
		.unwrap()
	}

	/// Read one frame, as (seconds, the lane it was shown from).
	async fn read(consumer: &mut Consumer) -> (u64, Lane) {
		let frame = moq_clock::timeout(std::time::Duration::from_secs(10), consumer.next_frame())
			.await
			.expect("a frame within ten seconds")
			.unwrap()
			.expect("the track is live");
		let seconds = u64::try_from(frame.timestamp.as_micros()).unwrap() / SECOND;
		(seconds, consumer.selector.source())
	}

	/// Let the decoders run with nothing new to show until the max-delay one
	/// has caught up, or fail after ten seconds of trying.
	async fn catch_up(consumer: &mut Consumer) {
		let deadline = moq_clock::Instant::now() + std::time::Duration::from_secs(10);
		while consumer.selector.source() != Lane::Max {
			assert!(
				moq_clock::Instant::now() < deadline,
				"the max-delay decoder never caught up"
			);
			if let Ok(frame) = moq_clock::timeout(std::time::Duration::from_millis(20), consumer.next_frame()).await {
				panic!(
					"nothing new was published, yet read returned {:?}",
					frame.map(|f| f.map(|f| f.timestamp))
				);
			}
		}
	}

	/// A cold join shows the min-delay decoder's pictures at the live edge first,
	/// then the max-delay decoder's once it has decoded the sweep before them.
	#[tokio::test]
	async fn a_cold_join_hands_over_to_the_max_delay_decoder() {
		let (broadcast, mut track) = live_track();
		let published = track.clone();
		group(&mut track, 0, &[0, 1, 2, 3]).finish().unwrap();
		let mut live = group(&mut track, 1, &[4, 5]);

		let mut consumer = dual(
			&broadcast,
			Start::Latest,
			std::time::Duration::ZERO,
			probe::IMMEDIATE_NAME,
		)
		.await;
		assert_eq!(
			published.subscription().and_then(|sub| sub.max_delay),
			Some(WARMUP),
			"the two subscriptions did not share one request at the larger budget"
		);

		// The max-delay decoder starts at 0, so only the min-delay one can show 4 first.
		assert_eq!(read(&mut consumer).await.0, 4, "the live edge shows first");
		assert_eq!(read(&mut consumer).await.0, 5);
		catch_up(&mut consumer).await;
		for second in 6..9 {
			write(&mut live, &[second]);
			assert_eq!(read(&mut consumer).await, (second, Lane::Max));
		}
	}

	/// A stall shorter than a sweep makes the min-delay decoder skip, while the
	/// max-delay one waits it out, and takes the picture back once it catches up,
	/// never alternating in between.
	#[tokio::test]
	async fn a_short_stall_skips_only_the_min_delay_decoder() {
		let (broadcast, mut track) = live_track();
		let mut consumer = dual(
			&broadcast,
			Start::Oldest,
			std::time::Duration::ZERO,
			probe::IMMEDIATE_NAME,
		)
		.await;

		// One frame at a time once both decoders are caught up, so every picture
		// below has exactly one lane that can show it.
		let mut first = group(&mut track, 0, &[0]);
		assert_eq!(read(&mut consumer).await.0, 0);
		catch_up(&mut consumer).await;
		let mut shown = Vec::new();
		for second in 1..4 {
			write(&mut first, &[second]);
			shown.push(read(&mut consumer).await);
		}
		first.finish().unwrap();
		let mut stalled = group(&mut track, 1, &[4]);
		shown.push(read(&mut consumer).await);

		// Group 1 stalls and group 2 starts: the min-delay decoder skips to it,
		// leading by more than a frame, so it is shown at once.
		let mut live = group(&mut track, 2, &[8]);
		shown.push(read(&mut consumer).await);
		assert!(
			moq_clock::timeout(std::time::Duration::from_millis(100), consumer.next_frame())
				.await
				.is_err()
		);
		assert_eq!(
			consumer.selector.source(),
			Lane::Min,
			"the max-delay decoder skipped the stall too"
		);

		write(&mut stalled, &[5, 6, 7]);
		stalled.finish().unwrap();
		catch_up(&mut consumer).await;
		for second in 9..12 {
			write(&mut live, &[second]);
			shown.push(read(&mut consumer).await);
		}

		assert_eq!(
			shown,
			[
				(1, Lane::Max),
				(2, Lane::Max),
				(3, Lane::Max),
				(4, Lane::Max),
				(8, Lane::Min),
				(9, Lane::Max),
				(10, Lane::Max),
				(11, Lane::Max),
			]
		);
	}

	/// A stall longer than a sweep skips both decoders, so the stalled group's
	/// late frames are never shown, and the max-delay decoder still takes over.
	#[tokio::test]
	async fn a_long_stall_skips_both_decoders() {
		let (broadcast, mut track) = live_track();
		let mut consumer = dual(
			&broadcast,
			Start::Oldest,
			std::time::Duration::ZERO,
			probe::IMMEDIATE_NAME,
		)
		.await;

		// Written as it is read: a zero budget would skip a backlog.
		group(&mut track, 0, &[0, 1, 2, 3]).finish().unwrap();
		let mut shown = Vec::new();
		for _ in 0..4 {
			shown.push(read(&mut consumer).await.0);
		}
		let mut stalled = group(&mut track, 1, &[4]);
		shown.push(read(&mut consumer).await.0);
		let mut live = group(&mut track, 2, &[8, 9, 10, 11, 12]);
		for _ in 0..5 {
			shown.push(read(&mut consumer).await.0);
		}
		catch_up(&mut consumer).await;

		write(&mut stalled, &[5, 6, 7]);
		stalled.finish().unwrap();
		write(&mut live, &[13]);
		shown.push(read(&mut consumer).await.0);
		assert_eq!(shown, [0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 13]);
	}

	/// Both decoders hold their last picture until the track ends, and each
	/// drains it exactly once.
	#[tokio::test]
	async fn the_tail_drains_from_both_decoders() {
		let (broadcast, mut track) = live_track();
		group(&mut track, 0, &[0, 1, 2, 3]).finish().unwrap();
		group(&mut track, 1, &[4, 5]).finish().unwrap();
		track.finish().unwrap();

		// A budget that keeps every cached group, so both decoders read them all.
		let mut consumer = dual(&broadcast, Start::Oldest, WARMUP, probe::BUFFERED_NAME).await;
		let mut shown = Vec::new();
		while let Some(frame) = consumer.next_frame().await.unwrap() {
			shown.push(u64::try_from(frame.timestamp.as_micros()).unwrap() / SECOND);
		}
		assert_eq!(shown, [0, 1, 2, 3, 4, 5]);
		assert!(consumer.next_frame().await.unwrap().is_none());
		assert!(consumer.lanes.iter().all(|slot| matches!(slot, Slot::Closed)));
	}

	/// A rendition without `warmup` opens one subscription and one decoder.
	#[tokio::test]
	async fn no_warmup_runs_one_decoder() {
		for warmup in [None, Some(std::time::Duration::ZERO)] {
			let (broadcast, track) = live_track();
			let max_delay = std::time::Duration::from_millis(500);
			let consumer = Consumer::new(
				&broadcast.consume(),
				&warmup_catalog(warmup),
				"video",
				Options {
					decoder: Config {
						kind: Kind::Named(probe::IMMEDIATE_NAME.into()),
						..Config::new()
					},
					max_delay,
					..Options::new()
				},
			)
			.await
			.unwrap();
			assert!(matches!(consumer.lanes[Lane::Max as usize], Slot::Closed));
			assert_eq!(track.subscription().and_then(|sub| sub.max_delay), Some(max_delay));
		}
	}

	/// A warmup rendition needs both decoders: one that cannot open fails the
	/// consumer instead of playing with stripes and no signal.
	#[tokio::test]
	async fn a_second_decoder_that_cannot_open_fails_the_consumer() {
		let (broadcast, _track) = live_track();
		let result = Consumer::new(
			&broadcast.consume(),
			&warmup_catalog(Some(WARMUP)),
			"video",
			Options {
				decoder: Config {
					kind: Kind::Named(probe::SINGLE_NAME.into()),
					..Config::new()
				},
				..Options::new()
			},
		)
		.await;
		let Err(err) = result else {
			panic!("the consumer opened without its max-delay decoder");
		};
		assert!(
			err.to_string().contains("out of decoder sessions"),
			"unexpected error: {err}"
		);
	}

	/// A max-delay decode error reaches the caller, like a min-delay one.
	#[tokio::test]
	async fn a_max_delay_decode_error_fails_the_read() {
		let (broadcast, mut track) = live_track();
		// Only the max-delay decoder reads group 0, a group before the live edge.
		let mut earlier = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video)
			.write(
				&mut earlier,
				&[moq_mux::container::Frame {
					timestamp: Timestamp::ZERO,
					duration: None,
					payload: Bytes::from_static(probe::CORRUPT),
					keyframe: true,
				}],
			)
			.unwrap();
		write(&mut earlier, &[1, 2, 3]);
		earlier.finish().unwrap();
		let _live = group(&mut track, 1, &[4, 5]);

		let mut consumer = dual(
			&broadcast,
			Start::Latest,
			std::time::Duration::ZERO,
			probe::IMMEDIATE_NAME,
		)
		.await;
		for _ in 0..3 {
			match moq_clock::timeout(std::time::Duration::from_secs(10), consumer.next_frame()).await {
				Ok(Ok(Some(_))) => continue,
				Ok(Err(err)) => {
					assert!(err.to_string().contains("corrupt"), "unexpected error: {err}");
					return;
				}
				Ok(Ok(None)) => panic!("the live track ended"),
				Err(_) => break,
			}
		}
		panic!("the max-delay decoder's error was swallowed");
	}
}

//! Timeline publish/subscribe.
//!
//! A timeline is one media track's group index: one [`hang::timeline::Record`] per group,
//! appended the moment the group opens, mapping a group sequence to the group's start
//! timestamp. A consumer can answer "which group covers time T" and "where is the live edge"
//! from a few bytes per group without subscribing to media, the primitive a playlist server
//! (HLS/DASH), a seek bar, or a recorder index needs.
//!
//! A timeline can be shared: audio and video groups have different durations so they need separate
//! timelines, but an aligned set of renditions (a transcode ladder whose rungs mirror the source's
//! group boundaries) can point at one. The write side splits by role:
//!
//! - [`Producer`] owns the track and its catalog metadata: the [`section`](Producer::section)
//!   advertised in a rendition's config and the [`set_wall`](Producer::set_wall) anchor. Get one
//!   from [`catalog::Producer::timeline`](crate::catalog::Producer::timeline); it is `Clone`, and
//!   every clone shares the one track, so N renditions advertising the same timeline share it.
//! - [`Recorder`] is the move-only handle a media track records group opens through, wired into a
//!   rendition's [`container::Producer`](crate::container::Producer) via
//!   [`with_recorder`](crate::container::Producer::with_recorder) (or, for the 1:1 default,
//!   [`catalog::Producer::media_producer`](crate::catalog::Producer::media_producer)). Recording is
//!   what fills a shared timeline, so wire exactly one recorder into it (the source), and let the
//!   other renditions only advertise.
//!
//! On the read side, [`Consumer::subscribe`] reads a timeline straight from its
//! [`hang::catalog::Timeline`] section (so the track name and timescale come from the catalog and
//! can't be mismatched) and yields decoded [`Entry`]s with a real [`Timestamp`].
//!
//! On the wire the track is a DEFLATE-compressed [`moq_json::stream`] (a single group, one record
//! per frame; see [`hang::timeline`] for the record schema).
//!
//! Recording is throttled to a [`granularity`](Producer::with_granularity) (default
//! [`DEFAULT_GRANULARITY`], one second): at most one record per that much media time. Video
//! keyframes are already a granularity or more apart, so every group is indexed; short audio groups
//! are thinned out. A consumer that lands between two records extrapolates the group number
//! (sequences are contiguous) or fetches to fill the gap.

use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::SystemTime;

use hang::catalog::Timeline;
use hang::timeline::{Record, RecordExt, track_name};

use moq_net::{Timescale, Timestamp};

/// The default [`granularity`](Producer::with_granularity): at most one record per second of
/// media time.
pub const DEFAULT_GRANULARITY: Timestamp = Timestamp::new_const(1, Timescale::SECOND);

/// A media timeline: its catalog [`section`](Self::section) and wall anchor, and the [`Recorder`]
/// its group opens are recorded through.
///
/// Generic over the record extension `E` (defaulting to `()`; see [`RecordExt`]). `Clone`, and every
/// clone shares the one track and its wall anchor, so a set of aligned renditions can advertise one
/// timeline. Get one from [`catalog::Producer::timeline`](crate::catalog::Producer::timeline), which
/// keeps ownership and closes the track when the catalog finishes.
#[derive(Clone)]
pub struct Producer<E: RecordExt = ()> {
	inner: moq_json::stream::Producer<Record<E>>,
	track: String,
	timescale: Timescale,
	granularity: Timestamp,
	// The wall-clock time of pts 0, in timescale units since the moq epoch, advertised in section().
	// Shared across clones so a set_wall on one is seen by every rendition advertising this timeline.
	wall: Arc<Mutex<Option<u64>>>,
}

impl<E: RecordExt> Producer<E> {
	/// Create a timeline track for the media rendition `name` on the given broadcast.
	///
	/// The track is named per [`hang::timeline::track_name`] (`<name>.timeline.z`) at the
	/// default millisecond timescale and [`DEFAULT_GRANULARITY`].
	pub fn new(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> Result<Self, moq_net::Error> {
		let track = track_name(name);
		let net = broadcast.create_track(track.as_str(), None)?;

		let config = moq_json::stream::ProducerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::stream::Producer::new(net, config),
			track,
			timescale: Timescale::new(Timeline::default_timescale() as u64).expect("default timescale is nonzero"),
			granularity: DEFAULT_GRANULARITY,
			wall: Arc::new(Mutex::new(None)),
		})
	}

	/// Set the record throttle: at most one record per `granularity` of media time. See
	/// [`DEFAULT_GRANULARITY`]. Applies to recorders minted after this call.
	pub fn with_granularity(mut self, granularity: Timestamp) -> Self {
		self.granularity = granularity;
		self
	}

	/// The catalog section advertising this timeline, to attach to the rendition's config.
	pub fn section(&self) -> Timeline {
		let mut section = Timeline::new(&self.track);
		section.timescale = self.timescale.as_u64() as u32;
		section.wall = *self.wall.lock().unwrap();
		section
	}

	/// Set (or replace) the wall-clock anchor advertised in the catalog section, from an observed
	/// pairing of a media timestamp `pts` with its wall-clock time `wall`.
	///
	/// Stored as the extrapolated wall-clock time of pts 0, the single value the
	/// [`Timeline::wall`](hang::catalog::Timeline::wall) field carries: in this timeline's timescale,
	/// measured from the moq epoch ([`MOQ_EPOCH_UNIX_MILLIS`](hang::catalog::MOQ_EPOCH_UNIX_MILLIS),
	/// 2020). Read every time the rendition republishes its catalog entry, so set it before (or as)
	/// the rendition registers.
	pub fn set_wall(&mut self, pts: Timestamp, wall: SystemTime) {
		let unix_millis = wall
			.duration_since(SystemTime::UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis();
		let scale = self.timescale.as_u64() as u128;
		let pts_units = pts.as_scale(self.timescale);
		let moq_millis = unix_millis.saturating_sub(hang::catalog::MOQ_EPOCH_UNIX_MILLIS as u128);
		let moq_units = moq_millis * scale / 1000;
		*self.wall.lock().unwrap() = Some(moq_units.saturating_sub(pts_units) as u64);
	}

	/// Mint a [`Recorder`] to record group opens into this timeline.
	///
	/// Wire it into a media track's [`container::Producer`](crate::container::Producer) with
	/// [`with_recorder`](crate::container::Producer::with_recorder). A recorder owns its own throttle
	/// cursor, so wire exactly one per timeline (a shared timeline is filled by its source alone).
	pub fn recorder(&self) -> Recorder<E> {
		Recorder {
			inner: self.inner.clone(),
			timescale: self.timescale,
			granularity: self.granularity,
			last: None,
		}
	}

	/// Finish the timeline track, closing its group.
	pub fn finish(&mut self) -> Result<(), moq_net::Error> {
		match self.inner.finish() {
			Ok(()) => Ok(()),
			Err(moq_json::Error::Net(err)) => Err(err),
			Err(err) => unreachable!("timeline finish failed to encode: {err}"),
		}
	}
}

/// Records a media track's group opens into its timeline, throttled to a granularity.
///
/// Move-only (not `Clone`): it owns its throttle cursor, so wire exactly one per timeline. Minted by
/// [`Producer::recorder`] and held by a rendition's
/// [`container::Producer`](crate::container::Producer).
pub struct Recorder<E: RecordExt = ()> {
	inner: moq_json::stream::Producer<Record<E>>,
	timescale: Timescale,
	granularity: Timestamp,
	// The pts of the last recorded group; the throttle floor. Owned, since a recorder is 1:1.
	last: Option<Timestamp>,
}

impl<E: RecordExt> Recorder<E> {
	/// Record that group `sequence` opened at presentation time `pts`, unless it falls within the
	/// granularity of the last recorded group (skipped, so a consumer extrapolates or fetches).
	pub(crate) fn record(&mut self, sequence: u64, pts: Timestamp) -> Result<(), moq_net::Error> {
		if let Some(last) = self.last
			&& pts.as_micros() < last.as_micros() + self.granularity.as_micros()
		{
			return Ok(());
		}
		self.last = Some(pts);

		let record = Record::new(sequence, pts.as_scale(self.timescale) as u64);
		match self.inner.append(&record) {
			Ok(()) => Ok(()),
			Err(moq_json::Error::Net(err)) => Err(err),
			// A base record is plain integers and the DEFLATE encoder is infallible, so only a
			// transport error can surface.
			Err(err) => unreachable!("timeline record failed to encode: {err}"),
		}
	}
}

/// One decoded timeline entry: a group and the [`Timestamp`] it opened at.
///
/// `pts` is a real timestamp, already converted from the record's on-wire timescale, so a reader
/// never juggles timescale units.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<E: RecordExt = ()> {
	/// The group's sequence number, as used by FETCH/SUBSCRIBE on the media track.
	pub group: u64,

	/// The group's start (its first frame's presentation timestamp).
	pub pts: Timestamp,

	/// The record's application extension (nothing for the default `()`).
	pub ext: E,
}

/// Reads a media track's timeline, yielding decoded [`Entry`]s in publish order.
///
/// Generic over the record extension `E` (see [`RecordExt`]).
pub struct Consumer<E: RecordExt = ()> {
	inner: moq_json::stream::Consumer<Record<E>>,
	timescale: Timescale,
}

impl<E: RecordExt> Consumer<E> {
	/// Subscribe to the timeline advertised by a media track's [`Timeline`] catalog section.
	///
	/// The section supplies both the track name and the timescale, so a reader can't pair the wrong
	/// scale with the track.
	pub async fn subscribe(
		broadcast: &moq_net::broadcast::Consumer,
		section: &Timeline,
	) -> Result<Self, moq_net::Error> {
		let track = broadcast.track(&section.track)?.subscribe(None).await?;

		let config = moq_json::stream::ConsumerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::stream::Consumer::new(track, config),
			timescale: Timescale::new(section.timescale as u64).unwrap_or(Timescale::MILLI),
		})
	}

	fn decode(&self, record: Record<E>) -> Entry<E> {
		Entry {
			group: record.group,
			pts: Timestamp::new(record.pts, self.timescale).unwrap_or(Timestamp::ZERO),
			ext: record.ext,
		}
	}

	/// Get the next entry, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<Entry<E>>, moq_json::Error> {
		match self.inner.next().await? {
			Some(record) => Ok(Some(self.decode(record))),
			None => Ok(None),
		}
	}

	/// Poll for the next entry, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Entry<E>>, moq_json::Error>> {
		match self.inner.poll_next(waiter)? {
			Poll::Ready(Some(record)) => Poll::Ready(Ok(Some(self.decode(record)))),
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;

	fn entry(group: u64, pts_ms: u64) -> Entry {
		Entry {
			group,
			pts: Timestamp::from_millis(pts_ms).unwrap(),
			ext: (),
		}
	}

	/// Drain a finished timeline track by subscribing to the producer's advertised section.
	async fn drain(broadcast: &moq_net::broadcast::Producer, producer: &Producer) -> Vec<Entry> {
		let mut consumer = Consumer::subscribe(&broadcast.consume(), &producer.section())
			.await
			.unwrap();
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(entry))) = consumer.poll_next(&waiter) {
			out.push(entry);
		}
		out
	}

	fn frame(timestamp_us: u64, keyframe: bool) -> crate::container::Frame {
		crate::container::Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			payload: bytes::Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	#[tokio::test]
	async fn records_group_opens_in_milliseconds() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = Producer::new(&mut broadcast, "video0").unwrap();
		assert_eq!(timeline.track, "video0.timeline.z");

		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy)
			.with_recorder(timeline.recorder());

		media.write(frame(0, true)).unwrap(); // group 0 @ 0us
		media.write(frame(2_000_000, false)).unwrap(); // extends group 0
		media.write(frame(4_000_000, true)).unwrap(); // group 1 @ 4_000_000us
		media.finish().unwrap();
		timeline.finish().unwrap();

		// Entry pts is a real Timestamp (decoded from the ms-timescale record).
		assert_eq!(drain(&broadcast, &timeline).await, vec![entry(0, 0), entry(1, 4_000)]);
	}

	#[tokio::test]
	async fn granularity_throttles_records() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = Producer::new(&mut broadcast, "audio0").unwrap();
		let mut recorder = timeline.recorder();

		// Default granularity is 1s. Group opens 300ms apart, all within a second of the first, then
		// one past it: only the first and the one past the granularity are recorded.
		for (seq, ms) in [(0u64, 0u64), (1, 300), (2, 600), (3, 900), (4, 1200)] {
			recorder.record(seq, Timestamp::from_millis(ms).unwrap()).unwrap();
		}
		drop(recorder);
		timeline.finish().unwrap();

		assert_eq!(drain(&broadcast, &timeline).await, vec![entry(0, 0), entry(4, 1200)]);
	}

	#[test]
	fn section_advertises_track_and_wall() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = Producer::<()>::new(&mut broadcast, "audio0").unwrap();

		let section = timeline.section();
		assert_eq!(section.track, "audio0.timeline.z");
		assert_eq!(section.timescale, 1000);
		assert_eq!(section.wall, None);

		// pts 0 observed at a wall time => wall of pts 0 is that time minus the moq epoch (ms scale).
		let moq = hang::catalog::MOQ_EPOCH_UNIX_MILLIS;
		let observed = SystemTime::UNIX_EPOCH + Duration::from_millis(1_751_846_400_000);
		timeline.set_wall(Timestamp::from_micros(0).unwrap(), observed);
		assert_eq!(timeline.section().wall, Some(1_751_846_400_000 - moq));

		// A nonzero pts extrapolates back to pts 0: a frame at pts 2s observed at that wall time means
		// pts 0 was 2s (2000 ms) earlier.
		timeline.set_wall(Timestamp::from_micros(2_000_000).unwrap(), observed);
		assert_eq!(timeline.section().wall, Some(1_751_846_400_000 - moq - 2_000));
	}

	#[tokio::test]
	async fn consumer_decodes_pts_from_the_section() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = Producer::new(&mut broadcast, "video0").unwrap();
		timeline
			.recorder()
			.record(3, Timestamp::from_micros(7_000).unwrap())
			.unwrap();
		timeline.finish().unwrap();

		// The reader takes the track name + timescale from the section, and yields a real Timestamp.
		// The pts is decoded at the timeline's (millisecond) timescale, so compare the instant rather
		// than the scale-sensitive representation (7ms == 7000us as an instant, but not field-wise).
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries, vec![entry(3, 7)]);
		assert_eq!(entries[0].pts.as_micros(), 7_000);
	}
}

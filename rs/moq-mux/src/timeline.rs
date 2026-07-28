//! Timeline publish/subscribe: the segment index HLS/DASH export is built from.
//!
//! A timeline is one media track's segment index: one [`hang::timeline::Record`] per segment,
//! appended the moment the segment's first group opens, mapping an aligned segment number to
//! the group and timestamp it starts at. A consumer can answer "which groups cover segment N"
//! and "where is the live edge" from a few bytes per segment without subscribing to media,
//! which is the primitive a playlist server (HLS/DASH), a seek bar, or a recorder index needs.
//!
//! ## Aligned segments
//!
//! Segment numbers are shared across the broadcast's tracks through one [`Segmenter`]: segment
//! 5 of the audio timeline covers the same span of content time as segment 5 of the video
//! timeline, which is what HLS requires of switchable renditions. The tracks differ only in
//! how many groups a segment holds, declared per recorder as a [`Cadence`]:
//!
//! - [`Cadence::Boundary`]: every group opens the next segment. This is the video track: its
//!   groups already open on keyframes, and a keyframe is where a segment must start, so a
//!   video segment is exactly one group. The boundary track is what paces the broadcast's
//!   segments; wire exactly one per segmenter.
//! - [`Cadence::Aligned`]: groups pack into the segments the boundary track opens. This is
//!   audio (and any other short-group track): the first group at or after each boundary is
//!   recorded as the segment's start, and the groups before the next record fill out the
//!   segment. When no boundary track ever records (an audio-only broadcast), an aligned
//!   recorder paces segments itself at the segmenter's [`interval`](Segmenter::with_interval).
//!
//! ## Handles
//!
//! The write side splits by role:
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
//! can't be mismatched) and yields decoded [`Entry`]s with a real [`Timestamp`]. It is generic over
//! a [`RecordExt`], so it can read the extra fields another publisher flattens into a record; the
//! write side publishes the base record shape only.
//!
//! On the wire the track is a DEFLATE-compressed [`moq_json::stream`] (a single group, one record
//! per frame; see [`hang::timeline`] for the record schema).

use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::SystemTime;

use hang::catalog::Timeline;
use hang::timeline::{Record, RecordExt, track_name};

use moq_net::{Timescale, Timestamp};

/// The default [`Segmenter`] interval: an undriven (audio-only) timeline opens a segment about
/// once per second of media time.
pub const DEFAULT_INTERVAL: Timestamp = Timestamp::new_const(1, Timescale::SECOND);

/// How a track's groups map onto the broadcast's aligned segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Cadence {
	/// Every group opens the next segment (video: groups open on keyframes, and a segment must
	/// start on one, so a video segment is exactly one group). The boundary track paces the
	/// broadcast's segments; wire exactly one per [`Segmenter`].
	Boundary,

	/// Groups pack into the segments the boundary track opens (audio: many short groups per
	/// segment). The first group at or after each boundary starts the track's slice of that
	/// segment. When no boundary track ever records, the recorder paces segments itself at the
	/// segmenter's interval, so an audio-only broadcast still segments.
	Aligned,
}

/// The state one [`Segmenter`] guards: the open segment and how it was opened.
struct SegmenterState {
	/// The open segment's number and boundary timestamp, `None` before the first group.
	current: Option<(u64, Timestamp)>,
	/// A [`Cadence::Boundary`] recorder has opened a segment, so aligned recorders never
	/// self-pace: the boundary track owns the cadence from its first group on.
	driven: bool,
	/// How often an *undriven* segmenter opens a new segment (see [`Segmenter::with_interval`]).
	interval: Timestamp,
}

/// The broadcast-wide segment counter every [`Recorder`] shares, so segment numbers align
/// across tracks.
///
/// One per broadcast: [`catalog::Producer`](crate::catalog::Producer) owns one and wires it
/// into every timeline it mints (see
/// [`catalog::Producer::segmenter`](crate::catalog::Producer::segmenter)). `Clone` shares the
/// counter. The [`Cadence::Boundary`] recorder advances it; [`Cadence::Aligned`] recorders
/// read it, falling back to [`interval`](Self::with_interval) pacing only while no boundary
/// recorder has spoken.
#[derive(Clone)]
pub struct Segmenter {
	state: Arc<Mutex<SegmenterState>>,
}

impl Default for Segmenter {
	fn default() -> Self {
		Self::new()
	}
}

impl Segmenter {
	/// A fresh segmenter: no segments yet, [`DEFAULT_INTERVAL`] pacing while undriven.
	pub fn new() -> Self {
		Self {
			state: Arc::new(Mutex::new(SegmenterState {
				current: None,
				driven: false,
				interval: DEFAULT_INTERVAL,
			})),
		}
	}

	/// Set the pacing interval an *undriven* segmenter (no [`Cadence::Boundary`] recorder, e.g.
	/// an audio-only broadcast) opens segments at. Ignored once a boundary recorder is pacing.
	///
	/// Applies to the shared state, so it also works on a clone that is already wired in.
	pub fn with_interval(self, interval: Timestamp) -> Self {
		self.state.lock().unwrap().interval = interval;
		self
	}

	/// A [`Cadence::Boundary`] group open at `pts`: open the next segment and return its number.
	///
	/// The first boundary adopts a segment an aligned recorder self-opened (rather than
	/// stranding it as a number the boundary track never records), re-anchoring its boundary to
	/// `pts`; from then on every call opens a new segment.
	fn boundary(&self, pts: Timestamp) -> u64 {
		let mut state = self.state.lock().unwrap();
		let segment = match state.current {
			None => 0,
			Some((segment, _)) if !state.driven => segment,
			Some((segment, _)) => segment + 1,
		};
		state.current = Some((segment, pts));
		state.driven = true;
		segment
	}

	/// A [`Cadence::Aligned`] group open at `pts`, having last recorded `last`: the segment this
	/// group starts for its track, or `None` if it merely extends the one already recorded.
	///
	/// A group before the current boundary extends the previous segment; the first group at or
	/// after it starts the track's slice of the current one. While undriven, a group a full
	/// interval past the boundary opens the next segment itself.
	fn align(&self, pts: Timestamp, last: Option<u64>) -> Option<u64> {
		let mut state = self.state.lock().unwrap();
		let Some((segment, boundary)) = state.current else {
			// The very first group of the broadcast: open segment 0 at its timestamp.
			state.current = Some((0, pts));
			return Some(0);
		};
		if last != Some(segment) && pts.as_micros() >= boundary.as_micros() {
			return Some(segment);
		}
		if !state.driven && pts.as_micros() >= boundary.as_micros() + state.interval.as_micros() {
			state.current = Some((segment + 1, pts));
			return Some(segment + 1);
		}
		None
	}
}

/// A media timeline: its catalog [`section`](Self::section) and wall anchor, and the [`Recorder`]
/// its group opens are recorded through.
///
/// Publishes the base [`Record`] shape (no extension); a [`Consumer`] can still read a record
/// extension published by another implementation. `Clone`, and every clone shares the one track and
/// its wall anchor, so a set of aligned renditions can advertise one timeline. Get one from
/// [`catalog::Producer::timeline`](crate::catalog::Producer::timeline), which keeps ownership,
/// wires the broadcast's shared [`Segmenter`], and closes the track when the catalog finishes.
#[derive(Clone)]
pub struct Producer {
	inner: moq_json::stream::Producer<Record>,
	track: String,
	timescale: Timescale,
	segmenter: Segmenter,
	// The wall-clock time of pts 0, in timescale units since the moq epoch, advertised in section().
	// Shared across clones so a set_wall on one is seen by every rendition advertising this timeline.
	wall: Arc<Mutex<Option<u64>>>,
}

impl Producer {
	/// Create a timeline track for the media rendition `name` on the given broadcast, numbering
	/// segments through `segmenter`.
	///
	/// Pass the broadcast's shared segmenter so this track's segment numbers align with its
	/// siblings'; a track segmented on its own (nothing to align with) passes a fresh one. The
	/// track is named per [`hang::timeline::track_name`] (`<name>.timeline.z`) at the default
	/// millisecond timescale.
	pub fn new(
		broadcast: &mut moq_net::broadcast::Producer,
		name: &str,
		segmenter: Segmenter,
	) -> Result<Self, moq_net::Error> {
		let track = track_name(name);
		let net = broadcast.create_track(track.as_str(), None)?;

		let config = moq_json::stream::ProducerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::stream::Producer::new(net, config),
			track,
			timescale: Timescale::new(Timeline::default_timescale() as u64).expect("default timescale is nonzero"),
			segmenter,
			wall: Arc::new(Mutex::new(None)),
		})
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

	/// Mint a [`Recorder`] to record group opens into this timeline, mapping them onto the
	/// shared segment numbering per `cadence`.
	///
	/// Wire it into a media track's [`container::Producer`](crate::container::Producer) with
	/// [`with_recorder`](crate::container::Producer::with_recorder). A recorder owns its own
	/// cursor, so wire exactly one per timeline (a shared timeline is filled by its source alone).
	pub fn recorder(&self, cadence: Cadence) -> Recorder {
		Recorder {
			inner: self.inner.clone(),
			timescale: self.timescale,
			segmenter: self.segmenter.clone(),
			cadence,
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

/// Records a media track's group opens into its timeline as aligned segments.
///
/// Move-only (not `Clone`): it owns its segment cursor, so wire exactly one per timeline. Minted by
/// [`Producer::recorder`] with the track's [`Cadence`] and held by a rendition's
/// [`container::Producer`](crate::container::Producer).
pub struct Recorder {
	inner: moq_json::stream::Producer<Record>,
	timescale: Timescale,
	segmenter: Segmenter,
	cadence: Cadence,
	// The last segment this track recorded; the cursor an aligned recorder dedupes against.
	last: Option<u64>,
}

impl Recorder {
	/// Record that group `sequence` opened at presentation time `pts`. Per the [`Cadence`], the
	/// group either opens a segment (recorded) or extends the current one (skipped).
	pub(crate) fn record(&mut self, sequence: u64, pts: Timestamp) -> Result<(), moq_net::Error> {
		let segment = match self.cadence {
			Cadence::Boundary => Some(self.segmenter.boundary(pts)),
			Cadence::Aligned => self.segmenter.align(pts, self.last),
		};
		let Some(segment) = segment else {
			return Ok(());
		};
		self.last = Some(segment);

		let record = Record::new(segment, sequence, pts.as_scale(self.timescale) as u64);
		match self.inner.append(&record) {
			Ok(()) => Ok(()),
			Err(moq_json::Error::Net(err)) => Err(err),
			// A base record is plain integers and the DEFLATE encoder is infallible, so only a
			// transport error can surface.
			Err(err) => unreachable!("timeline record failed to encode: {err}"),
		}
	}
}

/// One decoded timeline entry: an aligned segment, the group it starts at for this track, and
/// the [`Timestamp`] it opened at.
///
/// `pts` is a real timestamp, already converted from the record's on-wire timescale, so a reader
/// never juggles timescale units.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<E: RecordExt = ()> {
	/// The segment's number, aligned across the broadcast's tracks.
	pub segment: u64,

	/// The segment's first group, as used by FETCH/SUBSCRIBE on the media track. The segment
	/// covers every group up to (excluding) the next entry's `group`.
	pub group: u64,

	/// The segment's start (its first frame's presentation timestamp).
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
	///
	/// Errors if the section declares a timescale that isn't representable.
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer, section: &Timeline) -> crate::Result<Self> {
		let track = broadcast.track(&section.track)?.subscribe(None).await?;

		let config = moq_json::stream::ConsumerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::stream::Consumer::new(track, config),
			timescale: Timescale::new(section.timescale as u64)
				.map_err(|_| crate::Error::InvalidTimescale(section.timescale))?,
		})
	}

	/// Decode a record into an entry, converting its pts out of the wire timescale.
	///
	/// A pts the timescale can't represent is an error rather than a substituted value: silently
	/// moving a timestamp would misdirect seeking and live-edge logic.
	fn decode(&self, record: Record<E>) -> crate::Result<Entry<E>> {
		Ok(Entry {
			segment: record.segment,
			group: record.group,
			pts: Timestamp::new(record.pts, self.timescale)?,
			ext: record.ext,
		})
	}

	/// Get the next entry, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Entry<E>>> {
		match self.inner.next().await? {
			Some(record) => Ok(Some(self.decode(record)?)),
			None => Ok(None),
		}
	}

	/// Poll for the next entry, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Entry<E>>>> {
		match self.inner.poll_next(waiter)? {
			Poll::Ready(Some(record)) => Poll::Ready(self.decode(record).map(Some)),
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;

	fn entry(segment: u64, group: u64, pts_ms: u64) -> Entry {
		Entry {
			segment,
			group,
			pts: Timestamp::from_millis(pts_ms).unwrap(),
			ext: (),
		}
	}

	fn producer(broadcast: &mut moq_net::broadcast::Producer, name: &str, segmenter: &Segmenter) -> Producer {
		Producer::new(broadcast, name, segmenter.clone()).unwrap()
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
	async fn boundary_records_every_group_as_a_segment() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = producer(&mut broadcast, "video0", &Segmenter::new());
		assert_eq!(timeline.track, "video0.timeline.z");

		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy)
			.with_recorder(timeline.recorder(Cadence::Boundary));

		media.write(frame(0, true)).unwrap(); // group 0 @ 0us
		media.write(frame(2_000_000, false)).unwrap(); // extends group 0
		media.write(frame(4_000_000, true)).unwrap(); // group 1 @ 4_000_000us
		media.finish().unwrap();
		timeline.finish().unwrap();

		// Entry pts is a real Timestamp (decoded from the ms-timescale record); each group is a segment.
		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![entry(0, 0, 0), entry(1, 1, 4_000)]
		);
	}

	#[tokio::test]
	async fn aligned_records_the_first_group_at_each_boundary() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let segmenter = Segmenter::new();
		let mut video = producer(&mut broadcast, "video0", &segmenter);
		let mut audio = producer(&mut broadcast, "audio0", &segmenter);
		let mut leader = video.recorder(Cadence::Boundary);
		let mut follower = audio.recorder(Cadence::Aligned);

		// Interleaved group opens: video keyframes every 2s, audio groups every 300ms.
		leader.record(0, Timestamp::from_millis(0).unwrap()).unwrap(); // segment 0 @ 0
		for (seq, ms) in [
			(0u64, 0u64),
			(1, 300),
			(2, 600),
			(3, 900),
			(4, 1_200),
			(5, 1_500),
			(6, 1_800),
		] {
			follower.record(seq, Timestamp::from_millis(ms).unwrap()).unwrap();
		}
		leader.record(1, Timestamp::from_millis(2_000).unwrap()).unwrap(); // segment 1 @ 2s
		for (seq, ms) in [(7u64, 2_100u64), (8, 2_400)] {
			follower.record(seq, Timestamp::from_millis(ms).unwrap()).unwrap();
		}
		video.finish().unwrap();
		audio.finish().unwrap();

		// Video: one group per segment. Audio: the first group at/after each boundary; the rest
		// of its groups pack into the segment implicitly (they run until the next record's group).
		assert_eq!(
			drain(&broadcast, &video).await,
			vec![entry(0, 0, 0), entry(1, 1, 2_000)]
		);
		assert_eq!(
			drain(&broadcast, &audio).await,
			vec![entry(0, 0, 0), entry(1, 7, 2_100)]
		);
	}

	#[tokio::test]
	async fn undriven_aligned_recorder_paces_itself() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = producer(&mut broadcast, "audio0", &Segmenter::new());
		let mut recorder = timeline.recorder(Cadence::Aligned);

		// No boundary track: the default interval is 1s, so group opens 300ms apart self-open a
		// segment only once a full interval has passed since the last boundary.
		for (seq, ms) in [(0u64, 0u64), (1, 300), (2, 600), (3, 900), (4, 1_200)] {
			recorder.record(seq, Timestamp::from_millis(ms).unwrap()).unwrap();
		}
		drop(recorder);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![entry(0, 0, 0), entry(1, 4, 1_200)]
		);
	}

	#[tokio::test]
	async fn first_boundary_adopts_a_self_opened_segment() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let segmenter = Segmenter::new();
		let mut video = producer(&mut broadcast, "video0", &segmenter);
		let mut audio = producer(&mut broadcast, "audio0", &segmenter);
		let mut leader = video.recorder(Cadence::Boundary);
		let mut follower = audio.recorder(Cadence::Aligned);

		// Audio starts first and self-opens segment 0; the video keyframe arriving moments later
		// adopts it (same number) instead of stranding an audio-only segment 0, then paces on.
		follower.record(0, Timestamp::from_millis(0).unwrap()).unwrap();
		leader.record(0, Timestamp::from_millis(30).unwrap()).unwrap();
		leader.record(1, Timestamp::from_millis(2_030).unwrap()).unwrap();
		follower.record(70, Timestamp::from_millis(2_100).unwrap()).unwrap();
		video.finish().unwrap();
		audio.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &video).await,
			vec![entry(0, 0, 30), entry(1, 1, 2_030)]
		);
		assert_eq!(
			drain(&broadcast, &audio).await,
			vec![entry(0, 0, 0), entry(1, 70, 2_100)]
		);
	}

	#[tokio::test]
	async fn aligned_recorder_joining_late_records_the_current_segment() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let segmenter = Segmenter::new();
		let mut video = producer(&mut broadcast, "video0", &segmenter);
		let mut audio = producer(&mut broadcast, "audio0", &segmenter);
		let mut leader = video.recorder(Cadence::Boundary);

		leader.record(0, Timestamp::from_millis(0).unwrap()).unwrap();
		leader.record(1, Timestamp::from_millis(2_000).unwrap()).unwrap();

		// An audio track added mid-broadcast: its first group lands in segment 1, not 0.
		let mut follower = audio.recorder(Cadence::Aligned);
		follower.record(0, Timestamp::from_millis(2_050).unwrap()).unwrap();
		video.finish().unwrap();
		audio.finish().unwrap();

		assert_eq!(drain(&broadcast, &audio).await, vec![entry(1, 0, 2_050)]);
	}

	#[test]
	fn section_advertises_track_and_wall() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = producer(&mut broadcast, "audio0", &Segmenter::new());

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
	async fn rejects_an_invalid_timescale() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = producer(&mut broadcast, "video0", &Segmenter::new());
		timeline.finish().unwrap();

		// A timescale of 0 can't be honored, and quietly reading the track at milliseconds would
		// report timestamps the publisher never meant.
		let mut section = timeline.section();
		section.timescale = 0;
		match Consumer::<()>::subscribe(&broadcast.consume(), &section).await {
			Err(crate::Error::InvalidTimescale(0)) => {}
			Err(err) => panic!("expected an invalid timescale, got {err:?}"),
			Ok(_) => panic!("expected an invalid timescale to be rejected"),
		}
	}

	#[tokio::test]
	async fn rejects_an_out_of_range_pts() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let timeline = producer(&mut broadcast, "video0", &Segmenter::new());

		// Publish a record whose pts no Timestamp can hold, bypassing the recorder.
		let track = broadcast.create_track("raw.timeline.z", None).unwrap();
		let config = moq_json::stream::ProducerConfig::default().with_compression(true);
		let mut raw = moq_json::stream::Producer::new(track, config);
		raw.append(&Record::<()>::new(0, 0, u64::MAX)).unwrap();
		raw.finish().unwrap();

		let mut section = timeline.section();
		section.track = "raw.timeline.z".to_string();
		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), &section).await.unwrap();

		let waiter = kio::Waiter::noop();
		match consumer.poll_next(&waiter) {
			Poll::Ready(Err(crate::Error::TimestampOverflow(_))) => {}
			other => panic!("expected a decode error, got {other:?}"),
		}
	}

	#[tokio::test]
	async fn consumer_decodes_pts_from_the_section() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut timeline = producer(&mut broadcast, "video0", &Segmenter::new());
		timeline
			.recorder(Cadence::Boundary)
			.record(3, Timestamp::from_micros(7_000).unwrap())
			.unwrap();
		timeline.finish().unwrap();

		// The reader takes the track name + timescale from the section, and yields a real Timestamp.
		// The pts is decoded at the timeline's (millisecond) timescale, so compare the instant rather
		// than the scale-sensitive representation (7ms == 7000us as an instant, but not field-wise).
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries, vec![entry(0, 3, 7)]);
		assert_eq!(entries[0].pts.as_micros(), 7_000);
	}
}

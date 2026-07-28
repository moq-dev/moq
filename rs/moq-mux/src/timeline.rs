//! Timeline publish/subscribe: the broadcast's segment index HLS/DASH export is built from.
//!
//! A broadcast has one timeline track, carrying one [`hang::timeline::Record`] per *segment*:
//! a span of content time shared by every media track, mapped to the group ranges that carry
//! it on each track. A consumer can answer "which groups cover segment N on track X" and
//! "where is the live edge" from a few bytes per segment without subscribing to media, which
//! is the primitive a playlist server (HLS/DASH), a seek bar, or a recorder index needs.
//!
//! ## Facts up, policy down
//!
//! The write side splits into facts and policy, meeting in the shared [`Segmenter`]:
//!
//! - **Tracks report facts.** Each media track enrolls via [`Segmenter::track`], declaring its
//!   [`Kind`], and its [`Recorder`] reports every group open (sequence, timestamp, keyframe).
//!   A container import never decides where segments fall; it only states what it published.
//! - **The owner sets policy.** Segment boundaries come from [`Segmenter::cut`] when the
//!   application knows them (an HLS import following the source playlist, a CMAF file's
//!   on-disk segments, an encoder that places its own keyframes). When nobody cuts, the
//!   segmenter auto-cuts: on the driver track's keyframes once
//!   [`duration_max`](Segmenter::with_duration_max) has elapsed since the last boundary. The
//!   driver is the first enrolled video track (video keyframes are where a segment must
//!   start), or the first enrolled track of any kind when the broadcast has no video. The
//!   first explicit cut disables auto-cut: the application has taken control.
//! - **Records flush on completeness.** A segment's record is published only once every
//!   enrolled track has reported a group at or past the segment's end boundary (or closed),
//!   proving the segment's group ranges are final on every track. The record is then
//!   self-contained and immediately servable; a future cut (e.g. pre-registered from a source
//!   playlist) just waits for the media to catch up.
//!
//! Alignment falls out of construction: every track maps its groups onto the same boundary
//! list, so segment N covers the same span of content time on every track, which is what HLS
//! requires of switchable renditions.
//!
//! ## Wiring
//!
//! [`catalog::Producer`](crate::catalog::Producer) owns the broadcast's segmenter (the
//! catalog is what owns the broadcast's shape) and wires all of this up:
//! [`media_producer`](crate::catalog::Producer::media_producer) enrolls the track and
//! creates the timeline track on first use, advertising it in the catalog's root
//! [`hang::catalog::Timeline`] section. A broadcast that never enrolls a track
//! publishes no timeline at all: segmentation is opt-in per broadcast, never per track.
//!
//! On the read side, [`Consumer::subscribe`] reads the timeline straight from the catalog's
//! [`hang::catalog::Timeline`] section (so the track name and timescale can't be
//! mismatched) and yields decoded [`Entry`]s. On the wire the track is a DEFLATE-compressed
//! [`moq_json::stream`] (a single group, one record per frame; see [`hang::timeline`] for the
//! record schema).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use hang::catalog::Timeline;
use hang::timeline::{DEFAULT_NAME, Range, Record, RecordExt};

use moq_net::{Timescale, Timestamp};

/// The default [`Segmenter`] auto-cut threshold: with nobody cutting explicitly, a new
/// segment starts at the first driver keyframe at least this far past the last boundary.
pub const DEFAULT_DURATION_MAX: Timestamp = Timestamp::new_const(2, Timescale::SECOND);

/// What a media track carries, declared when it enrolls via [`Segmenter::track`].
///
/// The segmenter prefers a video track as the auto-cut driver, since segment boundaries must
/// land on video keyframes to keep every rendition independently decodable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	/// Video: groups open on keyframes, the natural segment boundaries.
	Video,
	/// Audio (or any short-group track): groups pack into whatever segments exist.
	Audio,
}

/// One enrolled track's report state.
struct TrackState {
	kind: Kind,
	/// Group opens reported and not yet flushed into a record: (sequence, pts, keyframe).
	pending: VecDeque<(u64, Timestamp, bool)>,
	/// The newest reported timestamp: everything earlier is known, which is what lets a
	/// segment ending at or before it flush.
	frontier: Option<Timestamp>,
	/// The recorder was dropped; this track no longer gates completeness.
	closed: bool,
}

/// The state one [`Segmenter`] guards.
struct State {
	/// The auto-cut threshold (see [`Segmenter::with_duration_max`]).
	duration_max: Timestamp,
	/// An explicit [`cut`](Segmenter::cut) arrived: the application owns the boundaries and
	/// auto-cut stays off for the rest of the broadcast.
	manual: bool,
	/// Boundaries of segments not yet flushed: `boundaries[0]` starts segment `next_segment`,
	/// which spans to `boundaries[1]`.
	boundaries: VecDeque<Timestamp>,
	/// The newest boundary ever created (never popped), the auto-cut reference point.
	last_boundary: Option<Timestamp>,
	/// The number the next flushed record gets.
	next_segment: u64,
	/// Every enrolled track, keyed by media track name.
	tracks: BTreeMap<String, TrackState>,
	/// The auto-cut driver: the first enrolled video track, else the first enrolled track.
	driver: Option<String>,
	/// Where flushed records go once a [`Producer`] attaches; buffered until then.
	sink: Option<moq_json::stream::Producer<Record>>,
	/// Records flushed before a sink attached, drained into it on attach.
	buffered: Vec<Record>,
	/// The wire timescale for `pts`/`duration` (the catalog section's default: milliseconds).
	timescale: Timescale,
}

impl State {
	/// Register a boundary at `pts`, ignoring a non-monotonic one.
	fn cut_at(&mut self, pts: Timestamp) {
		if let Some(last) = self.last_boundary
			&& pts.as_micros() <= last.as_micros()
		{
			return;
		}
		self.boundaries.push_back(pts);
		self.last_boundary = Some(pts);
	}

	/// A group open reported by `name`: record the fact, auto-cut if this is the driver and
	/// the policy says so, and flush whatever became complete.
	fn report(&mut self, name: &str, sequence: u64, pts: Timestamp, keyframe: bool) {
		let Some(track) = self.tracks.get_mut(name) else {
			return;
		};
		track.pending.push_back((sequence, pts, keyframe));
		if track.frontier.is_none_or(|f| pts.as_micros() > f.as_micros()) {
			track.frontier = Some(pts);
		}

		// Auto-cut: only the driver paces, only at a keyframe when the driver is video (an
		// audio driver can cut anywhere), and never once the application cuts explicitly.
		if !self.manual && self.driver.as_deref() == Some(name) {
			let kind = self.tracks[name].kind;
			if keyframe || kind != Kind::Video {
				let due = match self.last_boundary {
					None => true,
					Some(last) => pts.as_micros() >= last.as_micros() + self.duration_max.as_micros(),
				};
				if due {
					self.cut_at(pts);
				}
			}
		}

		self.try_flush(false);
	}

	/// A track's recorder was dropped: stop gating completeness on it and re-elect the driver.
	fn close(&mut self, name: &str) {
		if let Some(track) = self.tracks.get_mut(name) {
			track.closed = true;
		}
		if self.driver.as_deref() == Some(name) {
			self.driver = self.elect();
		}
		self.try_flush(false);
	}

	/// The auto-cut driver among open tracks: a video track if any, else any open track.
	fn elect(&self) -> Option<String> {
		self.tracks
			.iter()
			.find(|(_, t)| !t.closed && t.kind == Kind::Video)
			.or_else(|| self.tracks.iter().find(|(_, t)| !t.closed))
			.map(|(name, _)| name.clone())
	}

	/// Flush every segment whose content is final on all tracks.
	///
	/// A segment needs its end boundary and, per open track, a report at or past it (closed
	/// tracks can't report again, so they never gate). `finished` treats every track as
	/// closed, for the terminal flush.
	fn try_flush(&mut self, finished: bool) {
		// With nothing enrolled there is nothing to describe; boundaries just wait. This also
		// keeps pre-registered cuts (e.g. an HLS import cutting ahead of its media) from
		// flushing empty records.
		if self.tracks.is_empty() {
			return;
		}

		while self.boundaries.len() >= 2 {
			let end = self.boundaries[1];
			let complete = finished
				|| self
					.tracks
					.values()
					.all(|t| t.closed || t.frontier.is_some_and(|f| f.as_micros() >= end.as_micros()));
			if !complete {
				return;
			}

			let start = self.boundaries.pop_front().expect("checked len");
			self.flush_segment(start, Some(end));
		}
	}

	/// Emit the record for the segment starting at `start`: drain every track's groups before
	/// `end` (all of them for the final, unbounded segment) into ranges.
	///
	/// Anything reported before the first boundary lands in the oldest segment: the segment's
	/// span is nominal, and early groups (a track racing ahead of the first cut) belong to it
	/// rather than to nowhere.
	fn flush_segment(&mut self, start: Timestamp, end: Option<Timestamp>) {
		let pts = start.as_scale(self.timescale) as u64;
		let duration = match end {
			Some(end) => (end.as_scale(self.timescale) as u64).saturating_sub(pts),
			// The final segment has no end boundary; its duration runs to the newest report.
			// The last group's own duration is unknown, so this is short by up to one group.
			None => self
				.tracks
				.values()
				.filter_map(|t| t.frontier)
				.map(|f| f.as_scale(self.timescale) as u64)
				.max()
				.unwrap_or(pts)
				.saturating_sub(pts),
		};

		let mut record = Record::new(self.next_segment, pts, duration);
		self.next_segment += 1;

		for (name, track) in &mut self.tracks {
			let mut ranges: Vec<Range> = Vec::new();
			while let Some(&(sequence, group_pts, keyframe)) = track.pending.front() {
				if end.is_some_and(|end| group_pts.as_micros() >= end.as_micros()) {
					break;
				}
				track.pending.pop_front();
				match ranges.last_mut() {
					// Contiguous sequences extend the run; a skip starts a new range (a gap:
					// groups that never existed).
					Some(last) if last.end + 1 == sequence => last.end = sequence,
					_ => {
						let mut range = Range::new(sequence, sequence);
						range.keyframe = keyframe;
						ranges.push(range);
					}
				}
			}
			if !ranges.is_empty() {
				record.tracks.insert(name.clone(), ranges);
			}
		}

		self.emit(record);
	}

	/// Publish a flushed record, buffering until a sink attaches.
	///
	/// The timeline is an optional sidecar, so a transport failure logs and stops publishing
	/// rather than tearing down the media path.
	fn emit(&mut self, record: Record) {
		let Some(sink) = self.sink.as_mut() else {
			self.buffered.push(record);
			return;
		};
		if let Err(err) = sink.append(&record) {
			tracing::warn!(%err, "timeline publish failed; dropping the timeline track");
			self.sink = None;
		}
	}

	/// The terminal flush: treat every track as closed, then emit the final open segment.
	fn finish(&mut self) {
		// Content but never a boundary (nobody cut and the driver never reported): anchor the
		// one segment at the earliest report so the content is still indexed.
		if self.boundaries.is_empty() {
			let first = self
				.tracks
				.values()
				.filter_map(|t| t.pending.front().map(|&(_, pts, _)| pts))
				.min_by_key(|pts| pts.as_micros());
			if let Some(first) = first {
				self.cut_at(first);
			}
		}

		self.try_flush(true);

		if let Some(start) = self.boundaries.pop_front() {
			// Skip an empty tail: a cut with no content after it describes nothing.
			if self.tracks.values().any(|t| !t.pending.is_empty()) {
				self.flush_segment(start, None);
			}
		}
	}
}

/// The broadcast's segmenter: the shared boundary list every track's groups map onto.
///
/// One per broadcast, owned by [`catalog::Producer`](crate::catalog::Producer); `Clone`
/// shares it. Media tracks enroll with [`track`](Self::track) and report group opens through
/// the returned [`Recorder`]; the application (or the auto-cut policy) declares boundaries
/// with [`cut`](Self::cut). See the [module docs](self) for the full model.
#[derive(Clone)]
pub struct Segmenter {
	state: Arc<Mutex<State>>,
}

impl Default for Segmenter {
	fn default() -> Self {
		Self::new()
	}
}

impl Segmenter {
	/// A fresh segmenter: no tracks, no boundaries, auto-cut at [`DEFAULT_DURATION_MAX`].
	pub fn new() -> Self {
		Self {
			state: Arc::new(Mutex::new(State {
				duration_max: DEFAULT_DURATION_MAX,
				manual: false,
				boundaries: VecDeque::new(),
				last_boundary: None,
				next_segment: 0,
				tracks: BTreeMap::new(),
				driver: None,
				sink: None,
				buffered: Vec::new(),
				timescale: Timescale::MILLI,
			})),
		}
	}

	/// Set the auto-cut threshold: a new segment starts at the first driver keyframe at least
	/// this far past the last boundary. Translates to HLS `EXT-X-TARGETDURATION` (though an
	/// exporter should trust the observed durations: a long GOP makes a longer segment, since
	/// a boundary can only land on a keyframe).
	///
	/// Applies to the shared state, so it also works on a clone that is already wired in.
	/// Irrelevant once [`cut`](Self::cut) is used; explicit cuts disable auto-cut.
	pub fn with_duration_max(self, duration_max: Timestamp) -> Self {
		self.state.lock().unwrap().duration_max = duration_max;
		self
	}

	/// Declare a segment boundary at `pts`: the segment before it ends, the one after starts.
	///
	/// For applications that know their boundaries (an HLS import following the source
	/// playlist, CMAF segments on disk, an encoder placing keyframes). Cuts must be
	/// monotonic; an out-of-order one is ignored. Cutting ahead of the media is fine: the
	/// segment's record still waits for every track's groups. The first explicit cut turns
	/// auto-cut off for good.
	pub fn cut(&self, pts: Timestamp) {
		let mut state = self.state.lock().unwrap();
		state.manual = true;
		state.cut_at(pts);
		state.try_flush(false);
	}

	/// Enroll the media track `name`, returning the [`Recorder`] it reports group opens
	/// through.
	///
	/// The segment records key ranges by this name, and the track gates segment completeness
	/// until its recorder drops. Enroll a track when it is about to produce (an enrolled but
	/// silent track holds every record back, by design: a segment isn't complete until every
	/// track's content is known). One recorder per track: enrolling the same name again
	/// resets its state.
	pub fn track(&self, name: &str, kind: Kind) -> Recorder {
		let mut state = self.state.lock().unwrap();
		state.tracks.insert(
			name.to_string(),
			TrackState {
				kind,
				pending: VecDeque::new(),
				frontier: None,
				closed: false,
			},
		);

		// The first video track drives auto-cut (boundaries must land on its keyframes);
		// without video, the first enrolled track of any kind paces instead.
		let promote = match &state.driver {
			None => true,
			Some(driver) => kind == Kind::Video && state.tracks.get(driver).is_none_or(|d| d.kind != Kind::Video),
		};
		if promote {
			state.driver = Some(name.to_string());
		}

		Recorder {
			state: self.state.clone(),
			name: name.to_string(),
		}
	}
}

/// Reports one media track's group opens into the shared [`Segmenter`].
///
/// Move-only: it is the track's single reporting handle, and dropping it closes the track's
/// enrollment (segments stop waiting on it). Minted by [`Segmenter::track`] and held by a
/// rendition's [`container::Producer`](crate::container::Producer).
pub struct Recorder {
	state: Arc<Mutex<State>>,
	name: String,
}

impl Recorder {
	/// Report that group `sequence` opened at presentation time `pts`, `keyframe` stating
	/// whether its first frame is one (i.e. whether a player could join here).
	///
	/// Reports must be in group order with monotonic timestamps; this is the fact the
	/// segmenter builds ranges and completeness from.
	pub(crate) fn record(&mut self, sequence: u64, pts: Timestamp, keyframe: bool) {
		self.state.lock().unwrap().report(&self.name, sequence, pts, keyframe);
	}
}

impl Drop for Recorder {
	fn drop(&mut self) {
		self.state.lock().unwrap().close(&self.name);
	}
}

/// The broadcast's timeline track: where the segmenter's records are published, plus the
/// catalog section advertising it.
///
/// `Clone`; clones share the one track and wall anchor. Get one from
/// [`catalog::Producer::timeline`](crate::catalog::Producer::timeline), which owns it and
/// finishes it with the catalog.
#[derive(Clone)]
pub struct Producer {
	state: Arc<Mutex<State>>,
	track: String,
	timescale: Timescale,
	// The wall-clock time of pts 0, in timescale units since the moq epoch, advertised in
	// section(). Shared across clones.
	wall: Arc<Mutex<Option<u64>>>,
}

impl Producer {
	/// Create the broadcast's timeline track ([`DEFAULT_NAME`]) and attach it to `segmenter`
	/// as the record sink; records flushed before this call are published immediately.
	pub fn new(broadcast: &mut moq_net::broadcast::Producer, segmenter: &Segmenter) -> Result<Self, moq_net::Error> {
		let net = broadcast.create_track(DEFAULT_NAME, None)?;
		let config = moq_json::stream::ProducerConfig::default().with_compression(true);
		let mut sink = moq_json::stream::Producer::new(net, config);

		let timescale = {
			let mut state = segmenter.state.lock().unwrap();
			for record in state.buffered.drain(..) {
				if let Err(err) = sink.append(&record) {
					tracing::warn!(%err, "timeline publish failed; dropping the buffered records");
					break;
				}
			}
			state.sink = Some(sink);
			state.timescale
		};

		Ok(Self {
			state: segmenter.state.clone(),
			track: DEFAULT_NAME.to_string(),
			timescale,
			wall: Arc::new(Mutex::new(None)),
		})
	}

	/// The catalog's root section advertising this timeline.
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
	/// 2020). Read every time the catalog republishes the section, so set it early.
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

	/// Flush the final (still open) segment and finish the track.
	pub fn finish(&mut self) -> Result<(), moq_net::Error> {
		let mut state = self.state.lock().unwrap();
		state.finish();
		// Finish the sink in place (dropping it would retire the track before late readers
		// catch up); a post-finish flush then fails in emit() and is logged there.
		let Some(sink) = state.sink.as_mut() else {
			return Ok(());
		};
		match sink.finish() {
			Ok(()) => Ok(()),
			Err(moq_json::Error::Net(err)) => Err(err),
			Err(err) => unreachable!("timeline finish failed to encode: {err}"),
		}
	}
}

/// One decoded timeline entry: a complete aligned segment with real [`Timestamp`]s and the
/// group ranges each track contributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<E: RecordExt = ()> {
	/// The segment's number, consecutive within the broadcast.
	pub segment: u64,

	/// The segment's start.
	pub pts: Timestamp,

	/// The segment's duration. The next entry starts at `pts + duration` unless content time
	/// itself jumped (a discontinuity).
	pub duration: Duration,

	/// The group ranges each participating media track contributes, keyed by track name. A
	/// track absent from the map has no content in this span (HLS `EXT-X-GAP`).
	pub tracks: BTreeMap<String, Vec<Range>>,

	/// The record's application extension (nothing for the default `()`).
	pub ext: E,
}

/// Reads a broadcast's timeline, yielding decoded [`Entry`]s in segment order.
///
/// Generic over the record extension `E` (see [`RecordExt`]).
pub struct Consumer<E: RecordExt = ()> {
	inner: moq_json::stream::Consumer<Record<E>>,
	timescale: Timescale,
}

impl<E: RecordExt> Consumer<E> {
	/// Subscribe to the timeline advertised by the catalog's [`Timeline`] section.
	///
	/// The section supplies both the track name and the timescale, so a reader can't pair the
	/// wrong scale with the track. Errors if the section declares a timescale that isn't
	/// representable.
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer, section: &Timeline) -> crate::Result<Self> {
		let track = broadcast.track(&section.track)?.subscribe(None).await?;

		let config = moq_json::stream::ConsumerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::stream::Consumer::new(track, config),
			timescale: Timescale::new(section.timescale as u64)
				.map_err(|_| crate::Error::InvalidTimescale(section.timescale))?,
		})
	}

	/// Decode a record into an entry, converting its timing out of the wire timescale.
	///
	/// A pts the timescale can't represent is an error rather than a substituted value: silently
	/// moving a timestamp would misdirect seeking and live-edge logic.
	fn decode(&self, record: Record<E>) -> crate::Result<Entry<E>> {
		let scale = self.timescale.as_u64() as u128;
		let nanos = (record.duration as u128) * 1_000_000_000 / scale;
		Ok(Entry {
			segment: record.segment,
			pts: Timestamp::new(record.pts, self.timescale)?,
			duration: Duration::from_nanos(nanos as u64),
			tracks: record.tracks,
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

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	/// Build an Entry the tests compare against.
	fn entry(segment: u64, pts_ms: u64, duration_ms: u64, tracks: &[(&str, &[(u64, u64)])]) -> Entry {
		Entry {
			segment,
			pts: ms(pts_ms),
			duration: Duration::from_millis(duration_ms),
			tracks: tracks
				.iter()
				.map(|(name, ranges)| {
					(
						name.to_string(),
						ranges.iter().map(|&(start, end)| Range::new(start, end)).collect(),
					)
				})
				.collect(),
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

	fn setup() -> (moq_net::broadcast::Producer, Segmenter, Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let segmenter = Segmenter::new();
		let producer = Producer::new(&mut broadcast, &segmenter).unwrap();
		(broadcast, segmenter, producer)
	}

	#[tokio::test]
	async fn auto_cut_paces_on_video_keyframes() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// Video keyframes every 2s (the default duration_max), audio groups every 500ms.
		// Reports interleave like a real muxer: audio runs slightly ahead.
		video.record(0, ms(0), true);
		for (seq, t) in [(0u64, 0u64), (1, 500), (2, 1_000), (3, 1_500)] {
			audio.record(seq, ms(t), true);
		}
		video.record(1, ms(2_000), true);
		for (seq, t) in [(4u64, 2_000u64), (5, 2_500), (6, 3_000), (7, 3_500)] {
			audio.record(seq, ms(t), true);
		}
		video.record(2, ms(4_000), true);
		audio.record(8, ms(4_000), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		// Segment 0 flushes only once BOTH tracks reported past 2s; every record is
		// self-contained (start AND end groups, explicit duration).
		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 3)])]),
				entry(1, 2_000, 2_000, &[("video0", &[(1, 1)]), ("audio0", &[(4, 7)])]),
				entry(2, 4_000, 0, &[("video0", &[(2, 2)]), ("audio0", &[(8, 8)])]),
			]
		);
	}

	#[tokio::test]
	async fn a_segment_waits_for_every_track() {
		let (broadcast, segmenter, timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// Video is two boundaries ahead, but audio hasn't crossed the first one: nothing
		// may flush, because audio's groups for segment 0 aren't final yet.
		video.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		video.record(2, ms(4_000), true);
		audio.record(0, ms(0), true);
		let section = timeline.section();
		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), &section).await.unwrap();
		let waiter = kio::Waiter::noop();
		assert!(consumer.poll_next(&waiter).is_pending());

		// Audio crossing the second boundary finalizes segment 0 (and only segment 0).
		audio.record(1, ms(2_100), true);
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(got))) => {
				assert_eq!(got, entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 0)])]))
			}
			other => panic!("expected segment 0, got {other:?}"),
		}
		assert!(consumer.poll_next(&waiter).is_pending());
	}

	#[tokio::test]
	async fn explicit_cuts_disable_auto_cut() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);

		// The app cuts every 6s (e.g. following a source playlist); video keyframes every 2s.
		// Auto-cut (duration_max 2s) must not fire between the explicit cuts.
		segmenter.cut(ms(0));
		segmenter.cut(ms(6_000));
		for (seq, t) in [(0u64, 0u64), (1, 2_000), (2, 4_000), (3, 6_000), (4, 8_000)] {
			video.record(seq, ms(t), true);
		}
		drop(video);
		timeline.finish().unwrap();

		// One multi-GOP segment per cut: tiny GOPs pack into larger segments.
		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 6_000, &[("video0", &[(0, 2)])]),
				entry(1, 6_000, 2_000, &[("video0", &[(3, 4)])]),
			]
		);
	}

	#[tokio::test]
	async fn cutting_ahead_of_the_media_waits_for_it() {
		let (broadcast, segmenter, mut timeline) = setup();

		// An HLS import pre-registers boundaries from the playlist before any media arrives.
		segmenter.cut(ms(0));
		segmenter.cut(ms(6_000));
		segmenter.cut(ms(12_000));

		let mut video = segmenter.track("video0", Kind::Video);
		let section = timeline.section();
		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), &section).await.unwrap();
		let waiter = kio::Waiter::noop();
		assert!(consumer.poll_next(&waiter).is_pending(), "no media, no records");

		for (seq, t) in [(0u64, 0u64), (1, 3_000), (2, 6_000), (3, 9_000)] {
			video.record(seq, ms(t), true);
		}
		drop(video);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 6_000, &[("video0", &[(0, 1)])]),
				entry(1, 6_000, 6_000, &[("video0", &[(2, 3)])]),
			]
		);
	}

	#[tokio::test]
	async fn audio_only_paces_itself() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// No video: the first (audio) track drives, cutting every duration_max (2s).
		for (seq, t) in [(0u64, 0u64), (1, 500), (2, 1_000), (3, 1_500), (4, 2_000), (5, 2_500)] {
			audio.record(seq, ms(t), true);
		}
		drop(audio);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("audio0", &[(0, 3)])]),
				entry(1, 2_000, 500, &[("audio0", &[(4, 5)])]),
			]
		);
	}

	#[tokio::test]
	async fn video_enrolling_late_takes_over_the_pacing() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut audio = segmenter.track("audio0", Kind::Audio);
		audio.record(0, ms(0), true);
		audio.record(1, ms(2_000), true); // audio-driven boundary at 2s

		// Video joins: it becomes the driver, and the next boundary lands on ITS keyframe.
		let mut video = segmenter.track("video0", Kind::Video);
		video.record(0, ms(2_100), true);
		video.record(1, ms(4_100), true);
		audio.record(2, ms(4_200), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("audio0", &[(0, 0)])]),
				entry(1, 2_000, 2_100, &[("audio0", &[(1, 1)]), ("video0", &[(0, 0)])]),
				entry(2, 4_100, 100, &[("audio0", &[(2, 2)]), ("video0", &[(1, 1)])]),
			]
		);
	}

	#[tokio::test]
	async fn groups_before_the_first_boundary_join_the_first_segment() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// Audio races ahead of video's first keyframe (the startup race): its early groups
		// belong to segment 0, not to nowhere.
		audio.record(0, ms(0), true);
		video.record(0, ms(30), true);
		video.record(1, ms(2_030), true);
		audio.record(1, ms(2_100), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 30, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 0)])]),
				entry(1, 2_030, 70, &[("video0", &[(1, 1)]), ("audio0", &[(1, 1)])]),
			]
		);
	}

	#[tokio::test]
	async fn sequence_gaps_split_ranges() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// Elemental-style gap: audio groups 2..=4 never existed inside segment 0.
		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		audio.record(1, ms(300), true);
		audio.record(5, ms(1_500), true);
		video.record(1, ms(2_000), true);
		audio.record(6, ms(2_100), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries[0],
			entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 1), (5, 5)])]),
		);
	}

	#[tokio::test]
	async fn a_whole_segment_gap_omits_the_track() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		// Audio drops out for all of segment 1 and returns in segment 2: the record simply
		// has no audio entry (an HLS exporter renders EXT-X-GAP).
		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		video.record(2, ms(4_000), true);
		audio.record(1, ms(4_500), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries[1], entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]));
		assert_eq!(
			entries[2],
			entry(2, 4_000, 500, &[("video0", &[(2, 2)]), ("audio0", &[(1, 1)])])
		);
	}

	#[tokio::test]
	async fn a_non_keyframe_range_start_is_flagged() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);

		segmenter.cut(ms(0));
		segmenter.cut(ms(2_000));
		video.record(0, ms(0), true);
		// A gappy source resumes without an IDR: the range says so, and auto-cut would not
		// have cut here anyway (explicit cuts are in charge).
		video.record(1, ms(2_500), false);
		video.record(2, ms(4_000), true);
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		let ranges = &entries[1].tracks["video0"];
		assert_eq!((ranges[0].start, ranges[0].end, ranges[0].keyframe), (1, 2, false));
	}

	#[tokio::test]
	async fn a_closed_track_stops_gating() {
		let (broadcast, segmenter, mut timeline) = setup();
		let mut video = segmenter.track("video0", Kind::Video);
		let mut audio = segmenter.track("audio0", Kind::Audio);

		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		video.record(2, ms(4_000), true);

		// Audio dies mid-broadcast. Its recorder dropping is what unblocks segment 0 (and 1):
		// completeness can't wait forever on a track that will never report again.
		drop(audio);
		drop(video);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 0)])]),
				entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]),
				entry(2, 4_000, 0, &[("video0", &[(2, 2)])]),
			]
		);
	}

	#[tokio::test]
	async fn records_flushed_before_the_track_exists_are_buffered() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let segmenter = Segmenter::new();

		// Segments complete before any timeline track exists (e.g. the catalog wires the
		// track lazily): they publish the moment it attaches.
		let mut audio = segmenter.track("audio0", Kind::Audio);
		for (seq, t) in [(0u64, 0u64), (1, 1_000), (2, 2_000), (3, 3_000), (4, 4_000)] {
			audio.record(seq, ms(t), true);
		}

		let mut timeline = Producer::new(&mut broadcast, &segmenter).unwrap();
		drop(audio);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries[0], entry(0, 0, 2_000, &[("audio0", &[(0, 1)])]));
		assert_eq!(entries.len(), 3);
	}

	#[test]
	fn section_advertises_track_and_wall() {
		let (_broadcast, _segmenter, mut timeline) = setup();

		let section = timeline.section();
		assert_eq!(section.track, "timeline.z");
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
		let (broadcast, _segmenter, mut timeline) = setup();
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
		let segmenter = Segmenter::new();
		let timeline = Producer::new(&mut broadcast, &segmenter).unwrap();

		// Publish a record whose pts no Timestamp can hold, bypassing the segmenter.
		let track = broadcast.create_track("raw.timeline.z", None).unwrap();
		let config = moq_json::stream::ProducerConfig::default().with_compression(true);
		let mut raw = moq_json::stream::Producer::new(track, config);
		raw.append(&Record::<()>::new(0, u64::MAX, 0)).unwrap();
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
}

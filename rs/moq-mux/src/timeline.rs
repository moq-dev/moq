//! Per-track timelines: each track's index of spans, which HLS/DASH export and recordings use.
//!
//! Every indexed track gets its own timeline track carrying one [`hang::timeline::Record`] per
//! *span*: a run of the track's frames mapped to their content time. A consumer can answer "which
//! groups cover time T on track X" and "where is the live edge" from a few bytes per span
//! without subscribing to media, which is the primitive a playlist server (HLS/DASH), a seek bar,
//! or a recorder index needs.
//!
//! ## Cutting
//!
//! Tracks cut independently; nothing waits for another track.
//!
//! - A record closes at the first group start at least [`Config::duration_min`] past its own start,
//!   so short groups (audio) pack into one record and long ones (video GOPs) get one each.
//! - With a zero minimum every group is its own record, closed as soon as the group finishes. That
//!   suits a sparse track such as a catalog, which may not publish again for the rest of the
//!   broadcast.
//! - A group still open [`Config::duration_max`] past the record's start is split at the next
//!   reported frame, so an append-only group that never closes is still indexed as it grows.
//! - A skipped group sequence always closes the record, so a record's groups are contiguous.
//! - [`Segmenter::cut`] adds an application boundary, such as a video keyframe cutting audio so a
//!   derived segment needs fewer objects. The first cut takes over from minimum-duration pacing.
//!
//! ## Wiring
//!
//! [`catalog::Producer`](crate::catalog::Producer) owns the broadcast's [`Timelines`] and advertises
//! them in the catalog's root [`hang::catalog::Archive`] entry. Its role-specific track
//! constructors enroll each media track, and the catalog enrolls itself as a sparse track.
//!
//! A [`Segmenter`] builds records from a track's frame reports without publishing them, and a
//! [`Producer`] publishes records onto a timeline track. A [`Recorder`] pairs the two for live
//! publishing; a recording writer drives them separately so it can store the media a record names
//! before publishing it. On the read side, [`Consumer::subscribe`] reads one track's timeline from
//! the catalog's [`Archive`] entry and yields decoded [`Event`]s. On the wire each timeline is a
//! DEFLATE-compressed [`moq_json::window`], so a DVR can trim old records while an unbounded
//! timeline simply never pops them.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use hang::catalog::Archive;
use hang::timeline::{Position, Record, RecordExt};
use moq_json::window::Checkpoint;

use moq_net::{Timescale, Timestamp};

/// The conventional [`Config::duration_min`] (1 second), for callers with no opinion.
pub const DEFAULT_DURATION_MIN: Duration = Duration::from_secs(1);

/// The conventional [`Config::duration_max`] (10 seconds), for callers with no opinion.
pub const DEFAULT_DURATION_MAX: Duration = Duration::from_secs(10);

/// Recent records repeated when the window track rolls to a new group.
const CHECKPOINT_RECORDS: usize = 256;

/// How a track's records are cut.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The shortest a record may be before a group start closes it.
	///
	/// A floor rather than a target on purpose: a floor is always satisfiable (wait for the next
	/// group), so no group is split to honor it. Zero makes every group its own record.
	pub duration_min: Duration,

	/// The longest a record may run before a frame inside a group splits it.
	///
	/// Advertised in the catalog, so a consumer can size an HLS `EXT-X-TARGETDURATION` up front.
	pub duration_max: Duration,
}

impl Config {
	/// Set [`duration_min`](Self::duration_min).
	pub fn with_duration_min(mut self, duration: Duration) -> Self {
		self.duration_min = duration;
		self
	}

	/// Set [`duration_max`](Self::duration_max).
	pub fn with_duration_max(mut self, duration: Duration) -> Self {
		self.duration_max = duration;
		self
	}

	/// The catalog section advertising timelines cut by this config, with no timelines yet.
	pub fn section(&self) -> Archive {
		let mut section = Archive::new();
		section.timescale = TIMESCALE.as_u64() as u32;
		section.duration_max = Some(units(self.duration_max));
		section
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			duration_min: DEFAULT_DURATION_MIN,
			duration_max: DEFAULT_DURATION_MAX,
		}
	}
}

/// The wire timescale for record `pts`/`duration`: the catalog section's default, milliseconds.
const TIMESCALE: Timescale = Timescale::MILLI;

/// A duration in wire units, rounded up so a bound never understates itself.
fn units(duration: Duration) -> u64 {
	(duration.as_micros() * TIMESCALE.as_u64() as u128).div_ceil(1_000_000) as u64
}

fn wire(pts: Timestamp) -> u64 {
	pts.as_scale(TIMESCALE) as u64
}

/// The record being built.
struct Open {
	start: Position,
	pts: Timestamp,
	keyframe: bool,
}

/// Builds one track's records from reports of the frames it published.
///
/// Reports arrive in position order: [`frame`](Self::frame) for each frame that may start a record
/// (every frame, or at least the first of each group), [`finish_group`](Self::finish_group) once a
/// group can gain no more frames, and [`end`](Self::end) wherever the content is known to stop.
/// Closed records queue until [`Iterator::next`] takes them.
pub struct Segmenter {
	config: Config,
	/// The number the next closed record gets.
	sequence: u64,
	open: Option<Open>,
	/// The newest reported frame position.
	last: Option<Position>,
	/// Whether the group of `last` has finished.
	finished: bool,
	/// The newest reported content time in the open record.
	frontier: Option<Timestamp>,
	/// Application boundaries not yet reached, in order.
	cuts: VecDeque<Timestamp>,
	/// A cut arrived, so the application owns the group boundaries from here on.
	manual: bool,
	ready: VecDeque<Record>,
	closed: bool,
}

impl Segmenter {
	/// A segmenter numbering its records from zero.
	pub fn new(config: Config) -> Self {
		Self {
			config,
			sequence: 0,
			open: None,
			last: None,
			finished: false,
			frontier: None,
			cuts: VecDeque::new(),
			manual: false,
			ready: VecDeque::new(),
			closed: false,
		}
	}

	/// Number the next record `sequence`, continuing a resumed timeline.
	pub fn with_sequence(mut self, sequence: u64) -> Self {
		self.sequence = sequence;
		self
	}

	/// The number the next closed record gets.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Report the frame at `position`, presented at `pts`.
	///
	/// A report at or before the previous position is ignored: records never overlap.
	pub fn frame(&mut self, position: Position, pts: Timestamp, keyframe: bool) {
		if self.closed {
			return;
		}
		if let Some(last) = self.last
			&& position <= last
		{
			tracing::warn!(?position, ?last, "ignoring a timeline report that does not advance");
			return;
		}

		if let (Some(start), Some(last)) = (self.open.as_ref().map(|open| open.pts), self.last) {
			let elapsed = pts.as_micros().saturating_sub(start.as_micros());
			let end = if position.group != last.group {
				let contiguous = position.group == last.group + 1 && position.frame == 0;
				// A skipped sequence or a group joined mid-way always closes, so a record's groups are
				// contiguous and the reader never has to guess what lies between.
				let boundary = !contiguous
					|| elapsed >= self.config.duration_max.as_micros()
					|| self.boundary(start, pts, elapsed);
				boundary.then_some(Position::group(last.group + 1))
			} else {
				(elapsed >= self.config.duration_max.as_micros()).then_some(position)
			};
			if let Some(end) = end {
				self.emit(end, pts);
			}
		}

		if self.open.is_none() {
			self.open = Some(Open {
				start: position,
				pts,
				keyframe,
			});
			self.frontier = None;
		}
		self.last = Some(position);
		self.finished = false;
		self.advance(pts);
	}

	/// Whether a group starting at `pts` ends the record that started at `start`.
	fn boundary(&mut self, start: Timestamp, pts: Timestamp, elapsed: u128) -> bool {
		while self
			.cuts
			.front()
			.is_some_and(|cut| cut.as_micros() <= start.as_micros())
		{
			self.cuts.pop_front();
		}
		if self.cuts.front().is_some_and(|cut| cut.as_micros() <= pts.as_micros()) {
			while self.cuts.front().is_some_and(|cut| cut.as_micros() <= pts.as_micros()) {
				self.cuts.pop_front();
			}
			return true;
		}
		!self.manual && elapsed >= self.config.duration_min.as_micros()
	}

	/// Report that group `group` can gain no more frames.
	///
	/// With a zero [`Config::duration_min`] this closes the record, so a sparse track's newest
	/// group is indexed without waiting for the next one.
	pub fn finish_group(&mut self, group: u64) {
		if self.closed || self.last.is_none_or(|last| last.group != group) {
			return;
		}
		self.finished = true;
		if self.config.duration_min.is_zero() && self.open.is_some() {
			let pts = self.frontier.expect("an open record has a frontier");
			self.emit(Position::group(group + 1), pts);
		}
	}

	/// Report that the content extends to `pts`, without a frame starting there.
	///
	/// A frame report says where content starts; the last record has no successor to bound it, so
	/// its duration would otherwise stop at its last frame's start.
	pub fn end(&mut self, pts: Timestamp) {
		if !self.closed && self.open.is_some() {
			self.advance(pts);
		}
	}

	fn advance(&mut self, pts: Timestamp) {
		if self
			.frontier
			.is_none_or(|frontier| pts.as_micros() > frontier.as_micros())
		{
			self.frontier = Some(pts);
		}
	}

	/// Declare a boundary at `pts`: the record closes at the first group starting at or after it.
	///
	/// The first cut takes over from [`Config::duration_min`] pacing for good, since pacing would
	/// otherwise close a record just before the caller declares where it really ends. A cut at or
	/// before an earlier one is ignored, so several producers declaring the same boundaries (the
	/// renditions of one import) cost nothing.
	pub fn cut(&mut self, pts: Timestamp) {
		if self.closed {
			return;
		}
		self.manual = true;
		if self.cuts.back().is_none_or(|back| pts.as_micros() > back.as_micros()) {
			self.cuts.push_back(pts);
		}
	}

	/// Close the open record now, at the end of the newest reported frame.
	///
	/// The record ends after the newest group when that group finished, otherwise after the newest
	/// reported frame. Later reports start a new record.
	pub fn flush(&mut self) {
		let (Some(last), Some(frontier)) = (self.last, self.frontier) else {
			return;
		};
		if self.open.is_none() {
			return;
		}
		let end = match self.finished {
			true => Position::group(last.group + 1),
			false => Position::new(last.group, last.frame + 1),
		};
		self.emit(end, frontier);
	}

	/// Flush, then ignore every later report.
	pub fn close(&mut self) {
		self.flush();
		self.closed = true;
	}

	/// Close the open record at `end`, its content running to `pts`.
	fn emit(&mut self, end: Position, pts: Timestamp) {
		let Some(open) = self.open.take() else {
			return;
		};
		let start = wire(open.pts);
		let duration = wire(pts).saturating_sub(start);
		let mut record = Record::new(self.sequence, start, duration, open.start, end);
		record.keyframe = open.keyframe;
		self.sequence += 1;
		self.ready.push_back(record);
	}
}

/// Yields each closed record once; `None` means none is ready yet, not that none ever will be.
impl Iterator for Segmenter {
	type Item = Record;

	fn next(&mut self) -> Option<Record> {
		self.ready.pop_front()
	}
}

/// Publishes one track's records onto its timeline track.
///
/// Records must be pushed in sequence order, starting at the window's next index.
pub struct Producer {
	sink: moq_json::window::Producer<Record>,
}

impl Producer {
	/// The properties a timeline track is created with.
	pub fn info() -> moq_net::track::Info {
		moq_net::track::Info::default().with_priority(hang::catalog::PRIORITY.catalog)
	}

	/// Publish onto `track`, starting at record zero.
	pub fn new(track: moq_net::track::Producer) -> Self {
		Self {
			sink: moq_json::window::Producer::new(track, Self::config()),
		}
	}

	/// Publish onto `track`, continuing `checkpoint`, such as a window recovered from storage.
	///
	/// The track's first group restates the checkpoint, and the next record is sequence
	/// `checkpoint.range.end`. Fails when a checkpoint record is not the sequence at its index or the
	/// checkpoint is malformed.
	pub fn resume(track: moq_net::track::Producer, checkpoint: &Checkpoint<Record>) -> crate::Result<Self> {
		let sink = moq_json::window::Producer::resume(track, Self::config(), checkpoint).map_err(json)?;
		// The encoder accepted the checkpoint, so the records fit before `range.end`.
		let start = checkpoint.range.end - checkpoint.records.len() as u64;
		for (index, record) in (start..).zip(&checkpoint.records) {
			if record.sequence != index {
				return Err(crate::Error::TimelineCheckpoint(index));
			}
		}
		Ok(Self { sink })
	}

	fn config() -> moq_json::window::ProducerConfig {
		moq_json::window::ProducerConfig::default()
			.with_compression(true)
			.with_checkpoint_records(CHECKPOINT_RECORDS)
	}

	/// The window indices currently retained.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.sink.range()
	}

	/// Append `record`, which must be the window's next sequence.
	pub fn push(&mut self, record: &Record) -> crate::Result<()> {
		let next = self.sink.range().end;
		if record.sequence != next {
			return Err(crate::Error::TimelineSequence {
				expected: next,
				actual: record.sequence,
			});
		}
		self.sink.push(record).map_err(json)
	}

	/// Remove up to `count` oldest records from the window.
	pub fn pop(&mut self, count: u64) -> crate::Result<()> {
		self.sink.pop(count).map_err(json)
	}

	/// Close the track's open group, so every push and pop so far sits in a complete group.
	///
	/// A recorder stores those groups before committing the next record. The next edit opens a
	/// new group restating the window.
	pub fn flush(&mut self) -> crate::Result<()> {
		self.sink.cut().map_err(json)
	}

	/// Finish the timeline track.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.sink.finish().map_err(json)
	}
}

/// Surface a transport failure as one, rather than wrapped in the JSON layer.
fn json(err: moq_json::Error) -> crate::Error {
	match err {
		moq_json::Error::Net(err) => err.into(),
		err => err.into(),
	}
}

/// A live track's segmenter and the timeline it publishes to.
struct Live {
	segmenter: Segmenter,
	/// `None` once a publish failed: the timeline is an optional sidecar, so it stops instead.
	output: Option<Producer>,
}

impl Live {
	fn publish(&mut self) {
		for record in self.segmenter.by_ref() {
			let Some(output) = self.output.as_mut() else {
				continue;
			};
			if let Err(err) = output.push(&record) {
				tracing::warn!(%err, "timeline publish failed; dropping the timeline track");
				let _ = output.finish();
				self.output = None;
			}
		}
	}

	fn finish(&mut self) {
		self.segmenter.close();
		self.publish();
		if let Some(output) = self.output.as_mut() {
			let _ = output.finish();
		}
	}
}

struct Registry {
	/// Each enrolled track's timeline, by track name. Kept after its recorder drops, so a finished
	/// span stays discoverable and a re-enrolled track continues its numbering.
	tracks: BTreeMap<String, (String, Arc<Mutex<Live>>)>,
	finished: bool,
}

/// A broadcast's timelines, one per enrolled track.
///
/// Each enrollment creates the track's timeline track and returns the [`Recorder`] its producer
/// reports through. `Clone` shares the set.
#[derive(Clone)]
pub struct Timelines {
	broadcast: moq_net::broadcast::Producer,
	config: Config,
	registry: Arc<Mutex<Registry>>,
}

impl Timelines {
	/// Timelines for tracks of `broadcast`, cut by `config`.
	pub fn new(broadcast: &moq_net::broadcast::Producer, config: Config) -> Self {
		// The contents are `Send + Sync` natively; on wasm moq-net's handles are `Rc`-backed, so
		// clippy sees a pointlessly atomic `Arc`. One type for both targets is worth it.
		#[allow(clippy::arc_with_non_send_sync)]
		let registry = Arc::new(Mutex::new(Registry {
			tracks: BTreeMap::new(),
			finished: false,
		}));
		Self {
			broadcast: broadcast.clone(),
			config,
			registry,
		}
	}

	/// Enroll the media track `name`, cut by the configured durations.
	///
	/// Creates its timeline track, named by [`hang::timeline::default_name`], on first enrollment.
	/// Enrolling a name again continues its timeline's numbering for a new producer. Errors when
	/// the timeline track name is taken or the timelines finished.
	pub fn track(&self, name: &str) -> crate::Result<Recorder> {
		self.enroll(name, self.config.clone())
	}

	/// Enroll the sparse track `name`, such as a catalog: every group is its own record.
	pub fn sparse(&self, name: &str) -> crate::Result<Recorder> {
		self.enroll(name, self.config.clone().with_duration_min(Duration::ZERO))
	}

	fn enroll(&self, name: &str, config: Config) -> crate::Result<Recorder> {
		let mut registry = self.registry.lock().unwrap();
		if registry.finished {
			return Err(moq_net::Error::Closed.into());
		}
		if let Some((_, existing)) = registry.tracks.get(name) {
			// A re-enrolled track is a new producer whose group sequences may restart, so it gets a
			// fresh segmenter continuing the record numbering.
			let mut live = existing.lock().unwrap();
			live.segmenter.flush();
			live.publish();
			let sequence = live.segmenter.sequence();
			live.segmenter = Segmenter::new(config).with_sequence(sequence);
			drop(live);
			return Ok(Recorder { live: existing.clone() });
		}

		let timeline = hang::timeline::default_name(name);
		let track = self.broadcast.create_track(timeline.as_str(), Producer::info())?;
		#[allow(clippy::arc_with_non_send_sync)]
		let live = Arc::new(Mutex::new(Live {
			segmenter: Segmenter::new(config),
			output: Some(Producer::new(track)),
		}));
		registry.tracks.insert(name.to_string(), (timeline, live.clone()));
		Ok(Recorder { live })
	}

	/// Declare a boundary at `pts` on every enrolled track; see [`Segmenter::cut`].
	pub fn cut(&self, pts: Timestamp) {
		let registry = self.registry.lock().unwrap();
		for (_, live) in registry.tracks.values() {
			let mut live = live.lock().unwrap();
			live.segmenter.cut(pts);
			live.publish();
		}
	}

	/// The catalog's root `archive` entry advertising every enrolled track's timeline.
	pub fn section(&self) -> Archive {
		let registry = self.registry.lock().unwrap();
		let mut section = self.config.section();
		section.timelines = registry
			.tracks
			.iter()
			.map(|(name, (timeline, _))| (name.clone(), timeline.clone()))
			.collect();
		section
	}

	/// Flush every open record and finish every timeline track. Later enrollments fail.
	pub fn finish(&self) {
		let mut registry = self.registry.lock().unwrap();
		registry.finished = true;
		for (_, live) in registry.tracks.values() {
			live.lock().unwrap().finish();
		}
	}
}

/// Reports one track's frames into its live timeline.
///
/// Move-only: it is the track's reporting handle, minted by [`Timelines::track`] and held by a
/// rendition's [`container::Producer`](crate::container::Producer). Dropping it flushes the open
/// record; the timeline track stays open until [`Timelines::finish`].
pub struct Recorder {
	live: Arc<Mutex<Live>>,
}

impl Recorder {
	fn report(&mut self, f: impl FnOnce(&mut Segmenter)) {
		let mut live = self.live.lock().unwrap();
		f(&mut live.segmenter);
		live.publish();
	}

	/// Report the frame at `position`, presented at `pts`; see [`Segmenter::frame`].
	pub fn frame(&mut self, position: Position, pts: Timestamp, keyframe: bool) {
		self.report(|segmenter| segmenter.frame(position, pts, keyframe));
	}

	/// Report that group `group` can gain no more frames; see [`Segmenter::finish_group`].
	pub fn finish_group(&mut self, group: u64) {
		self.report(|segmenter| segmenter.finish_group(group));
	}

	/// Report that the content extends to `pts`; see [`Segmenter::end`].
	pub fn end(&mut self, pts: Timestamp) {
		self.report(|segmenter| segmenter.end(pts));
	}

	/// Declare a boundary at `pts` on this track; see [`Segmenter::cut`].
	pub fn cut(&mut self, pts: Timestamp) {
		self.report(|segmenter| segmenter.cut(pts));
	}
}

impl Drop for Recorder {
	fn drop(&mut self) {
		self.report(Segmenter::flush);
	}
}

/// One decoded timeline record, with real [`Timestamp`]s.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<E: RecordExt = ()> {
	/// The record's number, consecutive within its track's timeline.
	pub sequence: u64,

	/// The span's start.
	pub pts: Timestamp,

	/// The span's duration. The next entry starts at `pts + duration` unless content time itself
	/// jumped (a discontinuity).
	pub duration: Duration,

	/// The first frame of the span, inclusive.
	pub start: Position,

	/// The end of the span, exclusive.
	pub end: Position,

	/// Whether the span's first frame is a keyframe.
	pub keyframe: bool,

	/// The record's application extension (nothing for the default `()`).
	pub ext: E,
}

impl<E: RecordExt> Entry<E> {
	/// The span's exclusive end time.
	pub fn end_time(&self) -> Duration {
		Duration::from(self.pts) + self.duration
	}
}

/// One change to the visible timeline window.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event<E: RecordExt = ()> {
	/// A record became visible at this window index, which equals its sequence.
	Push {
		/// Absolute window index assigned to the record.
		index: u64,
		/// The decoded record.
		entry: Entry<E>,
	},
	/// These previously visible window indices were removed.
	Pop(std::ops::Range<u64>),
	/// These indices were removed before this consumer saw them.
	Skip(std::ops::Range<u64>),
}

/// Reads one track's timeline, yielding decoded [`Event`]s in window order.
///
/// Generic over the record extension `E` (see [`RecordExt`]).
pub struct Consumer<E: RecordExt = ()> {
	inner: moq_json::window::Consumer<Record<E>>,
	timescale: Timescale,
}

impl<E: RecordExt> Consumer<E> {
	/// Subscribe to `track`'s timeline as advertised by the catalog's [`Archive`] entry.
	///
	/// The section supplies both the timeline name and the timescale, so a reader can't pair the
	/// wrong scale with the track. Errors if the section indexes no such track or declares a
	/// timescale that isn't representable.
	pub async fn subscribe(
		broadcast: &moq_net::broadcast::Consumer,
		section: &Archive,
		track: &str,
	) -> crate::Result<Self> {
		let name = section
			.timelines
			.get(track)
			.ok_or_else(|| crate::Error::TimelineMissing(track.to_string()))?;
		let timescale =
			Timescale::new(section.timescale as u64).map_err(|_| crate::Error::InvalidTimescale(section.timescale))?;
		let track = broadcast.track(name)?.subscribe(None).await?;
		let config = moq_json::window::ConsumerConfig::default().with_compression(true);
		Ok(Self {
			inner: moq_json::window::Consumer::new(track, config),
			timescale,
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
			sequence: record.sequence,
			pts: Timestamp::new(record.pts, self.timescale)?,
			duration: Duration::from_nanos(nanos as u64),
			start: record.start,
			end: record.end,
			keyframe: record.keyframe,
			ext: record.ext,
		})
	}

	fn decode_event(&self, event: moq_json::window::Event<Record<E>>) -> crate::Result<Event<E>> {
		Ok(match event {
			moq_json::window::Event::Push { index, value } => Event::Push {
				index,
				entry: self.decode(value)?,
			},
			moq_json::window::Event::Pop(range) => Event::Pop(range),
			moq_json::window::Event::Skip(range) => Event::Skip(range),
			_ => unreachable!("unknown timeline window event"),
		})
	}

	/// Get the next window event, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Event<E>>> {
		match self.inner.next().await? {
			Some(event) => Ok(Some(self.decode_event(event)?)),
			None => Ok(None),
		}
	}

	/// Poll for the next window event, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event<E>>>> {
		match self.inner.poll_next(waiter)? {
			Poll::Ready(Some(event)) => Poll::Ready(self.decode_event(event).map(Some)),
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	fn at(group: u64) -> Position {
		Position::group(group)
	}

	/// A record's `(pts, duration, start, end)` in wire units, for compact comparisons.
	fn span(record: &Record) -> (u64, u64, Position, Position) {
		(record.pts, record.duration, record.start, record.end)
	}

	fn drain(segmenter: &mut Segmenter) -> Vec<(u64, u64, Position, Position)> {
		let mut out = Vec::new();
		let mut sequence = None;
		for record in segmenter.by_ref() {
			if let Some(previous) = sequence {
				assert_eq!(record.sequence, previous + 1, "records are numbered consecutively");
			}
			sequence = Some(record.sequence);
			out.push(span(&record));
		}
		out
	}

	/// One frame per group, one group every `step` ms.
	fn groups(segmenter: &mut Segmenter, groups: std::ops::Range<u64>, step: u64) {
		for group in groups {
			segmenter.frame(at(group), ms(group * step), true);
			segmenter.finish_group(group);
		}
	}

	#[test]
	fn long_groups_get_a_record_each() {
		let mut video = Segmenter::new(Config::default());
		groups(&mut video, 0..3, 2_000);
		video.end(ms(6_000));
		video.close();

		assert_eq!(
			drain(&mut video),
			vec![
				(0, 2_000, at(0), at(1)),
				(2_000, 2_000, at(1), at(2)),
				(4_000, 2_000, at(2), at(3)),
			]
		);
	}

	#[test]
	fn short_groups_pack_up_to_the_minimum() {
		let mut audio = Segmenter::new(Config::default().with_duration_min(Duration::from_millis(1_500)));
		groups(&mut audio, 0..8, 500);
		audio.close();

		assert_eq!(
			drain(&mut audio),
			vec![
				(0, 1_500, at(0), at(3)),
				(1_500, 1_500, at(3), at(6)),
				(3_000, 500, at(6), at(8)),
			]
		);
	}

	#[test]
	fn a_long_group_splits_by_frame_at_the_maximum() {
		let mut video = Segmenter::new(Config::default().with_duration_max(Duration::from_secs(3)));
		for frame in 0..8 {
			video.frame(Position::new(0, frame), ms(frame * 1_000), frame == 0);
		}
		video.close();

		let records: Vec<Record> = std::iter::from_fn(|| video.next()).collect();
		assert_eq!(
			records.iter().map(span).collect::<Vec<_>>(),
			vec![
				(0, 3_000, Position::new(0, 0), Position::new(0, 3)),
				(3_000, 3_000, Position::new(0, 3), Position::new(0, 6)),
				(6_000, 1_000, Position::new(0, 6), Position::new(0, 8)),
			]
		);
		assert!(records[0].keyframe);
		assert!(
			!records[1].keyframe,
			"a split inside a group does not start on a keyframe"
		);
	}

	#[test]
	fn a_sparse_track_records_each_group_when_it_finishes() {
		let mut catalog = Segmenter::new(Config::default().with_duration_min(Duration::ZERO));
		catalog.frame(at(0), ms(0), true);
		assert!(catalog.next().is_none(), "the group may still grow");
		catalog.finish_group(0);
		assert_eq!(drain(&mut catalog), vec![(0, 0, at(0), at(1))]);

		catalog.frame(at(1), ms(60_000), true);
		catalog.finish_group(1);
		assert_eq!(drain(&mut catalog), vec![(60_000, 0, at(1), at(2))]);
	}

	#[test]
	fn an_append_only_group_is_indexed_as_it_grows() {
		// A never-closing log: one frame a second, split every three seconds.
		let mut log = Segmenter::new(
			Config::default()
				.with_duration_min(Duration::ZERO)
				.with_duration_max(Duration::from_secs(3)),
		);
		for frame in 0..7 {
			log.frame(Position::new(0, frame), ms(frame * 1_000), true);
		}
		assert_eq!(
			drain(&mut log),
			vec![
				(0, 3_000, Position::new(0, 0), Position::new(0, 3)),
				(3_000, 3_000, Position::new(0, 3), Position::new(0, 6)),
			]
		);

		// The tail ends after the newest frame, since the group never finished.
		log.close();
		assert_eq!(
			drain(&mut log),
			vec![(6_000, 0, Position::new(0, 6), Position::new(0, 7))]
		);
	}

	#[test]
	fn a_skipped_sequence_closes_the_record() {
		let mut audio = Segmenter::new(Config::default());
		audio.frame(at(0), ms(0), true);
		audio.frame(at(1), ms(300), true);
		audio.frame(at(5), ms(600), true);
		audio.frame(at(6), ms(1_700), true);
		audio.close();

		assert_eq!(
			drain(&mut audio),
			vec![
				(0, 600, at(0), at(2)),
				(600, 1_100, at(5), at(6)),
				(1_700, 0, at(6), Position::new(6, 1))
			]
		);
	}

	#[test]
	fn explicit_cuts_override_the_pacing() {
		let mut video = Segmenter::new(Config::default());
		// Keyframes every second, cut every three: the records follow the cuts, not the GOPs.
		video.cut(ms(3_000));
		video.cut(ms(3_000));
		video.cut(ms(6_000));
		groups(&mut video, 0..7, 1_000);

		assert_eq!(
			drain(&mut video),
			vec![(0, 3_000, at(0), at(3)), (3_000, 3_000, at(3), at(6))]
		);
	}

	#[test]
	fn a_cut_on_the_first_group_does_not_poison_later_cuts() {
		let mut video = Segmenter::new(Config::default());
		video.cut(ms(0));
		for group in 0..10u64 {
			if group % 3 == 0 {
				video.cut(ms(group * 1_000));
			}
			video.frame(at(group), ms(group * 1_000), true);
		}

		assert_eq!(drain(&mut video)[0], (0, 3_000, at(0), at(3)));
	}

	#[test]
	fn a_non_keyframe_start_is_flagged() {
		let mut video = Segmenter::new(Config::default());
		video.frame(at(0), ms(0), true);
		// A mid-stream join: the group doesn't open on an IDR.
		video.frame(at(1), ms(2_000), false);
		video.frame(at(2), ms(4_000), true);

		let records: Vec<Record> = std::iter::from_fn(|| video.next()).collect();
		assert!(records[0].keyframe);
		assert!(!records[1].keyframe);
	}

	#[test]
	fn a_report_that_does_not_advance_is_ignored() {
		let mut video = Segmenter::new(Config::default());
		video.frame(at(3), ms(0), true);
		video.frame(at(2), ms(2_000), true);
		video.frame(at(4), ms(2_000), true);
		assert_eq!(drain(&mut video), vec![(0, 2_000, at(3), at(4))]);
	}

	#[test]
	fn a_resumed_segmenter_continues_the_numbering() {
		let mut video = Segmenter::new(Config::default()).with_sequence(7);
		groups(&mut video, 10..12, 2_000);
		assert_eq!(video.next().unwrap().sequence, 7);
	}

	/// Drain a finished timeline track.
	async fn read(broadcast: &moq_net::broadcast::Producer, section: &Archive, track: &str) -> Vec<Entry> {
		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), section, track)
			.await
			.unwrap();
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(event))) = consumer.poll_next(&waiter) {
			if let Event::Push { index, entry } = event {
				assert_eq!(index, entry.sequence);
				out.push(entry);
			}
		}
		out
	}

	#[tokio::test]
	async fn tracks_publish_independent_timelines() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast, Config::default());
		let mut video = timelines.track("video0").unwrap();
		let mut catalog = timelines.sparse("catalog.json").unwrap();

		catalog.frame(at(0), ms(0), true);
		catalog.finish_group(0);
		for group in 0..3 {
			video.frame(at(group), ms(group * 2_000), true);
			video.finish_group(group);
		}
		video.end(ms(6_000));
		drop(video);
		timelines.finish();

		let section = timelines.section();
		assert_eq!(section.timelines["video0"], "video0.timeline.z");
		assert_eq!(section.timelines["catalog.json"], "catalog.json.timeline.z");
		assert_eq!(section.duration_max, Some(10_000));

		let video = read(&broadcast, &section, "video0").await;
		assert_eq!(video.len(), 3);
		assert_eq!(video[2].end, at(3), "the terminal flush ends after the finished group");
		let catalog = read(&broadcast, &section, "catalog.json").await;
		assert_eq!(catalog.len(), 1, "the catalog does not wait for the video");
	}

	#[tokio::test]
	async fn a_sparse_record_publishes_before_the_track_ends() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast, Config::default());
		let mut catalog = timelines.sparse("catalog.json").unwrap();
		catalog.frame(at(0), ms(0), true);
		catalog.finish_group(0);

		assert_eq!(read(&broadcast, &timelines.section(), "catalog.json").await.len(), 1);
		drop(catalog);
	}

	#[tokio::test]
	async fn re_enrolling_continues_the_timeline() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast, Config::default());
		let mut first = timelines.track("video0").unwrap();
		first.frame(at(0), ms(0), true);
		drop(first);
		let mut second = timelines.track("video0").unwrap();
		// The new producer restarts its group sequences.
		second.frame(at(0), ms(2_000), true);
		drop(second);
		timelines.finish();

		let entries = read(&broadcast, &timelines.section(), "video0").await;
		assert_eq!(
			entries.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
			vec![0, 1]
		);
	}

	#[test]
	fn a_taken_timeline_name_fails_enrollment() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _squat = broadcast.create_track("video0.timeline.z", None).unwrap();
		let timelines = Timelines::new(&broadcast, Config::default());
		assert!(timelines.track("video0").is_err());
		assert!(timelines.section().timelines.is_empty());
	}

	#[test]
	fn enrollment_after_finish_is_refused() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast, Config::default());
		timelines.finish();
		assert!(matches!(
			timelines.track("video0"),
			Err(crate::Error::Moq(moq_net::Error::Closed))
		));
	}

	#[tokio::test]
	async fn a_resumed_producer_continues_the_checkpoint() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video0.timeline.z", Producer::info()).unwrap();
		let checkpoint = Checkpoint {
			range: 1..3,
			records: vec![
				Record::new(1, 1_000, 1_000, at(1), at(2)),
				Record::new(2, 2_000, 1_000, at(2), at(3)),
			],
		};
		let mut producer = Producer::resume(track, &checkpoint).unwrap();
		assert!(matches!(
			producer.push(&Record::new(4, 3_000, 1_000, at(3), at(4))),
			Err(crate::Error::TimelineSequence { expected: 3, actual: 4 })
		));
		producer.push(&Record::new(3, 3_000, 1_000, at(3), at(4))).unwrap();
		producer.finish().unwrap();

		let mut section = Config::default().section();
		section
			.timelines
			.insert("video0".to_string(), "video0.timeline.z".to_string());
		let entries = read(&broadcast, &section, "video0").await;
		assert_eq!(
			entries.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
			vec![1, 2, 3]
		);
	}

	#[test]
	fn a_checkpoint_record_must_be_the_sequence_at_its_index() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video0.timeline.z", Producer::info()).unwrap();
		let checkpoint = Checkpoint {
			range: 0..1,
			records: vec![Record::new(5, 0, 1_000, at(0), at(1))],
		};
		assert!(matches!(
			Producer::resume(track, &checkpoint),
			Err(crate::Error::TimelineCheckpoint(0))
		));
	}

	#[tokio::test]
	async fn a_missing_timeline_is_an_error() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let section = Config::default().section();
		assert!(matches!(
			Consumer::<()>::subscribe(&broadcast.consume(), &section, "video0").await,
			Err(crate::Error::TimelineMissing(_))
		));
	}

	#[tokio::test]
	async fn rejects_an_invalid_timescale() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast, Config::default());
		let _recorder = timelines.track("video0").unwrap();
		let mut section = timelines.section();
		section.timescale = 0;
		let err = Consumer::<()>::subscribe(&broadcast.consume(), &section, "video0").await;
		assert!(matches!(err, Err(crate::Error::InvalidTimescale(0))));
	}
}

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
//! Every track follows one rule, parameterized by its [`Config`]:
//!
//! - A record ends at the first group boundary at least [`Config::duration_min`] (4s by default)
//!   past its own start, so short groups (audio) pack into one record and long ones (video GOPs)
//!   get one each. Only sparse data, such as a catalog that may not publish again for the rest of
//!   the broadcast, uses a zero minimum: every group is then its own record, ended as soon as the
//!   group finishes.
//! - A group still open three declared durations past the record's start is split at the next
//!   reported frame, so an append-only group that never closes is still indexed as it grows.
//!   Before an unhinted track declares, the ceiling is ten seconds, or the minimum if longer.
//! - A skipped group sequence always closes the record at its newest reported content time, so a
//!   record's groups are contiguous and a pause is not absorbed into its duration.
//! - [`Segmenter::cut`] adds an application boundary, such as a video keyframe cutting audio so a
//!   derived segment needs fewer objects. The first cut takes over from minimum-duration pacing.
//!
//! An untimed track, whose frames carry no timestamps, has no duration to pace by: each group is
//! its own record, ended when the group finishes or the next one starts, and its records carry no
//! `pts` or `duration`. Nothing splits an untimed group, so one that never closes is indexed only
//! when the segmenter flushes.
//!
//! ## Wiring
//!
//! [`catalog::Producer`](crate::catalog::Producer) owns the broadcast's [`Timelines`] and advertises
//! them in the catalog's root [`hang::catalog::Archive`] entry. Its role-specific track
//! constructors enroll each media track, and the catalog enrolls itself with a zero minimum.
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

use hang::catalog::{Archive, Timeline};
use hang::timeline::{Position, Record, RecordExt};

use moq_net::{Timescale, Timestamp};

/// The default [`Config::duration_min`] (4 seconds). Only sparse data, such as a catalog, uses zero.
pub const DEFAULT_DURATION_MIN: Duration = Duration::from_secs(4);

/// The safety ceiling before an unhinted track declares its duration, unless the minimum is longer.
const DEFAULT_DURATION_CEILING: Duration = Duration::from_secs(10);

/// Records may overrun their target to reach a keyframe, but not indefinitely.
const DURATION_CEILING_MULTIPLIER: u32 = 3;

/// Recent records repeated when the window track rolls to a new group.
const CHECKPOINT_RECORDS: usize = 256;

/// How a track's records are cut.
///
/// Every track follows one rule: a record ends at the first group boundary at least
/// [`duration_min`](Self::duration_min) past its start. A hint fixes the nominal target to its
/// smallest multiple at or beyond that minimum.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The shortest a record may be before a group boundary ends it.
	///
	/// A floor rather than a target on purpose: a floor is always satisfiable (wait for the next
	/// group), so no group is split to honor it. Zero, for sparse data such as a catalog, makes
	/// every group its own record, ended as soon as the group finishes.
	pub duration_min: Duration,

	/// The application's nominal group duration, when known.
	pub duration_hint: Option<Duration>,
}

impl Config {
	/// Set [`duration_min`](Self::duration_min).
	pub fn with_duration_min(mut self, duration: Duration) -> Self {
		self.duration_min = duration;
		self
	}

	/// Set [`duration_hint`](Self::duration_hint).
	pub fn with_duration_hint(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.duration_hint = duration.into();
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			duration_min: DEFAULT_DURATION_MIN,
			duration_hint: None,
		}
	}
}

/// The wire timescale for record `pts`/`duration`: the catalog section's default, milliseconds.
const TIMESCALE: Timescale = Timescale::MILLI;

/// A duration in wire units, rounded up so a bound never understates itself.
fn units(duration: Duration) -> u64 {
	u64::try_from((duration.as_micros() * TIMESCALE.as_u64() as u128).div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

fn duration(units: u64) -> Duration {
	Duration::from_micros(units.saturating_mul(1_000_000) / TIMESCALE.as_u64())
}

fn wire(pts: Timestamp) -> u64 {
	pts.as_scale(TIMESCALE) as u64
}

/// The record being built.
struct Open {
	start: Position,
	/// `None` on an untimed track.
	pts: Option<Timestamp>,
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
	/// Fixed nominal duration; absent until an unhinted track's first record closes. Kept unrounded
	/// so pacing hits a fractional-millisecond GOP's boundary; only the wire declaration rounds.
	declared: Option<Duration>,
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
	/// Whether the track's frames carry timestamps, fixed by its first report.
	timed: Option<bool>,
	ready: VecDeque<Record>,
	closed: bool,
}

impl Segmenter {
	/// A segmenter numbering its records from zero.
	pub fn new(config: Config) -> Self {
		let declared = match config.duration_hint.filter(|hint| !hint.is_zero()) {
			Some(hint) => {
				let multiple = config.duration_min.as_nanos().div_ceil(hint.as_nanos()).max(1);
				Some(hint.saturating_mul(u32::try_from(multiple).unwrap_or(u32::MAX)))
			}
			None if config.duration_min.is_zero() => Some(Duration::ZERO),
			None => None,
		};
		Self {
			config,
			declared,
			sequence: 0,
			open: None,
			last: None,
			finished: false,
			frontier: None,
			cuts: VecDeque::new(),
			manual: false,
			timed: None,
			ready: VecDeque::new(),
			closed: false,
		}
	}

	/// The fixed nominal record duration, once known, in timeline units. An untimed track has no
	/// duration to declare.
	pub fn declared_duration(&self) -> Option<u64> {
		self.declared.filter(|_| self.timed != Some(false)).map(units)
	}

	fn target(&self) -> Duration {
		self.declared
			.filter(|value| !value.is_zero())
			.unwrap_or(self.config.duration_min)
	}

	fn ceiling(&self) -> Duration {
		self.declared
			.filter(|value| !value.is_zero())
			.and_then(|value| value.checked_mul(DURATION_CEILING_MULTIPLIER))
			.unwrap_or(DEFAULT_DURATION_CEILING.max(self.config.duration_min))
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

	/// Report the frame at `position`, presented at `pts`, or `None` on an untimed track.
	///
	/// Fails with [`TimelinePosition`](crate::Error::TimelinePosition) when `position` is at or
	/// before the previous report: records never overlap, so that frame could never be indexed.
	/// Fails with [`TimelineTiming`](crate::Error::TimelineTiming) when the track's first report
	/// was timed and this one is not, or the reverse.
	pub fn frame(
		&mut self,
		position: Position,
		pts: impl Into<Option<Timestamp>>,
		keyframe: bool,
	) -> crate::Result<()> {
		let pts = pts.into();
		if self.closed {
			return Ok(());
		}
		if let Some(last) = self.last
			&& position <= last
		{
			return Err(crate::Error::TimelinePosition { position, last });
		}
		if *self.timed.get_or_insert(pts.is_some()) != pts.is_some() {
			return Err(crate::Error::TimelineTiming(position));
		}
		if pts.is_none() {
			// Cuts are content times, which an untimed track never reaches.
			self.cuts.clear();
		}

		if let (Some(start), Some(last)) = (self.open.as_ref().map(|open| open.pts), self.last) {
			match (start, pts) {
				(Some(start), Some(pts)) => {
					if let Some(end) = self.timed_end(start, pts, position, last) {
						self.emit(end, Some(pts), true);
					}
				}
				// An untimed record holds one group, ended where the next one starts.
				_ if position.group != last.group => self.emit(Position::group(last.group + 1), None, true),
				_ => {}
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
		if let Some(pts) = pts {
			self.advance(pts);
		}
		Ok(())
	}

	/// Where pacing ends a timed record that started at `start`, if a frame at `position` and `pts`
	/// ends it there. A gap instead closes the record itself and returns `None`.
	fn timed_end(&mut self, start: Timestamp, pts: Timestamp, position: Position, last: Position) -> Option<Position> {
		let elapsed = pts.as_micros().saturating_sub(start.as_micros());
		if position.group == last.group {
			return (elapsed >= self.ceiling().as_micros()).then_some(position);
		}
		let contiguous = position.group == last.group + 1 && position.frame == 0;
		if !contiguous {
			// A skipped sequence or a group joined mid-way always closes, so a record's groups are
			// contiguous and the reader never has to guess what lies between. The skipped time is
			// not content, so the record ends at its newest report rather than at this frame.
			self.emit(Position::group(last.group + 1), self.frontier, false);
			return None;
		}
		if elapsed >= self.ceiling().as_micros() || self.boundary(start, pts, elapsed) {
			return Some(Position::group(last.group + 1));
		}
		None
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
		!self.manual && elapsed >= self.target().as_micros()
	}

	/// Report that group `group` can gain no more frames.
	///
	/// With a zero [`Config::duration_min`], or on an untimed track, this closes the record, so the
	/// newest group is indexed without waiting for the next one.
	pub fn finish_group(&mut self, group: u64) {
		if self.closed || self.last.is_none_or(|last| last.group != group) {
			return;
		}
		self.finished = true;
		let each = self.config.duration_min.is_zero() || self.timed == Some(false);
		if each && self.open.is_some() {
			self.emit(Position::group(group + 1), self.frontier, true);
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
	/// renditions of one import) cost nothing. An untimed track ignores cuts.
	pub fn cut(&mut self, pts: Timestamp) {
		if self.closed || self.timed == Some(false) {
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
		let Some(last) = self.last else {
			return;
		};
		if self.open.is_none() {
			return;
		}
		let end = match self.finished {
			true => Position::group(last.group + 1),
			false => Position::new(last.group, last.frame + 1),
		};
		self.emit(end, self.frontier, false);
	}

	/// Flush, then ignore every later report.
	pub fn close(&mut self) {
		self.flush();
		self.closed = true;
	}

	/// Close the open record at `end`, its content running to `pts`, which is `None` only when
	/// the track is untimed. `paced` is false when a gap or flush closes the record early.
	fn emit(&mut self, end: Position, pts: Option<Timestamp>, paced: bool) {
		let Some(open) = self.open.take() else {
			return;
		};
		let mut record = match open.pts {
			Some(start) => {
				let start = wire(start);
				let pts = pts.expect("a timed record has a frontier");
				// Frames farther apart than the maximum leave nothing to split at, so the record stops at
				// the advertised bound and the rest of the gap is uncovered.
				let duration = wire(pts).saturating_sub(start).min(units(self.ceiling()));
				// The first record fixes an unhinted declaration. One closed early says nothing about
				// the cadence, so the target falls back to the minimum.
				self.declared.get_or_insert(match paced {
					true => self::duration(duration),
					false => self.config.duration_min,
				});
				Record::new(self.sequence, start, duration, open.start, end)
			}
			None => Record::untimed(self.sequence, open.start, end),
		};
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
	/// The window's track, kept to abort it with the cause of a failure.
	track: moq_net::track::Producer,
}

impl Producer {
	/// The properties a timeline track is created with.
	pub fn info() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_priority(hang::catalog::PRIORITY.catalog)
			// A joiner asks for a fresh group once the ops outweigh eight headers.
			.with_refresh(8)
	}

	/// Publish onto `track`, starting at record zero.
	pub fn new(track: moq_net::track::Producer) -> Self {
		Self {
			sink: moq_json::window::Producer::new(track.clone(), Self::config()),
			track,
		}
	}

	/// Abort the timeline track with `err`, so its readers see a failure rather than an end.
	fn abort(self, err: &crate::Error) {
		// A cause outside moq-net has no wire code, so readers get a generic failure.
		let reason = match err {
			crate::Error::Moq(err) => err.clone(),
			_ => moq_net::StreamError::Internal.into(),
		};
		// A track that already closed keeps its first reason.
		let _ = self.track.abort(reason);
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
		self.refresh()?;
		self.sink
			.push(moq_net::Timed::from(record).at(moq_net::Timestamp::now()))
			.map_err(json)
	}

	/// Remove up to `count` oldest records from the window.
	pub fn pop(&mut self, count: u64) -> crate::Result<()> {
		self.refresh()?;
		self.sink
			.pop(moq_net::Timed::from(count).at(moq_net::Timestamp::now()))
			.map_err(json)
	}

	/// Finish the timeline track.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.sink.finish().map_err(json)
	}

	/// Restate the window in a fresh group if a joiner wants one.
	///
	/// Checked on each edit rather than driven by a task, since nothing here owns a runtime. A
	/// timeline gains a record every segment, so a joiner waits at most about one segment.
	fn refresh(&mut self) -> crate::Result<()> {
		match self.sink.poll_refresh(&kio::Waiter::noop()) {
			std::task::Poll::Ready(Err(err)) => Err(json(err)),
			_ => Ok(()),
		}
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
	/// Publishes an unhinted declaration with the first complete record.
	on_declare: Option<OnDeclare>,
	/// `None` once a publish failed: the timeline is an optional sidecar, so it stops instead.
	output: Option<Producer>,
	/// Whether a [`Recorder`] owns the segmenter, so a second enrollment can't share it.
	recording: bool,
}

impl Live {
	fn publish(&mut self) {
		for record in self.segmenter.by_ref() {
			let Some(output) = self.output.as_mut() else {
				continue;
			};
			if let Err(err) = output.push(&record) {
				tracing::warn!(%err, "timeline publish failed; aborting the timeline track");
				if let Some(output) = self.output.take() {
					output.abort(&err);
				}
			}
		}
		if let Some(duration) = self.segmenter.declared_duration()
			&& let Some(on_declare) = self.on_declare.take()
		{
			on_declare(duration);
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

type OnDeclare = Box<dyn FnOnce(u64) + Send>;

struct Registry {
	/// Each enrolled track's timeline, by track name. Kept after its recorder drops, so a finished
	/// span stays discoverable and a re-enrolled track continues its numbering, until
	/// [`Timelines::retire`] says the name is done.
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
	registry: Arc<Mutex<Registry>>,
}

impl Timelines {
	/// Timelines for tracks of `broadcast`.
	pub fn new(broadcast: &moq_net::broadcast::Producer) -> Self {
		let registry = Arc::new(Mutex::new(Registry {
			tracks: BTreeMap::new(),
			finished: false,
		}));
		Self {
			broadcast: broadcast.clone(),
			registry,
		}
	}

	/// Enroll the track `name`, its records cut by `config`.
	///
	/// Creates its timeline track, named by [`hang::timeline::default_name`], on first enrollment.
	/// Enrolling a name again after its recorder dropped continues its timeline's numbering for a new
	/// producer, cut by the new `config`. Errors when the name's recorder is still alive, the timeline
	/// track name is taken, or the timelines finished.
	pub fn track(&self, name: &str, config: Config) -> crate::Result<Recorder> {
		let mut registry = self.registry.lock().unwrap();
		if registry.finished {
			return Err(moq_net::Error::Closed.into());
		}
		if let Some((_, existing)) = registry.tracks.get(name) {
			// A re-enrolled track is a new producer whose group sequences may restart, so it gets a
			// fresh segmenter continuing the record numbering.
			let mut live = existing.lock().unwrap();
			if live.recording {
				return Err(moq_net::Error::Duplicate.into());
			}
			live.recording = true;
			live.segmenter.flush();
			live.publish();
			let sequence = live.segmenter.sequence();
			let declared = live.segmenter.declared;
			live.segmenter = Segmenter::new(config).with_sequence(sequence);
			if declared.is_some() {
				live.segmenter.declared = declared;
			}
			drop(live);
			return Ok(Recorder { live: existing.clone() });
		}

		let timeline = hang::timeline::default_name(name);
		let track = self.broadcast.create_track(timeline.as_str(), Producer::info())?;
		let live = Arc::new(Mutex::new(Live {
			segmenter: Segmenter::new(config),
			on_declare: None,
			output: Some(Producer::new(track)),
			recording: true,
		}));
		registry.tracks.insert(name.to_string(), (timeline, live.clone()));
		Ok(Recorder { live })
	}

	/// The catalog's root `archive` entry advertising every timeline whose duration is known.
	pub fn section(&self) -> Archive {
		let registry = self.registry.lock().unwrap();
		let mut section = Archive::new();
		section.timescale = TIMESCALE.as_u64() as u32;
		section.timelines = registry
			.tracks
			.iter()
			.filter_map(|(name, (track, live))| {
				live.lock()
					.unwrap()
					.segmenter
					.declared_duration()
					.map(|duration| (name.clone(), Timeline::new(track, duration)))
			})
			.collect();
		section
	}

	/// Finish the timeline of `name`, whose recorder dropped and whose owner never enrolls the
	/// name again, and forget it, so a producer minting a name per track holds only its live ones.
	///
	/// Readers already on the timeline track read it to its end. Nothing happens for a name never
	/// enrolled; a name whose recorder is still alive errors.
	pub(crate) fn retire(&self, name: &str) -> crate::Result<()> {
		let mut registry = self.registry.lock().unwrap();
		let Some((_, live)) = registry.tracks.get(name) else {
			return Ok(());
		};
		let mut live = live.lock().unwrap();
		if live.recording {
			return Err(moq_net::Error::Duplicate.into());
		}
		// The catalog stops advertising the timeline, so a last declaration must not add it back.
		live.on_declare = None;
		live.finish();
		drop(live);
		registry.tracks.remove(name);
		Ok(())
	}

	/// How many timelines are held, live or kept for a re-enrollment.
	#[cfg(test)]
	pub(crate) fn len(&self) -> usize {
		self.registry.lock().unwrap().tracks.len()
	}

	/// Close every open record now, publishing any declaration it fixes.
	pub(crate) fn flush(&self) {
		let registry = self.registry.lock().unwrap();
		for (_, live) in registry.tracks.values() {
			let mut live = live.lock().unwrap();
			live.segmenter.flush();
			live.publish();
		}
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
	/// Run `callback` once when an unhinted track learns its fixed duration.
	pub(crate) fn on_declare(&mut self, callback: impl FnOnce(u64) + Send + 'static) {
		self.live.lock().unwrap().on_declare = Some(Box::new(callback));
	}
	fn report(&mut self, f: impl FnOnce(&mut Segmenter)) {
		let mut live = self.live.lock().unwrap();
		f(&mut live.segmenter);
		live.publish();
	}

	/// Report the frame at `position`, presented at `pts`; see [`Segmenter::frame`].
	///
	/// A report the timeline cannot place ends the timeline track with an error rather than
	/// publishing a timeline that silently omits frames. Media publishing is unaffected.
	pub fn frame(&mut self, position: Position, pts: Timestamp, keyframe: bool) {
		let mut live = self.live.lock().unwrap();
		if let Err(err) = live.segmenter.frame(position, pts, keyframe) {
			tracing::warn!(%err, "timeline report rejected; aborting the timeline track");
			live.segmenter.close();
			// Subscribers see the timeline fail rather than end.
			if let Some(output) = live.output.take() {
				output.abort(&err);
			}
		}
		live.publish();
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

	/// Close the open record now; see [`Segmenter::flush`].
	pub(crate) fn flush(&mut self) {
		self.report(|segmenter| segmenter.flush());
	}
}

impl Drop for Recorder {
	fn drop(&mut self) {
		let mut live = self.live.lock().unwrap();
		live.segmenter.flush();
		live.publish();
		live.recording = false;
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
	/// Decode a record into an entry, converting its timing out of the wire timescale.
	///
	/// A pts the timescale can't represent is an error rather than a substituted value: silently
	/// moving a timestamp would misdirect seeking and live-edge logic. An untimed record, or one
	/// missing half its timing, fails with [`TimelineUntimed`](crate::Error::TimelineUntimed).
	pub fn decode(record: Record<E>, timescale: Timescale) -> crate::Result<Entry<E>> {
		let (Some(pts), Some(duration)) = (record.pts, record.duration) else {
			return Err(crate::Error::TimelineUntimed(record.sequence));
		};
		let scale = timescale.as_u64() as u128;
		let nanos = (duration as u128) * 1_000_000_000 / scale;
		Ok(Self {
			sequence: record.sequence,
			pts: Timestamp::new(pts, timescale)?,
			duration: Duration::from_nanos(nanos as u64),
			start: record.start,
			end: record.end,
			keyframe: record.keyframe,
			ext: record.ext,
		})
	}

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
		let timeline = section
			.timelines
			.get(track)
			.ok_or_else(|| crate::Error::TimelineMissing(track.to_string()))?;
		let timescale =
			Timescale::new(section.timescale as u64).map_err(|_| crate::Error::InvalidTimescale(section.timescale))?;
		let track = broadcast.track(&timeline.track)?.subscribe(None).await?;
		let config = moq_json::window::ConsumerConfig::default().with_compression(true);
		Ok(Self {
			inner: moq_json::window::Consumer::new(track, config),
			timescale,
		})
	}

	fn decode_event(&self, event: moq_json::window::Event<Record<E>>) -> crate::Result<Event<E>> {
		Ok(match event {
			moq_json::window::Event::Push { index, value } => Event::Push {
				index,
				entry: Entry::decode(value, self.timescale)?,
			},
			moq_json::window::Event::Pop(range) => Event::Pop(range),
			moq_json::window::Event::Skip(range) => Event::Skip(range),
			_ => unreachable!("unknown timeline window event"),
		})
	}

	/// Get the next window event, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Event<E>>> {
		match self.inner.next().await? {
			Some(event) => Ok(Some(self.decode_event(event.value)?)),
			None => Ok(None),
		}
	}

	/// Poll for the next window event, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event<E>>>> {
		match self.inner.poll_next(waiter)? {
			// Each record carries its own pts, so the frame timestamp adds nothing.
			Poll::Ready(Some(event)) => Poll::Ready(self.decode_event(event.value).map(Some)),
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Sparse data: every group is its own record.
	fn sparse() -> Config {
		Config::default().with_duration_min(Duration::ZERO)
	}

	/// Media whose encoder reports a 4s cadence, so its timeline declares before any record.
	fn hinted() -> Config {
		Config::default().with_duration_hint(Duration::from_secs(4))
	}

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	fn at(group: u64) -> Position {
		Position::group(group)
	}

	/// A record's `(pts, duration, start, end)` in wire units, for compact comparisons.
	fn span(record: &Record) -> (u64, u64, Position, Position) {
		(record.pts.unwrap(), record.duration.unwrap(), record.start, record.end)
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
			segmenter.frame(at(group), ms(group * step), true).unwrap();
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
			vec![(0, 4_000, at(0), at(2)), (4_000, 2_000, at(2), at(3))]
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
	fn a_hint_declares_its_smallest_multiple_at_or_past_the_minimum() {
		let segmenter = Segmenter::new(Config::default().with_duration_hint(Duration::from_millis(1_500)));
		assert_eq!(segmenter.declared_duration(), Some(4_500));
	}

	/// A 60-frame GOP at 29.97fps lasts 2002.002ms. Rounding the hint before multiplying would aim
	/// at 4006ms, past the second GOP's end, and pace three GOPs per record instead of two.
	#[test]
	fn a_fractional_hint_paces_on_its_own_boundaries() {
		let gop = Duration::from_nanos(2_002_002_002);
		let mut video = Segmenter::new(Config::default().with_duration_hint(gop));
		assert_eq!(video.declared_duration(), Some(4_005));
		for group in 0..5 {
			let pts = Timestamp::from_micros((gop * group as u32).as_micros() as u64).unwrap();
			video.frame(at(group), pts, true).unwrap();
			video.finish_group(group);
		}

		assert_eq!(
			drain(&mut video),
			vec![(0, 4_004, at(0), at(2)), (4_004, 4_004, at(2), at(4))]
		);
	}

	#[test]
	fn a_hint_remains_the_target_when_the_minimum_is_zero() {
		let segmenter = Segmenter::new(
			Config::default()
				.with_duration_min(Duration::ZERO)
				.with_duration_hint(Duration::from_secs(3)),
		);
		assert_eq!(segmenter.declared_duration(), Some(3_000));
	}

	#[test]
	fn packet_groups_estimate_a_record_instead_of_one_packet() {
		let mut audio = Segmenter::new(Config::default());
		groups(&mut audio, 0..202, 20);
		assert_eq!(audio.declared_duration(), Some(4_000));
		assert_eq!(audio.next().unwrap().duration, Some(4_000));
	}

	#[test]
	fn an_early_first_record_declares_the_minimum() {
		// A flushed tail is not a cadence: one frame would otherwise declare zero, as if sparse.
		let mut flushed = Segmenter::new(Config::default());
		flushed.frame(at(0), ms(0), true).unwrap();
		flushed.flush();
		assert_eq!(flushed.next().unwrap().duration, Some(0));
		assert_eq!(flushed.declared_duration(), Some(4_000));

		// A skipped sequence ends the record early too.
		let mut skipped = Segmenter::new(Config::default());
		skipped.frame(at(0), ms(0), true).unwrap();
		skipped.frame(at(2), ms(9_000), true).unwrap();
		assert_eq!(skipped.next().unwrap().duration, Some(0));
		assert_eq!(skipped.declared_duration(), Some(4_000));

		// Later paced records keep the declaration.
		skipped.frame(at(3), ms(15_000), true).unwrap();
		assert_eq!(skipped.next().unwrap().duration, Some(6_000));
		assert_eq!(skipped.declared_duration(), Some(4_000));
	}

	#[test]
	fn the_first_estimate_survives_later_longer_records() {
		let mut video = Segmenter::new(Config::default());
		groups(&mut video, 0..3, 2_000);
		assert_eq!(video.next().unwrap().duration, Some(4_000));
		assert_eq!(video.declared_duration(), Some(4_000));

		groups(&mut video, 3..5, 6_000);
		assert_eq!(video.next().unwrap().duration, Some(12_000));
		assert_eq!(video.declared_duration(), Some(4_000));
	}

	#[test]
	fn a_long_group_splits_by_frame_at_the_safety_ceiling() {
		let mut video = Segmenter::new(
			Config::default()
				.with_duration_min(Duration::from_secs(1))
				.with_duration_hint(Duration::from_secs(1)),
		);
		for frame in 0..8 {
			video
				.frame(Position::new(0, frame), ms(frame * 1_000), frame == 0)
				.unwrap();
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
		let mut catalog = Segmenter::new(sparse());
		catalog.frame(at(0), ms(0), true).unwrap();
		assert!(catalog.next().is_none(), "the group may still grow");
		catalog.finish_group(0);
		assert_eq!(drain(&mut catalog), vec![(0, 0, at(0), at(1))]);

		catalog.frame(at(1), ms(60_000), true).unwrap();
		catalog.finish_group(1);
		assert_eq!(drain(&mut catalog), vec![(60_000, 0, at(1), at(2))]);
	}

	#[test]
	fn an_append_only_group_is_indexed_as_it_grows() {
		// A never-closing log: one frame a second, split every three seconds.
		let mut log = Segmenter::new(
			Config::default()
				.with_duration_min(Duration::from_secs(1))
				.with_duration_hint(Duration::from_secs(1)),
		);
		for frame in 0..7 {
			log.frame(Position::new(0, frame), ms(frame * 1_000), true).unwrap();
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
		let mut audio = Segmenter::new(Config::default().with_duration_min(Duration::from_secs(1)));
		audio.frame(at(0), ms(0), true).unwrap();
		audio.frame(at(1), ms(300), true).unwrap();
		audio.frame(at(5), ms(600), true).unwrap();
		audio.frame(at(6), ms(1_700), true).unwrap();
		audio.close();

		assert_eq!(
			drain(&mut audio),
			vec![
				(0, 300, at(0), at(2)),
				(600, 1_100, at(5), at(6)),
				(1_700, 0, at(6), Position::new(6, 1))
			]
		);
	}

	#[test]
	fn a_skipped_run_ends_at_the_previous_frontier() {
		// A track paused for 40 minutes, resuming past a skipped sequence.
		let mut data = Segmenter::new(Config::default());
		groups(&mut data, 0..2, 1_000);
		data.end(ms(2_000));
		data.frame(at(3), ms(2_400_000), true).unwrap();
		data.close();

		assert_eq!(
			drain(&mut data),
			vec![(0, 2_000, at(0), at(2)), (2_400_000, 0, at(3), Position::new(3, 1))],
			"the pause is not absorbed into the record before it"
		);
	}

	#[test]
	fn an_unhinted_first_record_declares_the_default_ceiling_when_it_hits_it() {
		let mut log = Segmenter::new(Config::default());
		log.frame(Position::new(0, 0), ms(0), true).unwrap();
		log.frame(Position::new(0, 1), ms(20_000), true).unwrap();

		assert_eq!(
			drain(&mut log),
			vec![(0, 10_000, Position::new(0, 0), Position::new(0, 1))]
		);
		assert_eq!(log.declared_duration(), Some(10_000));
	}

	#[test]
	fn an_unhinted_ceiling_never_undercuts_a_longer_minimum() {
		let mut log = Segmenter::new(Config::default().with_duration_min(Duration::from_secs(12)));
		log.frame(Position::new(0, 0), ms(0), true).unwrap();
		log.frame(Position::new(0, 1), ms(11_000), true).unwrap();
		log.frame(Position::new(0, 2), ms(20_000), true).unwrap();

		assert_eq!(
			drain(&mut log),
			vec![(0, 12_000, Position::new(0, 0), Position::new(0, 2))]
		);
		assert_eq!(log.declared_duration(), Some(12_000));
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
			video.frame(at(group), ms(group * 1_000), true).unwrap();
		}

		assert_eq!(drain(&mut video)[0], (0, 3_000, at(0), at(3)));
	}

	#[test]
	fn a_non_keyframe_start_is_flagged() {
		let mut video = Segmenter::new(Config::default());
		video.frame(at(0), ms(0), true).unwrap();
		// A mid-stream join: the group doesn't open on an IDR.
		video.frame(at(1), ms(4_000), false).unwrap();
		video.frame(at(2), ms(8_000), true).unwrap();

		let records: Vec<Record> = std::iter::from_fn(|| video.next()).collect();
		assert!(records[0].keyframe);
		assert!(!records[1].keyframe);
	}

	#[test]
	fn a_report_that_does_not_advance_is_refused() {
		let mut video = Segmenter::new(Config::default());
		video.frame(at(3), ms(0), true).unwrap();
		for position in [at(2), at(3)] {
			assert!(matches!(
				video.frame(position, ms(2_000), true),
				Err(crate::Error::TimelinePosition { last, .. }) if last == at(3)
			));
		}
		video.frame(at(4), ms(4_000), true).unwrap();
		assert_eq!(drain(&mut video), vec![(0, 4_000, at(3), at(4))]);
	}

	#[test]
	fn a_resumed_segmenter_continues_the_numbering() {
		let mut video = Segmenter::new(Config::default()).with_sequence(7);
		groups(&mut video, 10..13, 2_000);
		assert_eq!(video.next().unwrap().sequence, 7);
	}

	#[test]
	fn an_untimed_track_records_each_group_without_timing() {
		// The minimum and hint measure time, which an untimed track has none of.
		let mut data = Segmenter::new(Config::default().with_duration_hint(Duration::from_secs(1)));
		data.frame(Position::new(0, 0), None, true).unwrap();
		data.frame(Position::new(0, 1), None, false).unwrap();
		assert!(data.next().is_none(), "nothing splits an untimed group");
		data.finish_group(0);
		// A group that starts before the previous one finished ends it, as does a skipped sequence.
		data.frame(at(1), None, true).unwrap();
		data.frame(at(3), None, true).unwrap();
		data.frame(Position::new(3, 1), None, false).unwrap();
		data.close();

		let records: Vec<Record> = std::iter::from_fn(|| data.next()).collect();
		assert_eq!(
			records,
			vec![
				Record::untimed(0, at(0), at(1)),
				Record::untimed(1, at(1), at(2)),
				Record::untimed(2, at(3), Position::new(3, 2)),
			]
		);
		assert_eq!(data.declared_duration(), None, "an untimed track declares no duration");
	}

	#[test]
	fn a_sparse_untimed_track_declares_no_duration() {
		let mut data = Segmenter::new(Config::default().with_duration_min(Duration::ZERO));
		assert_eq!(data.declared_duration(), Some(0), "unknown timing defaults to sparse");
		data.frame(at(0), None, true).unwrap();
		data.finish_group(0);
		assert_eq!(data.next(), Some(Record::untimed(0, at(0), at(1))));
		assert_eq!(data.declared_duration(), None);
	}

	#[test]
	fn an_untimed_track_keeps_no_cuts() {
		let mut data = Segmenter::new(Config::default());
		data.cut(ms(0));
		data.frame(at(0), None, true).unwrap();
		assert!(data.cuts.is_empty(), "a cut before the first report is dropped");
		for i in 1..100 {
			data.cut(ms(i * 1_000));
		}
		assert!(data.cuts.is_empty());
	}

	#[test]
	fn a_track_is_all_timed_or_all_untimed() {
		let mut timed = Segmenter::new(Config::default());
		timed.frame(at(0), ms(0), true).unwrap();
		assert!(matches!(
			timed.frame(at(1), None, true),
			Err(crate::Error::TimelineTiming(position)) if position == at(1)
		));

		let mut untimed = Segmenter::new(Config::default());
		untimed.frame(at(0), None, true).unwrap();
		assert!(matches!(
			untimed.frame(at(1), ms(0), true),
			Err(crate::Error::TimelineTiming(_))
		));
	}

	#[test]
	fn an_untimed_record_has_no_entry() {
		let record = Record::<()>::untimed(3, at(0), at(1));
		assert!(matches!(
			Entry::decode(record, Timescale::MILLI),
			Err(crate::Error::TimelineUntimed(3))
		));
		let mut half = Record::<()>::new(4, 0, 1_000, at(0), at(1));
		half.duration = None;
		assert!(matches!(
			Entry::decode(half, Timescale::MILLI),
			Err(crate::Error::TimelineUntimed(4))
		));
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
		let timelines = Timelines::new(&broadcast);
		let mut video = timelines.track("video0", hinted()).unwrap();
		let mut catalog = timelines.track("catalog.json", sparse()).unwrap();

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
		assert_eq!(section.timelines["video0"], Timeline::new("video0.timeline.z", 4_000));
		assert_eq!(
			section.timelines["catalog.json"],
			Timeline::new("catalog.json.timeline.z", 0)
		);

		let video = read(&broadcast, &section, "video0").await;
		assert_eq!(video.len(), 2);
		assert_eq!(video[1].end, at(3), "the terminal flush ends after the finished group");
		let catalog = read(&broadcast, &section, "catalog.json").await;
		assert_eq!(catalog.len(), 1, "the catalog does not wait for the video");
	}

	#[test]
	fn an_empty_enrollment_does_not_mask_a_later_hint() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		drop(timelines.track("video", Config::default()).unwrap());
		let _video = timelines
			.track("video", Config::default().with_duration_hint(Duration::from_secs(6)))
			.unwrap();
		assert_eq!(timelines.section().timelines["video"].duration, Some(6_000));
	}

	#[test]
	fn an_unhinted_timeline_waits_for_its_first_record() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let mut video = timelines.track("video", Config::default()).unwrap();
		assert!(!timelines.section().timelines.contains_key("video"));

		video.frame(at(0), ms(0), true);
		video.finish_group(0);
		video.frame(at(1), ms(4_000), true);
		assert_eq!(timelines.section().timelines["video"].duration, Some(4_000));
	}

	#[tokio::test]
	async fn a_sparse_record_publishes_before_the_track_ends() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let mut catalog = timelines.track("catalog.json", sparse()).unwrap();
		catalog.frame(at(0), ms(0), true);
		catalog.finish_group(0);

		assert_eq!(read(&broadcast, &timelines.section(), "catalog.json").await.len(), 1);
		drop(catalog);
	}

	#[tokio::test]
	async fn a_rejected_report_fails_the_timeline() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let mut video = timelines.track("video0", hinted()).unwrap();
		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), &timelines.section(), "video0")
			.await
			.unwrap();

		video.frame(at(1), ms(0), true);
		video.frame(at(0), ms(2_000), true);
		loop {
			match consumer.next().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("the timeline ended cleanly despite omitting a frame"),
				// Aborted with the failure, not left to read as a producer that went away.
				Err(err) => {
					assert!(
						matches!(
							err,
							crate::Error::Json(moq_json::Error::Net(moq_net::Error::Stream(
								moq_net::StreamError::Internal
							)))
						),
						"{err:?}"
					);
					break;
				}
			}
		}
	}

	#[tokio::test]
	async fn re_enrolling_continues_the_timeline() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let mut first = timelines.track("video0", Config::default()).unwrap();
		first.frame(at(0), ms(0), true);
		drop(first);
		let mut second = timelines.track("video0", Config::default()).unwrap();
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

	/// Two live recorders would share one segmenter, each resetting or closing the other's
	/// timeline, so a name is enrolled once at a time.
	#[tokio::test]
	async fn enrolling_an_active_track_is_refused() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let mut first = timelines.track("video0", Config::default()).unwrap();
		assert!(timelines.track("video0", Config::default()).is_err());

		// The live recorder is untouched by the refusal.
		first.frame(at(0), ms(0), true);
		first.frame(at(1), ms(2_000), true);
		drop(first);
		assert!(timelines.track("video0", Config::default()).is_ok());
	}

	#[test]
	fn a_taken_timeline_name_fails_enrollment() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _squat = broadcast.create_track("video0.timeline.z", None).unwrap();
		let timelines = Timelines::new(&broadcast);
		assert!(timelines.track("video0", Config::default()).is_err());
		assert!(timelines.section().timelines.is_empty());
	}

	#[test]
	fn enrollment_after_finish_is_refused() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		timelines.finish();
		assert!(matches!(
			timelines.track("video0", Config::default()),
			Err(crate::Error::Moq(moq_net::Error::Closed))
		));
	}

	#[tokio::test]
	async fn a_missing_timeline_is_an_error() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let section = Archive::new();
		assert!(matches!(
			Consumer::<()>::subscribe(&broadcast.consume(), &section, "video0").await,
			Err(crate::Error::TimelineMissing(_))
		));
	}

	#[tokio::test]
	async fn rejects_an_invalid_timescale() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timelines = Timelines::new(&broadcast);
		let _recorder = timelines.track("video0", hinted()).unwrap();
		let mut section = timelines.section();
		section.timescale = 0;
		let err = Consumer::<()>::subscribe(&broadcast.consume(), &section, "video0").await;
		assert!(matches!(err, Err(crate::Error::InvalidTimescale(0))));
	}
}

//! HLS segment records: where each of a rendition's HLS segments starts.
//!
//! Every video and audio rendition a catalog mints gets a track of [`hang::hls::Record`]s, one per
//! segment, and its catalog entry gets an [`hls`](hang::catalog::Hls) field naming that track and
//! the nominal segment duration, unless the application disables them with
//! [`catalog::Config::with_hls`](crate::catalog::Config::with_hls). An edge renders HLS (or DASH)
//! playlists from the records without subscribing to media. A record track nobody subscribes to
//! costs only the in-process bookkeeping.
//!
//! ## Segments
//!
//! A segment starts at a group boundary and holds whole groups, so a record is the segment's first
//! group and pts, published as the segment starts. Segments are cut on a grid of
//! [`Config::duration`] (4s by default) in pts: a group starts a segment when its pts falls in a
//! later multiple of the duration than the previous group's. Short groups (audio) pack into one
//! segment per grid cell, and long ones (video GOPs) get one each. A hint replaces the duration with
//! its smallest multiple at or beyond it, which is also the nominal duration declared; without one,
//! the first segment declares it.
//!
//! Each rendition cuts on its own grid and numbers its own records, sharing no state with the
//! others. Renditions whose keyframes sit at the same timestamps therefore cut the same boundaries,
//! including one enrolled late or rebuilt by a reconfigure, which lines up from its next grid line.
//!
//! Content stopping (a skipped group sequence, a break marker from
//! [`container::Producer::discontinuity`](crate::container::Producer::discontinuity), or the end of
//! the track) publishes a zero-duration record where it stopped, so the segment before it ends there
//! rather than absorbing the pause.
//!
//! ## Wiring
//!
//! [`catalog::Producer`](crate::catalog::Producer) owns the broadcast's record tracks and advertises
//! each in its rendition's `hls` field. On the read side, [`Consumer::subscribe`] reads one
//! rendition's records and yields decoded [`Event`]s. On the wire each record track is an
//! uncompressed [`moq_json::window`] whose groups restate the recent records.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use hang::hls::Record;

use moq_net::{Timescale, Timestamp};

/// The default [`Config::duration`] (4 seconds).
pub const DEFAULT_DURATION: Duration = Duration::from_secs(4);

/// The safety ceiling before an unhinted track declares its duration, unless the duration is longer.
const DEFAULT_DURATION_CEILING: Duration = Duration::from_secs(10);

/// A segment waiting on a source's cut may overrun its target to reach a group boundary, but not
/// indefinitely.
const DURATION_CEILING_MULTIPLIER: u32 = 3;

/// Recent records repeated when the window track rolls to a new group: one more than the 256
/// segments an HLS edge lists, since a segment is listed only once the record after it arrives.
const CHECKPOINT_RECORDS: usize = 257;

/// The wire timescale for record `pts`/`duration`: milliseconds.
const TIMESCALE: Timescale = Timescale::MILLI;

/// How a broadcast's HLS segments are cut.
///
/// A segment starts at the first group in a later multiple of [`duration`](Self::duration) than
/// the previous group's. A hint replaces the duration with its smallest multiple at or beyond it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The spacing of the pts grid segments are cut on.
	///
	/// A grid rather than a running length on purpose: renditions whose keyframes share timestamps
	/// cut the same boundaries without sharing any state.
	pub duration: Duration,

	/// The application's nominal group duration, when known.
	pub duration_hint: Option<Duration>,
}

impl Config {
	/// Set [`duration`](Self::duration).
	pub fn with_duration(mut self, duration: Duration) -> Self {
		self.duration = duration;
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
			duration: DEFAULT_DURATION,
			duration_hint: None,
		}
	}
}

/// A duration in wire units, rounded up so a nominal duration never understates itself.
fn units(duration: Duration) -> u64 {
	u64::try_from((duration.as_micros() * TIMESCALE.as_u64() as u128).div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

fn wire(pts: Timestamp) -> u64 {
	pts.as_scale(TIMESCALE) as u64
}

fn elapsed(start: Timestamp, pts: Timestamp) -> Duration {
	Duration::from_micros(u64::try_from(pts.as_micros().saturating_sub(start.as_micros())).unwrap_or(u64::MAX))
}

/// Builds one track's records from reports of the frames it published.
///
/// Reports arrive in group order: [`frame`](Self::frame) for each frame (or at least the first of
/// each group), [`end`](Self::end) wherever the content is known to extend, and
/// [`stop`](Self::stop) where it stops. Records queue until [`Iterator::next`] takes them.
pub(crate) struct Segmenter {
	config: Config,
	/// The grid's spacing: the hint's multiple, else the configured duration.
	grid: Duration,
	/// Fixed nominal duration; absent until an unhinted track's first segment closes. Kept unrounded;
	/// only the wire declaration rounds.
	declared: Option<Duration>,
	/// The number the next record gets.
	sequence: u64,
	/// The open segment's start: absent before the first segment and after a stop.
	open: Option<Timestamp>,
	/// The grid cell of the open segment's newest group.
	cell: u128,
	/// The newest reported group.
	last: Option<u64>,
	/// The newest reported content time in the open segment.
	frontier: Option<Timestamp>,
	/// Boundaries not yet reached, in order: at most the earliest and the latest.
	cuts: VecDeque<Timestamp>,
	/// The newest boundary declared, so a repeat of it (several sources declaring the same ones)
	/// is ignored.
	cut: Option<Timestamp>,
	/// Whether boundaries come only from cuts: the source declares them.
	manual: bool,
	ready: VecDeque<Record>,
	closed: bool,
}

impl Segmenter {
	/// A segmenter numbering its records from zero.
	pub fn new(config: Config) -> Self {
		let declared = config.duration_hint.filter(|hint| !hint.is_zero()).map(|hint| {
			let multiple = config.duration.as_nanos().div_ceil(hint.as_nanos()).max(1);
			hint.saturating_mul(u32::try_from(multiple).unwrap_or(u32::MAX))
		});
		Self {
			grid: declared.unwrap_or(config.duration),
			config,
			declared,
			sequence: 0,
			open: None,
			cell: 0,
			last: None,
			frontier: None,
			cuts: VecDeque::new(),
			cut: None,
			manual: false,
			ready: VecDeque::new(),
			closed: false,
		}
	}

	/// Number the next record `sequence`, continuing a re-enrolled track.
	pub fn with_sequence(mut self, sequence: u64) -> Self {
		self.sequence = sequence;
		self
	}

	/// The number the next record gets.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The fixed nominal segment duration, once known.
	pub fn declared(&self) -> Option<Duration> {
		self.declared
	}

	fn ceiling(&self) -> Duration {
		self.declared
			.filter(|value| !value.is_zero())
			.and_then(|value| value.checked_mul(DURATION_CEILING_MULTIPLIER))
			.unwrap_or(DEFAULT_DURATION_CEILING.max(self.config.duration))
	}

	/// The grid cell `pts` falls in. A zero grid gives every distinct pts its own.
	fn cell(&self, pts: Timestamp) -> u128 {
		let nanos = pts.as_micros() * 1_000;
		nanos.checked_div(self.grid.as_nanos()).unwrap_or(nanos)
	}

	/// Report a frame of group `group`, presented at `pts`.
	///
	/// Fails with [`HlsGroup`](crate::Error::HlsGroup) when `group` precedes the previous report's:
	/// segments hold whole groups in order, so that frame could never be listed.
	pub fn frame(&mut self, group: u64, pts: Timestamp) -> crate::Result<()> {
		if self.closed {
			return Ok(());
		}
		if let Some(last) = self.last {
			if group < last {
				return Err(crate::Error::HlsGroup { group, last });
			}
			if group != last {
				self.group(group, last, pts);
			}
		} else {
			self.begin(group, pts);
		}
		self.last = Some(group);
		self.advance(pts);
		Ok(())
	}

	/// Group `group` starts at `pts`, after group `last`.
	fn group(&mut self, group: u64, last: u64, pts: Timestamp) {
		let Some(start) = self.open else {
			self.begin(group, pts);
			return;
		};
		if group != last + 1 {
			// A skipped sequence stops the segment where its content reached, so the skipped time
			// is not absorbed into it and the reader never has to guess what lies between.
			self.stop_at(last + 1);
			self.begin(group, pts);
			return;
		}
		let cell = self.cell(pts);
		let elapsed = elapsed(start, pts);
		if elapsed >= self.ceiling() || self.boundary(start, pts, cell) {
			// A grid's first segment starts wherever the content does, so it may be short.
			let nominal = match self.manual {
				true => elapsed,
				false => elapsed.max(self.grid),
			};
			self.declared.get_or_insert(nominal);
			self.begin(group, pts);
		}
		self.cell = cell;
	}

	/// Start a segment with group `group` at `pts`.
	fn begin(&mut self, group: u64, pts: Timestamp) {
		// However the segment starts, a cut it reached is spent.
		while self.cuts.front().is_some_and(|cut| cut.as_micros() <= pts.as_micros()) {
			self.cuts.pop_front();
		}
		self.open = Some(pts);
		self.frontier = Some(pts);
		self.cell = self.cell(pts);
		self.ready.push_back(Record::new(self.sequence, group, wire(pts)));
		self.sequence += 1;
	}

	/// Whether a group starting at `pts` in grid cell `cell` ends the segment that started at
	/// `start`.
	fn boundary(&mut self, start: Timestamp, pts: Timestamp, cell: u128) -> bool {
		// A cut declared after its segment had already started is spent.
		while self
			.cuts
			.front()
			.is_some_and(|cut| cut.as_micros() <= start.as_micros())
		{
			self.cuts.pop_front();
		}
		match self.cuts.front() {
			Some(cut) if cut.as_micros() <= pts.as_micros() => true,
			_ => !self.manual && cell > self.cell,
		}
	}

	/// Report that the content extends to `pts`, without a frame starting there.
	///
	/// A frame report says where content starts; the stop after the last segment would otherwise
	/// land at its last frame's start.
	pub fn end(&mut self, pts: Timestamp) {
		if !self.closed {
			self.advance(pts);
		}
	}

	/// Content in the open segment reached `pts`.
	fn advance(&mut self, pts: Timestamp) {
		if self.open.is_some()
			&& self
				.frontier
				.is_none_or(|frontier| pts.as_micros() > frontier.as_micros())
		{
			self.frontier = Some(pts);
		}
	}

	/// Declare a boundary at `pts`: the segment ends at the first group starting at or after it.
	///
	/// The first cut takes over from grid pacing for good, since the grid would otherwise end a
	/// segment just before the caller declares where it really ends. A cut at or before an earlier
	/// one is ignored, so several sources declaring the same boundaries cost nothing.
	///
	/// Only the earliest and latest pending cuts are kept: a source declaring segments faster than
	/// it starts groups (an fMP4 `styp` without a sync sample) would otherwise queue them without
	/// bound. A later group may still start before the earliest, since nothing keeps a keyframe's
	/// pts from dipping below earlier content, so neither end is safe to drop; a dropped middle one
	/// only lengthens a segment, which the ceiling still caps.
	pub fn cut(&mut self, pts: Timestamp) {
		if self.closed {
			return;
		}
		self.manual = true;
		if self.cut.is_some_and(|cut| pts.as_micros() <= cut.as_micros()) {
			return;
		}
		self.cut = Some(pts);
		match self.cuts.len() {
			0 | 1 => self.cuts.push_back(pts),
			_ => *self.cuts.back_mut().expect("two pending cuts") = pts,
		}
	}

	/// Content stopped before group `group`: publish a zero-duration record where it reached.
	///
	/// The next group starts a new segment. Nothing happens while no segment is open.
	pub fn stop(&mut self, group: u64) {
		if !self.closed {
			self.stop_at(group);
		}
	}

	fn stop_at(&mut self, group: u64) {
		let Some(start) = self.open.take() else {
			return;
		};
		// One stopped early says nothing about the cadence, so the target falls back to the grid.
		self.declared.get_or_insert(self.grid);
		let end = self.frontier.take().unwrap_or(start);
		self.ready
			.push_back(Record::new(self.sequence, group, wire(end)).with_duration(0));
		self.sequence += 1;
	}

	/// Stop after the newest group, then ignore every later report.
	pub fn close(&mut self) {
		if let Some(last) = self.last {
			self.stop_at(last + 1);
		}
		self.closed = true;
	}
}

/// Yields each record once; `None` means none is ready yet, not that none ever will be.
impl Iterator for Segmenter {
	type Item = Record;

	fn next(&mut self) -> Option<Record> {
		self.ready.pop_front()
	}
}

/// Publishes one track's records onto its HLS record track, for a publisher that segments
/// itself; a [`catalog::Producer`](crate::catalog::Producer) records them for you.
///
/// Records must be pushed in sequence order, starting at the window's next index.
pub struct Producer {
	sink: moq_json::window::Producer<Record>,
	/// The record track, to abort.
	track: moq_net::track::Producer,
}

impl Producer {
	/// The properties a record track is created with.
	pub fn info() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_priority(hang::catalog::PRIORITY.catalog)
			// A joiner asks for a fresh group once the ops outweigh eight headers.
			.with_refresh(8)
	}

	/// Publish onto `track`, starting at record zero.
	pub fn new(track: moq_net::track::Producer) -> Self {
		let config = moq_json::window::ProducerConfig::default().with_checkpoint_records(CHECKPOINT_RECORDS);
		Self {
			sink: moq_json::window::Producer::new(track.clone(), config),
			track,
		}
	}

	/// Append `record`, which must be the window's next sequence.
	pub fn push(&mut self, record: &Record) -> crate::Result<()> {
		let next = self.sink.range().end;
		if record.sequence != next {
			return Err(crate::Error::HlsSequence {
				expected: next,
				actual: record.sequence,
			});
		}
		self.refresh()?;
		self.sink
			.push(moq_net::Timed::from(record).at(moq_net::Timestamp::now()))
			.map_err(json)
	}

	/// Finish the record track.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.sink.finish().map_err(json)
	}

	/// Abort the record track with `err`, so readers see the cause rather than a complete record.
	pub(crate) fn abort(self, err: moq_net::Error) {
		// Idempotent: a track already closed keeps its first reason.
		let _ = self.track.abort(err);
	}

	/// Restate the window in a fresh group if a joiner wants one.
	///
	/// Checked on each edit rather than driven by a task, since nothing here owns a runtime. A
	/// record track gains a record every segment, so a joiner waits at most about one segment.
	fn refresh(&mut self) -> crate::Result<()> {
		match self.sink.poll_refresh(&kio::Waiter::noop()) {
			Poll::Ready(Err(err)) => Err(json(err)),
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

/// Runs once, with the nominal duration, when an unhinted track learns it.
type OnDeclare = Box<dyn FnOnce(Duration) + Send>;

/// A live track's segmenter and the record track it publishes to.
struct Live {
	segmenter: Segmenter,
	/// Publishes an unhinted declaration with the first complete segment.
	on_declare: Option<OnDeclare>,
	/// `None` once a publish failed: the records are an optional sidecar, so they stop instead.
	output: Option<Producer>,
	/// Whether a [`Recorder`] owns the segmenter, so a second enrollment can't share it.
	recording: bool,
}

/// A declaration a [`Live::publish`] left for its caller to announce once its lock is released:
/// the callback takes the catalog's lock, which is taken before ours.
#[must_use]
struct Published {
	declare: Option<(OnDeclare, Duration)>,
}

impl Published {
	/// Announce any declaration.
	fn fire(self) {
		if let Some((on_declare, duration)) = self.declare {
			on_declare(duration);
		}
	}
}

impl Live {
	/// Publish ready records.
	fn publish(&mut self) -> Published {
		for record in self.segmenter.by_ref() {
			let Some(output) = self.output.as_mut() else {
				continue;
			};
			if let Err(err) = output.push(&record) {
				tracing::warn!(%err, "HLS record publish failed; aborting the record track");
				// Aborted rather than finished: a clean end reads as a complete recording.
				if let Some(output) = self.output.take() {
					output.abort(err.reason());
				}
			}
		}
		let declare = match self.segmenter.declared() {
			Some(duration) => self.on_declare.take().map(|on_declare| (on_declare, duration)),
			None => None,
		};
		Published { declare }
	}

	/// Abort the record track with `err`, recording nothing more.
	fn abort(&mut self, err: moq_net::Error) {
		self.on_declare = None;
		self.segmenter.closed = true;
		if let Some(output) = self.output.take() {
			output.abort(err);
		}
	}

	fn finish(&mut self) {
		// Nothing advertises a declaration once the tracks finish.
		self.on_declare = None;
		self.segmenter.close();
		let _ = self.publish();
		if let Some(output) = self.output.as_mut() {
			let _ = output.finish();
		}
	}
}

struct State {
	/// Each enrolled track's record track name and live state, by media track name. Kept after its
	/// recorder drops, so a re-enrolled track continues its numbering.
	tracks: BTreeMap<String, (String, Arc<Mutex<Live>>)>,
	finished: bool,
}

/// A broadcast's HLS record tracks, one per enrolled rendition.
///
/// Each enrollment creates the track's record track and returns the [`Recorder`] its producer
/// reports through. `Clone` shares the set.
#[derive(Clone)]
pub(crate) struct Tracks {
	broadcast: moq_net::broadcast::Producer,
	state: Arc<Mutex<State>>,
}

impl Tracks {
	/// Record tracks for tracks of `broadcast`.
	pub fn new(broadcast: &moq_net::broadcast::Producer) -> Self {
		Self {
			broadcast: broadcast.clone(),
			state: Arc::new(Mutex::new(State {
				tracks: BTreeMap::new(),
				finished: false,
			})),
		}
	}

	/// Enroll the track `name`, its segments cut by `config`.
	///
	/// Creates its record track, named by [`hang::hls::default_name`], on first enrollment.
	/// Enrolling a name again after its recorder dropped continues its numbering for a new
	/// producer. Errors when the name's recorder is still alive, the record track name is taken, or
	/// the tracks finished.
	pub fn track(&self, name: &str, config: Config) -> crate::Result<Recorder> {
		let mut state = self.state.lock().unwrap();
		if state.finished {
			return Err(moq_net::Error::Closed.into());
		}
		let segmenter = |sequence: u64| Segmenter::new(config.clone()).with_sequence(sequence);

		let live = match state.tracks.get(name) {
			Some((_, existing)) => {
				// A re-enrolled track is a new producer whose group sequences may restart, so it gets a
				// fresh segmenter continuing the numbering and the declaration already advertised.
				let mut live = existing.lock().unwrap();
				if live.recording {
					return Err(moq_net::Error::Duplicate.into());
				}
				let declared = live.segmenter.declared();
				live.segmenter = segmenter(live.segmenter.sequence());
				if declared.is_some() {
					live.segmenter.declared = declared;
				}
				live.recording = true;
				drop(live);
				existing.clone()
			}
			None => {
				let track = self
					.broadcast
					.create_track(hang::hls::default_name(name).as_str(), Producer::info())?;
				let live = Arc::new(Mutex::new(Live {
					segmenter: segmenter(0),
					on_declare: None,
					output: Some(Producer::new(track)),
					recording: true,
				}));
				state
					.tracks
					.insert(name.to_string(), (hang::hls::default_name(name), live.clone()));
				live
			}
		};
		Ok(Recorder { live })
	}

	/// The record track and nominal duration advertised for `name`, once its duration is known.
	pub fn hls(&self, name: &str) -> Option<hang::catalog::Hls> {
		let state = self.state.lock().unwrap();
		let (track, live) = state.tracks.get(name)?;
		let duration = live.lock().unwrap().segmenter.declared()?;
		Some(hang::catalog::Hls::new(track.clone(), round(duration)))
	}

	/// Stop every open segment now, publishing any declaration it fixes.
	pub fn flush(&self) {
		let published: Vec<_> = {
			let state = self.state.lock().unwrap();
			state
				.tracks
				.values()
				.map(|(_, live)| {
					let mut live = live.lock().unwrap();
					if let Some(last) = live.segmenter.last {
						live.segmenter.stop(last + 1);
					}
					live.publish()
				})
				.collect()
		};
		for published in published {
			published.fire();
		}
	}

	/// Finish the record track of `name`, whose recorder dropped and whose owner never enrolls the
	/// name again, and forget it, so a producer minting a name per track holds only its live ones.
	///
	/// Readers already on the record track read it to its end. Nothing happens for a name never
	/// enrolled; a name whose recorder is still alive errors.
	pub fn retire(&self, name: &str) -> crate::Result<()> {
		let mut state = self.state.lock().unwrap();
		let Some((_, live)) = state.tracks.get(name) else {
			return Ok(());
		};
		let mut live = live.lock().unwrap();
		if live.recording {
			return Err(moq_net::Error::Duplicate.into());
		}
		live.finish();
		drop(live);
		state.tracks.remove(name);
		Ok(())
	}

	/// How many record tracks are held, live or kept for a re-enrollment.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.state.lock().unwrap().tracks.len()
	}

	/// Abort every record track with `err`. Later enrollments fail.
	pub fn abort(&self, err: &moq_net::Error) {
		let mut state = self.state.lock().unwrap();
		state.finished = true;
		for (_, live) in state.tracks.values() {
			live.lock().unwrap().abort(err.clone());
		}
	}

	/// Stop every open segment and finish every record track. Later enrollments fail.
	pub fn finish(&self) {
		let mut state = self.state.lock().unwrap();
		state.finished = true;
		for (_, live) in state.tracks.values() {
			live.lock().unwrap().finish();
		}
	}
}

/// A nominal duration as the catalog advertises it, in whole wire units rounded up.
fn round(duration: Duration) -> Duration {
	Duration::from_millis(units(duration))
}

/// Reports one track's frames into its live record track.
///
/// Move-only: it is the track's reporting handle, minted by [`Tracks::track`] and held by a
/// rendition's [`container::Producer`](crate::container::Producer). Dropping it stops the open
/// segment; the record track stays open until [`Tracks::finish`].
pub(crate) struct Recorder {
	live: Arc<Mutex<Live>>,
}

impl Recorder {
	/// The track's fixed nominal segment duration, once known.
	pub fn declared(&self) -> Option<Duration> {
		self.live.lock().unwrap().segmenter.declared()
	}

	/// Run `callback` once when an unhinted track learns its fixed duration.
	pub fn on_declare(&mut self, callback: impl FnOnce(Duration) + Send + 'static) {
		self.live.lock().unwrap().on_declare = Some(Box::new(callback));
	}

	fn report(&mut self, f: impl FnOnce(&mut Live)) {
		let published = {
			let mut live = self.live.lock().unwrap();
			f(&mut live);
			live.publish()
		};
		published.fire();
	}

	/// Report a frame of group `group`, presented at `pts`; see [`Segmenter::frame`].
	///
	/// A report the segmenter cannot place ends the record track with an error rather than
	/// publishing records that silently omit frames. Media publishing is unaffected.
	pub fn frame(&mut self, group: u64, pts: Timestamp) {
		self.report(|live| {
			if let Err(err) = live.segmenter.frame(group, pts) {
				tracing::warn!(%err, "HLS report rejected; aborting the record track");
				live.abort(err.reason());
			}
		});
	}

	/// Report that the content extends to `pts`; see [`Segmenter::end`].
	pub fn end(&mut self, pts: Timestamp) {
		self.report(|live| live.segmenter.end(pts));
	}

	/// Report that content stopped before group `group`; see [`Segmenter::stop`].
	pub fn stop(&mut self, group: u64) {
		self.report(|live| live.segmenter.stop(group));
	}

	/// Declare a boundary at `pts` on this track; see [`Segmenter::cut`].
	pub fn cut(&mut self, pts: Timestamp) {
		self.report(|live| live.segmenter.cut(pts));
	}
}

impl Drop for Recorder {
	fn drop(&mut self) {
		self.report(|live| {
			if let Some(last) = live.segmenter.last {
				live.segmenter.stop(last + 1);
			}
			live.recording = false;
		});
		// The callback holds the catalog, which holds these tracks: an enrollment that ends without
		// declaring must drop it, or the broadcast never closes.
		self.live.lock().unwrap().on_declare = None;
	}
}

/// One decoded HLS record, with real [`Timestamp`]s.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Entry {
	/// The segment's number, consecutive within its track.
	pub sequence: u64,

	/// The group the segment starts with.
	pub group: u64,

	/// The segment's start.
	pub pts: Timestamp,

	/// The segment's duration, when it does not run to the next record's pts. Zero marks where
	/// content stopped: the segment holds no media.
	pub duration: Option<Duration>,
}

impl Entry {
	/// Decode a record, converting its timing out of milliseconds.
	///
	/// A pts the timescale can't represent is an error rather than a substituted value: silently
	/// moving a timestamp would misdirect the playlist.
	pub fn decode(record: Record) -> crate::Result<Self> {
		Ok(Self {
			sequence: record.sequence,
			group: record.group,
			pts: Timestamp::new(record.pts, TIMESCALE)?,
			duration: record.duration.map(Duration::from_millis),
		})
	}

	/// Whether the segment holds no media: content stopped at its start.
	pub fn is_stop(&self) -> bool {
		self.duration.is_some_and(|duration| duration.is_zero())
	}
}

/// One change to the visible record window.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
	/// A record became visible at this window index, which equals its sequence.
	Push {
		/// Absolute window index assigned to the record.
		index: u64,
		/// The decoded record.
		entry: Entry,
	},
	/// These previously visible window indices were removed.
	Pop(std::ops::Range<u64>),
	/// These indices were removed before this consumer saw them.
	Skip(std::ops::Range<u64>),
}

/// Reads one rendition's HLS records, yielding decoded [`Event`]s in window order.
pub struct Consumer {
	inner: moq_json::window::Consumer<Record>,
}

impl Consumer {
	/// Subscribe to the record track a rendition's [`hls`](hang::catalog::Hls) field names.
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer, hls: &hang::catalog::Hls) -> crate::Result<Self> {
		let track = broadcast.track(&hls.track)?.subscribe(None).await?;
		Ok(Self {
			inner: moq_json::window::Consumer::new(track, moq_json::window::ConsumerConfig::default()),
		})
	}

	fn decode_event(event: moq_json::window::Event<Record>) -> crate::Result<Event> {
		Ok(match event {
			moq_json::window::Event::Push { index, value } => {
				// The sequence is the segment's URL and media sequence, so one that disagrees with
				// its position would list two segments under one number.
				if value.sequence != index {
					return Err(crate::Error::HlsSequence {
						expected: index,
						actual: value.sequence,
					});
				}
				Event::Push {
					index,
					entry: Entry::decode(value)?,
				}
			}
			moq_json::window::Event::Pop(range) => Event::Pop(range),
			moq_json::window::Event::Skip(range) => Event::Skip(range),
			_ => unreachable!("unknown record window event"),
		})
	}

	/// Get the next window event, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Event>> {
		match self.inner.next().await? {
			Some(event) => Ok(Some(Self::decode_event(event.value)?)),
			None => Ok(None),
		}
	}

	/// Poll for the next window event, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event>>> {
		match self.inner.poll_next(waiter)? {
			// Each record carries its own pts, so the frame timestamp adds nothing.
			Poll::Ready(Some(event)) => Poll::Ready(Self::decode_event(event.value).map(Some)),
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Media whose encoder reports a 4s cadence, so it declares before any segment.
	fn hinted() -> Config {
		Config::default().with_duration_hint(Duration::from_secs(4))
	}

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	/// Each record as `(group, pts, duration)` in wire units, checking the numbering.
	fn drain(segmenter: &mut Segmenter) -> Vec<(u64, u64, Option<u64>)> {
		let mut out = Vec::new();
		for record in segmenter.by_ref() {
			assert_eq!(
				record.sequence,
				out.len() as u64 + first(&out),
				"records are numbered consecutively"
			);
			out.push((record.group, record.pts, record.duration));
		}
		out
	}

	fn first(_: &[(u64, u64, Option<u64>)]) -> u64 {
		0
	}

	/// Groups `range`, one frame each, `step` ms apart.
	fn groups(segmenter: &mut Segmenter, range: std::ops::Range<u64>, step: u64) {
		for group in range {
			segmenter.frame(group, ms(group * step)).unwrap();
		}
	}

	#[test]
	fn long_groups_get_a_segment_each() {
		let mut segmenter = Segmenter::new(Config::default());
		groups(&mut segmenter, 0..3, 5_000);
		assert_eq!(
			drain(&mut segmenter),
			[(0, 0, None), (1, 5_000, None), (2, 10_000, None)]
		);
		assert_eq!(segmenter.declared(), Some(Duration::from_secs(5)));
	}

	#[test]
	fn short_groups_pack_into_a_grid_cell() {
		let mut segmenter = Segmenter::new(Config::default());
		groups(&mut segmenter, 0..10, 1_000);
		assert_eq!(
			drain(&mut segmenter),
			[(0, 0, None), (4, 4_000, None), (8, 8_000, None)]
		);
	}

	#[test]
	fn a_hint_declares_its_smallest_multiple_at_or_past_the_duration() {
		let segmenter = Segmenter::new(Config::default().with_duration_hint(Duration::from_millis(1_500)));
		assert_eq!(segmenter.declared(), Some(Duration::from_millis(4_500)));
		assert_eq!(Segmenter::new(hinted()).declared(), Some(Duration::from_secs(4)));
	}

	#[test]
	fn a_stopped_first_segment_declares_the_grid() {
		let mut segmenter = Segmenter::new(Config::default());
		groups(&mut segmenter, 0..2, 1_000);
		segmenter.close();
		assert_eq!(segmenter.declared(), Some(DEFAULT_DURATION));
		assert_eq!(drain(&mut segmenter), [(0, 0, None), (2, 1_000, Some(0))]);
	}

	#[test]
	fn the_end_extends_the_stop() {
		let mut segmenter = Segmenter::new(hinted());
		groups(&mut segmenter, 0..2, 1_000);
		segmenter.end(ms(2_000));
		segmenter.close();
		assert_eq!(drain(&mut segmenter), [(0, 0, None), (2, 2_000, Some(0))]);
		// Nothing reports after the close.
		segmenter.frame(5, ms(9_000)).unwrap();
		assert!(drain(&mut segmenter).is_empty());
	}

	#[test]
	fn a_skipped_sequence_stops_the_segment() {
		let mut segmenter = Segmenter::new(hinted());
		groups(&mut segmenter, 0..2, 1_000);
		segmenter.end(ms(1_500));
		segmenter.frame(5, ms(60_000)).unwrap();
		assert_eq!(
			drain(&mut segmenter),
			[(0, 0, None), (2, 1_500, Some(0)), (5, 60_000, None)]
		);
	}

	#[test]
	fn a_stop_ends_the_segment_and_the_next_group_starts_one() {
		let mut segmenter = Segmenter::new(hinted());
		groups(&mut segmenter, 0..2, 1_000);
		segmenter.end(ms(2_000));
		segmenter.stop(2);
		segmenter.frame(3, ms(10_000)).unwrap();
		assert_eq!(
			drain(&mut segmenter),
			[(0, 0, None), (2, 2_000, Some(0)), (3, 10_000, None)]
		);
	}

	#[test]
	fn the_ceiling_ends_a_segment_waiting_on_cuts() {
		let mut segmenter = Segmenter::new(hinted());
		segmenter.cut(ms(100_000));
		groups(&mut segmenter, 0..20, 1_000);
		// Three declared durations: the source's cut never came.
		assert_eq!(drain(&mut segmenter), [(0, 0, None), (12, 12_000, None)]);
	}

	#[test]
	fn explicit_cuts_override_the_grid() {
		let mut segmenter = Segmenter::new(Config::default());
		for group in 0..5u64 {
			if matches!(group, 2 | 3) {
				segmenter.cut(ms(group * 1_000));
			}
			segmenter.frame(group, ms(group * 1_000)).unwrap();
		}
		assert_eq!(
			drain(&mut segmenter),
			[(0, 0, None), (2, 2_000, None), (3, 3_000, None)]
		);
	}

	/// A hostile source declaring `styp` segments that start no group (no sync sample) keeps the
	/// earliest and latest cuts, which the next groups meet.
	#[test]
	fn styp_segments_without_a_sync_sample_hold_two_cuts() {
		let mut segmenter = Segmenter::new(Config::default());
		segmenter.frame(0, ms(0)).unwrap();
		for step in 1..1_000u64 {
			segmenter.cut(ms(step));
			segmenter.frame(0, ms(step)).unwrap();
			assert!(segmenter.cuts.len() <= 2, "step {step}");
		}
		assert_eq!(segmenter.cuts, [ms(1), ms(999)]);
		segmenter.frame(1, ms(1_000)).unwrap();
		assert!(segmenter.cuts.is_empty());
		assert_eq!(drain(&mut segmenter), [(0, 0, None), (1, 1_000, None)]);
	}

	/// A group may start before the newest cut, so a cut it passed and the one past it both stand:
	/// the group at 15 ends the segment on the cut at 10, and the group at 25 on the cut at 20.
	#[test]
	fn a_group_between_cuts_keeps_the_later_one() {
		let mut segmenter = Segmenter::new(Config::default());
		segmenter.frame(0, ms(0)).unwrap();
		segmenter.cut(ms(10));
		segmenter.frame(0, ms(10)).unwrap();
		segmenter.cut(ms(20));
		segmenter.frame(0, ms(20)).unwrap();
		segmenter.frame(1, ms(15)).unwrap();
		segmenter.frame(2, ms(25)).unwrap();
		assert_eq!(drain(&mut segmenter), [(0, 0, None), (1, 15, None), (2, 25, None)]);
	}

	/// A segment spends the cuts it reached however it starts: on a cut, at the ceiling, or after
	/// a skipped group.
	#[test]
	fn every_segment_start_spends_its_cuts() {
		let mut segmenter = Segmenter::new(Config::default());
		segmenter.frame(0, ms(0)).unwrap();
		segmenter.cut(ms(1_000));
		segmenter.frame(1, ms(1_000)).unwrap();
		// A one-second declaration puts the ceiling at three, so each later group ends a segment there.
		for group in 2..100u64 {
			let pts = ms(1_000 + (group - 1) * 3_000);
			segmenter.cut(pts);
			segmenter.frame(group, pts).unwrap();
			assert!(segmenter.cuts.is_empty(), "group {group}");
		}
		segmenter.cut(ms(1_000_000));
		segmenter.frame(200, ms(1_000_000)).unwrap();
		assert!(segmenter.cuts.is_empty(), "after a skipped group");
	}

	#[test]
	fn a_report_that_goes_backwards_is_refused() {
		let mut segmenter = Segmenter::new(hinted());
		segmenter.frame(3, ms(0)).unwrap();
		segmenter.frame(3, ms(10)).unwrap();
		assert!(matches!(
			segmenter.frame(2, ms(20)),
			Err(crate::Error::HlsGroup { group: 2, last: 3 })
		));
	}

	#[test]
	fn an_uneven_gop_cuts_on_the_grid() {
		let mut segmenter = Segmenter::new(Config::default());
		// 3s GOPs on a 4s grid: a group starts a segment only in a later cell than the one before.
		groups(&mut segmenter, 0..9, 3_000);
		assert_eq!(
			drain(&mut segmenter),
			[
				(0, 0, None),
				(2, 6_000, None),
				(3, 9_000, None),
				(4, 12_000, None),
				(6, 18_000, None),
				(7, 21_000, None),
				(8, 24_000, None)
			]
		);
		assert_eq!(segmenter.declared(), Some(Duration::from_secs(6)));
	}

	// Renditions share no state, yet cut the same boundaries when their groups start on the same
	// grid lines, and one that starts late lines up from its next grid line.
	#[tokio::test]
	async fn renditions_cut_alike_without_sharing_state() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let tracks = Tracks::new(&broadcast);
		let mut video = tracks.track("video", hinted()).unwrap();
		let mut audio = tracks.track("audio", hinted()).unwrap();
		let mut late = tracks.track("late", hinted()).unwrap();
		// Audio runs ahead of video, grouped every 20ms.
		for step in 0..450u64 {
			let pts = step * 20;
			audio.frame(step, ms(pts + 5));
			if pts >= 5_000 {
				late.frame(step, ms(pts + 5));
			}
			if pts % 2_000 == 0 {
				video.frame(pts / 2_000, ms(pts));
			}
		}
		drop((video, audio, late));
		tracks.finish();

		let consumer = broadcast.consume();
		let read = async |name: &str| {
			let hls = tracks.hls(name).unwrap();
			let mut consumer = Consumer::subscribe(&consumer, &hls).await.unwrap();
			let mut out = Vec::new();
			while let Some(event) = consumer.next().await.unwrap() {
				let Event::Push { entry, .. } = event else {
					panic!("unexpected event {event:?}");
				};
				out.push((entry.sequence, Duration::from(entry.pts).as_millis(), entry.is_stop()));
			}
			out
		};
		assert_eq!(
			read("video").await,
			[(0, 0, false), (1, 4_000, false), (2, 8_000, false), (3, 8_000, true)]
		);
		assert_eq!(
			read("audio").await,
			[(0, 5, false), (1, 4_005, false), (2, 8_005, false), (3, 8_985, true)]
		);
		assert_eq!(
			read("late").await,
			[(0, 5_005, false), (1, 8_005, false), (2, 8_985, true)]
		);
	}

	#[test]
	fn an_unhinted_track_is_advertised_once_declared() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let tracks = Tracks::new(&broadcast);
		let mut recorder = tracks.track("video", Config::default()).unwrap();
		recorder.frame(0, ms(0));
		assert_eq!(tracks.hls("video"), None);
		recorder.frame(1, ms(5_000));
		assert_eq!(
			tracks.hls("video"),
			Some(hang::catalog::Hls::new("video.hls", Duration::from_secs(5)))
		);
	}

	#[tokio::test]
	async fn a_rejected_report_fails_the_record_track() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let tracks = Tracks::new(&broadcast);
		let mut recorder = tracks.track("video", hinted()).unwrap();
		let hls = tracks.hls("video").unwrap();
		let mut consumer = Consumer::subscribe(&broadcast.consume(), &hls).await.unwrap();
		recorder.frame(1, ms(0));
		recorder.frame(0, ms(1_000));
		let mut next = consumer.next().await;
		while let Ok(Some(_)) = next {
			next = consumer.next().await;
		}
		// Aborted with a cause, not dropped, so a reader can tell a refused report from a gone
		// publisher.
		let err = next.expect_err("the track fails rather than ends");
		assert!(
			matches!(err.reason(), moq_net::Error::Stream(moq_net::StreamError::Internal)),
			"{err:?}"
		);
	}

	#[tokio::test]
	async fn re_enrolling_continues_the_numbering() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let tracks = Tracks::new(&broadcast);
		let mut recorder = tracks.track("video", hinted()).unwrap();
		assert!(tracks.track("video", hinted()).is_err(), "its recorder is still alive");
		recorder.frame(0, ms(0));
		drop(recorder);
		let mut recorder = tracks.track("video", hinted()).unwrap();
		// The new producer restarts its group sequences.
		recorder.frame(0, ms(10_000));
		drop(recorder);
		tracks.finish();

		let hls = tracks.hls("video").unwrap();
		let mut consumer = Consumer::subscribe(&broadcast.consume(), &hls).await.unwrap();
		let mut records = Vec::new();
		while let Some(Event::Push { entry, .. }) = consumer.next().await.unwrap() {
			records.push((entry.sequence, entry.group, entry.is_stop()));
		}
		assert_eq!(records, [(0, 0, false), (1, 1, true), (2, 0, false), (3, 1, true)]);
	}

	#[test]
	fn a_taken_record_track_name_fails_enrollment() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _taken = broadcast.create_track("video.hls", None).unwrap();
		let tracks = Tracks::new(&broadcast);
		assert!(tracks.track("video", Config::default()).is_err());
	}

	#[test]
	fn enrollment_after_finish_is_refused() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let tracks = Tracks::new(&broadcast);
		tracks.finish();
		assert!(tracks.track("video", Config::default()).is_err());
	}

	#[tokio::test]
	async fn a_record_out_of_place_is_refused() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video.hls", Producer::info()).unwrap();
		let mut sink = moq_json::window::Producer::<Record>::new(track, moq_json::window::ProducerConfig::default());
		sink.push(moq_net::Timed::from(&Record::new(1, 0, 0)).at(Timestamp::ZERO))
			.unwrap();
		let hls = hang::catalog::Hls::new("video.hls", Duration::from_secs(4));
		let mut consumer = Consumer::subscribe(&broadcast.consume(), &hls).await.unwrap();
		assert!(matches!(
			consumer.next().await,
			Err(crate::Error::HlsSequence { expected: 0, actual: 1 })
		));
	}
}

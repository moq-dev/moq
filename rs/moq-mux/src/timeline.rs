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
//! The write side splits into facts and policy, meeting in the shared [`Producer`]:
//!
//! - **Tracks report facts.** Each media track enrolls via [`Producer::track`], and its
//!   [`Recorder`] reports every group open (sequence, timestamp, keyframe) plus where its
//!   content ends. A container import never decides where segments fall; it only states what
//!   it published.
//! - **The timeline sets policy.** A segment ends at the first group boundary that gives it at
//!   least [`Config::duration_min`] on every enrolled track (see [`Config`] for the exact
//!   rule). An application that knows its own boundaries overrides that with [`Producer::cut`].
//! - **Records flush on completeness.** A segment's record is published only once every
//!   enrolled track has reported a group at or past the segment's end (or closed), proving the
//!   segment's group ranges are final on every track. The record is then self-contained and
//!   immediately servable. [`Producer::reserve`] extends that across a batch of enrollments,
//!   the way the catalog's own reservation does.
//!
//! Alignment falls out of construction: every track maps its groups onto the same boundary
//! list, so segment N covers the same span of content time on every track, which is what HLS
//! requires of switchable renditions.
//!
//! ## Wiring
//!
//! [`catalog::Producer`](crate::catalog::Producer) owns the broadcast's timeline (the catalog
//! is what owns the broadcast's shape) and wires all of this up:
//! [`media_producer`](crate::catalog::Producer::media_producer) enrolls the track, which
//! creates the timeline track on first use and advertises it in the catalog's root
//! [`hang::catalog::Timeline`] section. A broadcast that never enrolls a track publishes no
//! timeline at all: segmentation is opt-in per broadcast, never per track.
//!
//! ## The timeline slides
//!
//! It indexes what the publisher can still *serve*, not everything it ever published. A record
//! is retracted once the groups behind it reach their track's
//! [`latency_max`](moq_net::track::Info::latency_max), which is why [`Producer::track`] takes
//! the track itself: reading the window from the same place the track declares it leaves nothing
//! for a caller to duplicate and get wrong. A segment covers every enrolled track, so the
//! earliest deadline among the groups it names bounds it, and tracks with different retention
//! need no special rule.
//!
//! The retraction *leads* the eviction it predicts. moq-net keeps a group for `latency_max` plus
//! a grace period, so a consumer that acts on the last record it saw still finds the media when
//! its FETCH lands a round trip later. Both sweeps run as content is written, so a publisher
//! that stalls stops trimming and stops evicting together, with no timer on either side.
//!
//! On the read side, [`Consumer::subscribe`] reads the timeline straight from the catalog's
//! [`hang::catalog::Timeline`] section (so the track name and timescale can't be mismatched)
//! and yields [`Update`]s: a decoded [`Entry`] per segment that becomes available, and a
//! retraction as older ones expire. A late joiner sees the live window rather than the whole
//! broadcast. On the wire the track is a DEFLATE-compressed [`moq_json::window`] (see
//! [`hang::timeline`] for the record schema).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, SystemTime};

// The same clock moq-net's own age eviction runs on, so the two agree (and a test that pauses
// time moves both). `std::time::Instant` would drift from it under wasm and under a test clock.
use web_async::time::Instant;

use hang::catalog::Timeline;
use hang::timeline::{DEFAULT_NAME, Range, Record, RecordExt};

use moq_net::{Timescale, Timestamp};

/// The conventional [`Config::duration_min`] (1 second), for callers with no opinion.
pub const DEFAULT_DURATION_MIN: Duration = Duration::from_secs(1);

/// How a [`Producer`] paces its segments.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The shortest a segment may be.
	///
	/// A segment ends at the first point that is a group boundary on every enrolled track and
	/// at least this far past the segment's start, so the track with the coarsest groups paces
	/// the broadcast. A 2 second GOP against a 1 second minimum yields 2 second segments; a
	/// real-time encoder with a 10 minute GOP yields one 10 minute segment, because there is
	/// nowhere else a segment could start and stay decodable.
	///
	/// A floor rather than a target on purpose: a floor is always satisfiable (wait longer),
	/// while a ceiling is not (a single group longer than it can't be split).
	pub duration_min: Duration,

	/// The longest a segment may be, advertised in the catalog when set.
	///
	/// Set it only when the publisher can actually promise one, i.e. it controls the encoder's
	/// keyframe cadence. Consumers that need a bound up front use it (an HLS exporter's
	/// `EXT-X-TARGETDURATION`), so it is a contract rather than a hint: a segment that would
	/// exceed it fails the timeline instead of publishing a record that contradicts the
	/// catalog. Leave it `None` when the media decides, which is the common case for real-time
	/// and for anything importing a source it doesn't control.
	pub duration_max: Option<Duration>,

	/// The wall-clock time of `pts` 0, advertised in the catalog when set.
	///
	/// It anchors content time to an absolute clock, which is what an HLS
	/// `EXT-X-PROGRAM-DATE-TIME` or a DASH `availabilityStartTime` needs. A publisher stamping
	/// timestamps from [`catalog::Producer::timestamp`](crate::catalog::Producer::timestamp) has
	/// its `pts` 0 at construction, so `Some(SystemTime::now())` is the live answer; a recording
	/// or an import passes the content's real start instead.
	///
	/// Clamped to the moq epoch ([`MOQ_EPOCH_UNIX_MILLIS`](hang::catalog::MOQ_EPOCH_UNIX_MILLIS),
	/// 2020), which the wire format measures from: an earlier time isn't representable.
	pub wall: Option<SystemTime>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			duration_min: DEFAULT_DURATION_MIN,
			duration_max: None,
			wall: None,
		}
	}
}

/// One group open reported by a track and not yet flushed into a record.
struct Pending {
	/// The group's sequence, as used by FETCH/SUBSCRIBE on the media track.
	sequence: u64,
	/// Where the group starts.
	pts: Timestamp,
	/// Whether its first frame is a keyframe.
	keyframe: bool,
	/// When the report arrived, which is when the publisher created the group. The clock the
	/// group's cache lifetime runs on, and therefore the clock its record's does too.
	reported: Instant,
}

/// One enrolled track's report state.
struct TrackState {
	/// Group opens reported and not yet flushed into a record.
	pending: VecDeque<Pending>,
	/// The newest reported timestamp: everything earlier is known, which is what lets a
	/// segment ending at or before it flush. Advanced by a group open (the group starts there)
	/// and by [`Recorder::end`] (the content stops there).
	frontier: Option<Timestamp>,
	/// The recorder was dropped; this track no longer paces boundaries or gates completeness.
	closed: bool,
	/// How long this track's publisher guarantees to keep a group cached
	/// ([`moq_net::track::Info::latency_max`]), read at enrollment. A segment's record is
	/// retracted once the groups behind it pass this, so the timeline never lists content that
	/// can't be fetched.
	latency_max: Duration,
}

impl TrackState {
	/// The first unflushed group starting at or after `threshold` micros: this track's vote for
	/// where the open segment can end.
	fn candidate(&self, threshold: u128) -> Option<Timestamp> {
		self.pending
			.iter()
			.map(|p| p.pts)
			.find(|pts| pts.as_micros() >= threshold)
	}
}

/// The state one [`Producer`] guards.
struct State {
	config: Config,
	/// Retained so the timeline track can be created when the first media track enrolls.
	broadcast: moq_net::broadcast::Producer,
	/// The timeline track, created by the first [`Producer::track`] call.
	sink: Option<moq_json::window::Producer<Record>>,
	/// When each record still in the window stops being fetchable, oldest first. One entry per
	/// record the sink holds, so [`State::sweep`] can trim the head without decoding anything.
	expiry: VecDeque<Instant>,
	/// Where the open (unflushed) segment starts; `None` until the first report.
	start: Option<Timestamp>,
	/// Explicit [`Producer::cut`] boundaries not yet reached, in order.
	cuts: VecDeque<Timestamp>,
	/// A [`Producer::cut`] arrived, so the application owns the boundaries from here on and the
	/// `duration_min` pacing stops. Without this the pacing races ahead of a source whose
	/// segments are longer than the minimum, closing one before its real boundary is declared.
	manual: bool,
	/// The number the next flushed record gets.
	next_segment: u64,
	/// Every enrolled track, keyed by media track name.
	tracks: BTreeMap<String, TrackState>,
	/// Live [`Reserved`] handles: while any exists, no record flushes (more tracks are still
	/// enrolling). Mirrors the catalog's own reservation gate.
	reservers: usize,
	/// A segment overran [`Config::duration_max`]: `(segment, duration)`. The timeline stops
	/// publishing, since the catalog's promise is already broken.
	overrun: Option<(u64, Duration)>,
	/// The wire timescale for `pts`/`duration` (the catalog section's default: milliseconds).
	timescale: Timescale,
}

impl State {
	/// A group open reported by `name`: record the fact and publish whatever became complete.
	fn report(&mut self, name: &str, sequence: u64, pts: Timestamp, keyframe: bool) {
		let Some(track) = self.tracks.get_mut(name) else {
			return;
		};
		track.pending.push_back(Pending {
			sequence,
			pts,
			keyframe,
			reported: Instant::now(),
		});
		self.advance(name, pts);
	}

	/// `name`'s content ends at `pts`, without a group opening there. This is what gives the
	/// final segment an honest duration, since its end is not a boundary anybody cut.
	fn report_end(&mut self, name: &str, pts: Timestamp) {
		if self.tracks.contains_key(name) {
			self.advance(name, pts);
		}
	}

	/// Raise `name`'s frontier to `pts`, then publish whatever that completed.
	fn advance(&mut self, name: &str, pts: Timestamp) {
		if let Some(track) = self.tracks.get_mut(name)
			&& track.frontier.is_none_or(|f| pts.as_micros() > f.as_micros())
		{
			track.frontier = Some(pts);
		}

		// The first thing anybody reports anchors the first segment, so content produced before
		// any boundary exists belongs to the oldest segment rather than to nowhere.
		if self.start.is_none() {
			self.start = Some(pts);
		}

		self.pump(false);
	}

	/// A track's recorder was dropped: stop gating completeness on it.
	fn close(&mut self, name: &str) {
		if let Some(track) = self.tracks.get_mut(name) {
			track.closed = true;
		}
		self.pump(false);
	}

	/// Publish every segment the media has finalized.
	///
	/// `finished` is the terminal pass: no track will report again, so a track that never
	/// reached a boundary stops voting instead of holding the timeline open forever.
	fn pump(&mut self, finished: bool) {
		// Retract before publishing: a reservation withholds records that don't exist yet, it
		// doesn't resurrect ones whose content is gone.
		self.sweep();

		// With nothing enrolled there is nothing to describe. A record is immutable once
		// published, so a caller that knows more tracks are still enrolling withholds it (see
		// [`Producer::reserve`]), and an overrun has already broken the catalog's promise.
		if self.tracks.is_empty() || self.reservers > 0 || self.overrun.is_some() {
			return;
		}

		while self.close_segment(finished) {
			// A segment that broke the declared maximum ends the timeline; every later record
			// would inherit the same broken promise.
			if self.overrun.is_some() {
				break;
			}
		}
	}

	/// Publish the open segment if the media has finalized it, returning whether it did.
	fn close_segment(&mut self, finished: bool) -> bool {
		let Some(start) = self.start else {
			return false;
		};

		// Discard boundaries the timeline has already reached. Only the front is ever consulted,
		// so leaving a spent one there would block every later cut behind it and silently drop
		// the caller back to `duration_min` pacing.
		while self.cuts.front().is_some_and(|c| c.as_micros() <= start.as_micros()) {
			self.cuts.pop_front();
		}

		let Some((end, cut)) = self.boundary(start, finished) else {
			return false;
		};

		// Every open track has to have reported at or past the boundary, proving its ranges for
		// this segment are final. The track that voted for `end` has by construction; a track
		// with shorter groups can still be behind it.
		let complete = finished
			|| self
				.tracks
				.values()
				.all(|t| t.closed || t.frontier.is_some_and(|f| f.as_micros() >= end.as_micros()));
		if !complete {
			return false;
		}

		self.flush_segment(start, Some(end));
		if cut {
			self.cuts.pop_front();
		}
		self.start = Some(end);
		true
	}

	/// Where the segment starting at `start` ends, once the media says so: an explicit cut when
	/// one is registered, otherwise the first group boundary shared by every track that gives
	/// the segment its minimum duration. The bool reports which.
	fn boundary(&self, start: Timestamp, finished: bool) -> Option<(Timestamp, bool)> {
		if let Some(&cut) = self.cuts.front()
			&& cut.as_micros() > start.as_micros()
		{
			return Some((cut, true));
		}

		// The application declared a boundary at some point, so it owns them all: pacing here
		// would close a segment the caller is about to cut somewhere else.
		if self.manual {
			return None;
		}

		let threshold = start.as_micros() + self.config.duration_min.as_micros();

		let mut end: Option<Timestamp> = None;
		for track in self.tracks.values() {
			match track.candidate(threshold) {
				// The latest vote wins: it is a group boundary on the coarsest track, and every
				// finer track assigns its groups by start, so no group is split. A closed track
				// still votes: it can't report more, but the groups it did report are boundaries
				// like any other, and without them a backlog would collapse into one segment.
				Some(pts) => {
					if end.is_none_or(|e| pts.as_micros() > e.as_micros()) {
						end = Some(pts);
					}
				}
				// This track has produced nothing past the minimum yet, so ending the segment
				// would strand it. A closed track never will, and neither does anything on the
				// terminal pass, so neither one blocks.
				None if finished || track.closed => continue,
				None => return None,
			}
		}

		end.map(|end| (end, false))
	}

	/// Emit the record for the segment starting at `start`: drain every track's groups before
	/// `end` (all of them for the final, unbounded segment) into ranges.
	fn flush_segment(&mut self, start: Timestamp, end: Option<Timestamp>) {
		let pts = start.as_scale(self.timescale) as u64;
		let duration = match end {
			Some(end) => (end.as_scale(self.timescale) as u64).saturating_sub(pts),
			// The final segment has no end boundary, so it runs to the newest thing any track
			// reported: its end of content when the track reported one (a finished
			// `container::Producer` does), otherwise the last group it opened, which
			// undercounts that group's tail.
			None => self
				.tracks
				.values()
				.filter_map(|t| t.frontier)
				.map(|f| f.as_scale(self.timescale) as u64)
				.max()
				.unwrap_or(pts)
				.saturating_sub(pts),
		};

		// The catalog promised a bound and this segment breaks it, so the record would
		// contradict what consumers were told. Fail the timeline rather than publish it: a
		// declared maximum the media can't honor is a bug in the publisher, and nothing a
		// consumer reading the catalog can work around.
		if let Some(max) = self.config.duration_max
			&& duration > self.units(max)
		{
			let observed = Duration::from_nanos(duration * (1_000_000_000 / self.timescale.as_u64()));
			tracing::error!(
				segment = self.next_segment,
				duration = ?observed,
				duration_max = ?max,
				"segment exceeded the declared duration_max; dropping the timeline track"
			);
			self.overrun = Some((self.next_segment, observed));
			// End the track rather than drop it: the records published before the promise broke
			// are still true, and a consumer that has them should keep them.
			if let Some(sink) = self.sink.as_mut() {
				let _ = sink.finish();
			}
			self.sink = None;
			return;
		}

		let mut record = Record::new(self.next_segment, pts, duration);
		self.next_segment += 1;

		// When this record stops being true. A consumer fetches a segment whole, so the first
		// group to leave its publisher's cache breaks the segment as thoroughly as any other:
		// the deadline is the earliest across every group the record names, which is what makes
		// tracks with different retention fall out rather than need a rule.
		let mut expiry: Option<Instant> = None;

		for (name, track) in &mut self.tracks {
			let mut ranges: Vec<Range> = Vec::new();
			while let Some(pending) = track.pending.front() {
				if end.is_some_and(|end| pending.pts.as_micros() >= end.as_micros()) {
					break;
				}
				let pending = track.pending.pop_front().expect("just peeked");

				let deadline = pending.reported + track.latency_max;
				expiry = Some(expiry.map_or(deadline, |e: Instant| e.min(deadline)));

				match ranges.last_mut() {
					// Contiguous sequences extend the run; a skip starts a new range (a gap:
					// groups that never existed).
					Some(last) if last.end + 1 == pending.sequence => last.end = pending.sequence,
					_ => {
						let mut range = Range::new(pending.sequence, pending.sequence);
						range.keyframe = pending.keyframe;
						ranges.push(range);
					}
				}
			}
			if !ranges.is_empty() {
				record.tracks.insert(name.clone(), ranges);
			}
		}

		// A record naming no groups describes a span with no content, so nothing can evict out
		// from under it; keep it for as long as the shortest window any enrolled track declares,
		// so a gap doesn't outlive the segments around it.
		let expiry = expiry.unwrap_or_else(|| {
			let shortest = self.tracks.values().map(|t| t.latency_max).min().unwrap_or_default();
			Instant::now() + shortest
		});

		self.emit(record, expiry);
	}

	/// Retract every record whose content has left the publisher's cache.
	///
	/// Driven by the same reports that drive publishing, which is also what drives moq-net's own
	/// age eviction (it sweeps as groups are committed), so the two stay in phase without a timer:
	/// a publisher that stalls stops trimming and stops evicting together.
	///
	/// The retraction leads the eviction it predicts, because moq-net keeps a group for
	/// `latency_max` plus a grace period. A consumer that acts on the last record it saw for a
	/// segment therefore still finds the groups when its FETCH lands.
	fn sweep(&mut self) {
		let now = Instant::now();
		let expired = self.expiry.iter().take_while(|&&deadline| deadline <= now).count();
		if expired == 0 {
			return;
		}
		self.expiry.drain(..expired);

		let Some(sink) = self.sink.as_mut() else {
			return;
		};
		if let Err(err) = sink.trim(expired) {
			tracing::warn!(%err, "timeline trim failed; dropping the timeline track");
			self.sink = None;
		}
	}

	/// A duration in the wire timescale's units, rounded up so a bound never understates itself.
	fn units(&self, duration: Duration) -> u64 {
		(duration.as_micros() * self.timescale.as_u64() as u128).div_ceil(1_000_000) as u64
	}

	/// Publish a flushed record.
	///
	/// The timeline is an optional sidecar, so a transport failure logs and stops publishing
	/// rather than tearing down the media path.
	fn emit(&mut self, record: Record, expiry: Instant) {
		let Some(sink) = self.sink.as_mut() else {
			return;
		};
		if let Err(err) = sink.append(&record) {
			tracing::warn!(%err, "timeline publish failed; dropping the timeline track");
			self.sink = None;
			return;
		}
		self.expiry.push_back(expiry);
	}

	/// The terminal flush: publish what the media finalized, then the open tail.
	fn finish(&mut self) {
		// Nothing more will enroll, so an outstanding reservation has nothing left to wait for.
		self.reservers = 0;
		self.pump(true);

		if self.overrun.is_some() {
			return;
		}

		// Skip an empty tail: a boundary with no content after it describes nothing.
		if let Some(start) = self.start.take()
			&& self.tracks.values().any(|t| !t.pending.is_empty())
		{
			self.flush_segment(start, None);
		}
	}

	/// The error a segment overrunning [`Config::duration_max`] left behind, if any.
	fn failure(&self) -> Option<crate::Error> {
		let (segment, duration) = self.overrun?;
		Some(crate::Error::TimelineOverrun {
			segment,
			duration,
			duration_max: self.config.duration_max.unwrap_or_default(),
		})
	}

	/// The catalog section advertising this timeline.
	fn section(&self) -> Timeline {
		let mut section = Timeline::new(DEFAULT_NAME);
		section.timescale = self.timescale.as_u64() as u32;
		section.duration_max = self.config.duration_max.map(|max| self.units(max));
		section.wall = self.config.wall.map(|wall| {
			// The wire measures from the moq epoch rather than the Unix one, so the value stays
			// small enough for a 53-bit integer even at fine timescales.
			let unix_millis = wall
				.duration_since(SystemTime::UNIX_EPOCH)
				.unwrap_or_default()
				.as_millis();
			let moq_millis = unix_millis.saturating_sub(hang::catalog::MOQ_EPOCH_UNIX_MILLIS as u128);
			(moq_millis * self.timescale.as_u64() as u128 / 1000) as u64
		});
		section
	}
}

/// The broadcast's timeline: the shared boundary list every track's groups map onto, and the
/// track those segment records are published on.
///
/// One per broadcast, owned by [`catalog::Producer`](crate::catalog::Producer); `Clone` shares
/// it. Media tracks enroll with [`track`](Self::track) and report group opens through the
/// returned [`Recorder`]; an application with its own boundaries overrides the pacing with
/// [`cut`](Self::cut). See the [module docs](self) for the whole model.
#[derive(Clone)]
pub struct Producer {
	state: Arc<Mutex<State>>,
}

impl Producer {
	/// A timeline for `broadcast`, paced by `config`.
	///
	/// The MoQ track itself is created when the first media track enrolls, so a broadcast that
	/// never segments never publishes (or advertises) one.
	pub fn new(broadcast: &moq_net::broadcast::Producer, config: Config) -> Self {
		Self {
			state: Arc::new(Mutex::new(State {
				config,
				broadcast: broadcast.clone(),
				sink: None,
				expiry: VecDeque::new(),
				start: None,
				cuts: VecDeque::new(),
				manual: false,
				next_segment: 0,
				tracks: BTreeMap::new(),
				reservers: 0,
				overrun: None,
				timescale: Timescale::MILLI,
			})),
		}
	}

	/// Enroll `track`, returning the [`Recorder`] its group opens are reported through.
	///
	/// The segment records key ranges by the track's name, and the track paces boundaries and
	/// gates completeness until its recorder drops. Enroll a track when it is about to produce:
	/// an enrolled but silent track holds every record back, by design, since a segment isn't
	/// complete until every track's content is known. One recorder per track: enrolling the same
	/// name again resets its state.
	///
	/// Takes the whole track rather than its name so the timeline reads its
	/// [`latency_max`](moq_net::track::Info::latency_max) from the same place, which is how long
	/// a record about it stays true. Passing a name could name a track whose window is something
	/// else entirely, and the timeline would advertise segments nobody can fetch.
	///
	/// Creates the timeline track on first use, which errors if the broadcast can't (something
	/// else already took the name).
	pub fn track(&self, track: &moq_net::track::Producer) -> crate::Result<Recorder> {
		let mut state = self.state.lock().unwrap();

		if state.sink.is_none() && state.overrun.is_none() {
			let net = state.broadcast.create_track(DEFAULT_NAME, None)?;
			let config = moq_json::window::ProducerConfig::default().with_compression(true);
			state.sink = Some(moq_json::window::Producer::new(net, config));
		}

		let name = track.name().to_string();
		state.tracks.insert(
			name.clone(),
			TrackState {
				pending: VecDeque::new(),
				frontier: None,
				closed: false,
				latency_max: track.info().latency_max,
			},
		);

		Ok(Recorder {
			state: self.state.clone(),
			name,
		})
	}

	/// Declare a segment boundary at `pts`, overriding the [`Config::duration_min`] pacing.
	///
	/// For applications that know their own boundaries (an HLS import following the source
	/// playlist, CMAF segments on disk, an encoder placing keyframes). Cutting ahead of the
	/// media is fine: the segment's record still waits for every track's groups. A cut that
	/// would make a segment shorter than [`Config::duration_min`] is ignored, so several
	/// producers declaring the same boundaries (the renditions of one import) cost nothing.
	///
	/// The first call takes over for good: [`Config::duration_min`] pacing stops, since it would
	/// otherwise close a segment just before the caller declares where it really ends. Segments
	/// then last exactly as long as the caller says, and the final one runs to the end of the
	/// media.
	///
	/// Errors if a segment already overran [`Config::duration_max`].
	pub fn cut(&self, pts: Timestamp) -> crate::Result<()> {
		let mut state = self.state.lock().unwrap();
		if let Some(err) = state.failure() {
			return Err(err);
		}

		// Even a cut this rejects says the caller owns the boundaries.
		state.manual = true;

		let floor = state
			.cuts
			.back()
			.copied()
			.or(state.start)
			.map(|since| since.as_micros() + state.config.duration_min.as_micros());
		if floor.is_none_or(|floor| pts.as_micros() >= floor) {
			state.cuts.push_back(pts);
			state.pump(false);
		}

		Ok(())
	}

	/// Begin reserving the track set, returning a clonable [`Reserved`].
	///
	/// The counterpart to [`catalog::Producer::reserve`](crate::catalog::Producer::reserve), for
	/// the same reason: while any `Reserved` clone is alive the track set may still grow, so
	/// records are withheld from the broadcast. A record is immutable once published and its
	/// completeness is judged against the tracks enrolled *at that moment*, so a segment that
	/// flushes while a sibling rendition is still enrolling omits it for good, and that
	/// rendition's earlier groups then land in whatever segment flushes next.
	///
	/// Hand it (or clones) to whatever brings the tracks up, so an importer that enrolls its
	/// renditions one at a time publishes nothing until they are all in. Unlike the catalog's,
	/// this gate is not one-shot: the catalog is a snapshot, so only its *first* publish needs
	/// protecting, while every timeline record is an immutable log entry. Take a fresh
	/// reservation around every batch.
	pub fn reserve(&self) -> Reserved {
		self.state.lock().unwrap().reservers += 1;
		Reserved {
			state: self.state.clone(),
		}
	}

	/// The catalog's root section advertising this timeline.
	pub fn section(&self) -> Timeline {
		self.state.lock().unwrap().section()
	}

	/// Flush the final (still open) segment and finish the track.
	///
	/// Errors if a segment overran [`Config::duration_max`], or if the track can't be finished.
	pub fn finish(&mut self) -> crate::Result<()> {
		let mut state = self.state.lock().unwrap();
		state.finish();
		if let Some(err) = state.failure() {
			return Err(err);
		}

		// Finish the sink in place (dropping it would retire the track before late readers
		// catch up); a post-finish flush then fails in emit() and is logged there.
		let Some(sink) = state.sink.as_mut() else {
			return Ok(());
		};
		match sink.finish() {
			Ok(()) => Ok(()),
			Err(moq_json::Error::Net(err)) => Err(err.into()),
			Err(err) => unreachable!("timeline finish failed to encode: {err}"),
		}
	}
}

/// A clonable reservation withholding [`Producer`] records while tracks are still enrolling.
///
/// Made via [`Producer::reserve`], mirroring [`catalog::Reserved`](crate::catalog::Reserved).
/// Whatever became complete meanwhile flushes once the last clone drops.
pub struct Reserved {
	state: Arc<Mutex<State>>,
}

impl Clone for Reserved {
	fn clone(&self) -> Self {
		self.state.lock().unwrap().reservers += 1;
		Self {
			state: self.state.clone(),
		}
	}
}

impl Drop for Reserved {
	fn drop(&mut self) {
		let mut state = self.state.lock().unwrap();
		state.reservers = state.reservers.saturating_sub(1);
		state.pump(false);
	}
}

/// Reports one media track's group opens into the shared [`Producer`].
///
/// Move-only: it is the track's single reporting handle, and dropping it closes the track's
/// enrollment (segments stop waiting on it). Minted by [`Producer::track`] and held by a
/// rendition's [`container::Producer`](crate::container::Producer).
pub struct Recorder {
	state: Arc<Mutex<State>>,
	name: String,
}

impl Recorder {
	/// Report that group `sequence` opened at presentation time `pts`, `keyframe` stating
	/// whether its first frame is one (i.e. whether a player could join here).
	///
	/// Reports must be in group order with monotonic timestamps; this is the fact the timeline
	/// builds ranges, boundaries and completeness from.
	pub(crate) fn record(&mut self, sequence: u64, pts: Timestamp, keyframe: bool) {
		self.state.lock().unwrap().report(&self.name, sequence, pts, keyframe);
	}

	/// Report that this track's content extends to `pts`, without a group opening there.
	///
	/// A group open says where content *starts*; the last group of a broadcast has no successor
	/// to bound it, so its segment would otherwise be published a group short (zero for a
	/// segment that is a single group). Report the end whenever you know it: closing a group,
	/// finishing a track.
	pub(crate) fn end(&mut self, pts: Timestamp) {
		self.state.lock().unwrap().report_end(&self.name, pts);
	}
}

impl Drop for Recorder {
	fn drop(&mut self) {
		self.state.lock().unwrap().close(&self.name);
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

/// One change to the timeline: a segment became available, or older ones stopped being.
///
/// The timeline is a sliding window over what the publisher can still serve, not a log of
/// everything it ever published, so a reader has to be told about both ends. An exporter that
/// only handles [`Append`](Self::Append) builds a playlist that grows forever and advertises
/// segments whose media is long gone.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Update<E: RecordExt = ()> {
	/// A segment became available, in segment order.
	Append(Entry<E>),

	/// Every segment before `segment` has left the publisher's cache and can no longer be
	/// fetched. Retracts only segments this consumer was told about.
	Trim {
		/// The oldest segment still available.
		segment: u64,
	},
}

/// Reads a broadcast's timeline, yielding each [`Update`] in order.
///
/// The timeline slides: a late joiner sees an [`Update::Append`] per segment currently available
/// and then follows along live, with [`Update::Trim`] as the old ones expire. Generic over the
/// record extension `E` (see [`RecordExt`]).
pub struct Consumer<E: RecordExt = ()> {
	inner: moq_json::window::Consumer<Record<E>>,
	timescale: Timescale,
	/// The `(position, segment)` of every record yielded and not yet retracted, oldest first.
	///
	/// The window counts in positions and the timeline speaks segment numbers. They advance
	/// together but need not coincide: a consumer joining a window that already trimmed starts
	/// at a nonzero position, and the publisher numbers segments from its own start.
	live: VecDeque<(u64, u64)>,
	/// One past the newest segment yielded: what a trim that empties the window reports, since
	/// there is no oldest record left to name.
	next_segment: u64,
}

impl<E: RecordExt> Consumer<E> {
	/// Subscribe to the timeline advertised by the catalog's [`Timeline`] section.
	///
	/// The section supplies both the track name and the timescale, so a reader can't pair the
	/// wrong scale with the track. Errors if the section declares a timescale that isn't
	/// representable.
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer, section: &Timeline) -> crate::Result<Self> {
		let track = broadcast.track(&section.track)?.subscribe(None).await?;

		let config = moq_json::window::ConsumerConfig::default().with_compression(true);

		Ok(Self {
			inner: moq_json::window::Consumer::new(track, config),
			timescale: Timescale::new(section.timescale as u64)
				.map_err(|_| crate::Error::InvalidTimescale(section.timescale))?,
			live: VecDeque::new(),
			next_segment: 0,
		})
	}

	/// Translate a window update into a timeline one, converting timing out of the wire timescale.
	///
	/// A pts the timescale can't represent is an error rather than a substituted value: silently
	/// moving a timestamp would misdirect seeking and live-edge logic.
	fn decode(&mut self, update: moq_json::window::Update<Record<E>>) -> crate::Result<Update<E>> {
		match update {
			moq_json::window::Update::Append { position, value } => {
				let scale = self.timescale.as_u64() as u128;
				let nanos = (value.duration as u128) * 1_000_000_000 / scale;
				let entry = Entry {
					segment: value.segment,
					pts: Timestamp::new(value.pts, self.timescale)?,
					duration: Duration::from_nanos(nanos as u64),
					tracks: value.tracks,
					ext: value.ext,
				};
				self.live.push_back((position, entry.segment));
				self.next_segment = entry.segment.saturating_add(1);
				Ok(Update::Append(entry))
			}
			moq_json::window::Update::Trim { offset } => {
				while self.live.front().is_some_and(|&(position, _)| position < offset) {
					self.live.pop_front();
				}
				// The window retracts positions and callers think in segments. With the window
				// emptied there is no oldest record to name, so the next segment the publisher
				// appends is the bound: it is the next one this consumer will be told about.
				let segment = self.live.front().map_or(self.next_segment, |&(_, segment)| segment);
				Ok(Update::Trim { segment })
			}
			// The window's update kinds are `#[non_exhaustive]`. One this build doesn't know says
			// something about availability that it can't translate into a segment fact, and
			// guessing would leave the reader's view of the timeline quietly wrong.
			_ => Err(moq_json::Error::Window("unsupported window update".to_string()).into()),
		}
	}

	/// Get the next update, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Update<E>>> {
		match self.inner.next().await? {
			Some(update) => Ok(Some(self.decode(update)?)),
			None => Ok(None),
		}
	}

	/// Poll for the next update, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Update<E>>>> {
		match self.inner.poll_next(waiter)? {
			Poll::Ready(Some(update)) => Poll::Ready(self.decode(update).map(Some)),
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

	/// Drain every update a fresh subscriber sees on the producer's advertised section.
	async fn drain_updates(broadcast: &moq_net::broadcast::Producer, producer: &Producer) -> Vec<Update> {
		let mut consumer = Consumer::subscribe(&broadcast.consume(), &producer.section())
			.await
			.unwrap();
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			out.push(update);
		}
		out
	}

	/// The segments a fresh subscriber can see, which for a window is what is still fetchable
	/// rather than everything ever published.
	async fn drain(broadcast: &moq_net::broadcast::Producer, producer: &Producer) -> Vec<Entry> {
		drain_updates(broadcast, producer)
			.await
			.into_iter()
			.filter_map(|update| match update {
				Update::Append(entry) => Some(entry),
				_ => None,
			})
			.collect()
	}

	fn setup() -> (moq_net::broadcast::Producer, Producer) {
		setup_with(Config::default())
	}

	/// Mint a media track on the broadcast and enroll it, at moq-net's default retention.
	fn enroll(broadcast: &moq_net::broadcast::Producer, timeline: &Producer, name: &str) -> Recorder {
		enroll_with(broadcast, timeline, name, None)
	}

	/// Same, with explicit track info: the timeline reads its retention from the track, so this is
	/// how a test gives one track a shorter (or longer) window than another.
	fn enroll_with(
		broadcast: &moq_net::broadcast::Producer,
		timeline: &Producer,
		name: &str,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> Recorder {
		let track = broadcast.clone().create_track(name, info).unwrap();
		timeline.track(&track).unwrap()
	}

	fn setup_with(config: Config) -> (moq_net::broadcast::Producer, Producer) {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let timeline = Producer::new(&broadcast, config);
		(broadcast, timeline)
	}

	// The coarsest track paces: video GOPs are longer than duration_min, so segments are GOPs.
	#[tokio::test]
	async fn the_coarsest_track_paces_the_segments() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		// Video keyframes every 2s, audio groups every 500ms, minimum 1s.
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

		// Audio's own candidate (1s) loses to video's (2s), so no segment splits a GOP. Every
		// record is self-contained: start AND end groups, explicit duration.
		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 3)])]),
				entry(1, 2_000, 2_000, &[("video0", &[(1, 1)]), ("audio0", &[(4, 7)])]),
				entry(2, 4_000, 0, &[("video0", &[(2, 2)]), ("audio0", &[(8, 8)])]),
			]
		);
	}

	// A real-time encoder's GOP can dwarf the minimum. Nothing is violated: there is nowhere
	// else a segment could start and stay decodable, so the segment is simply long.
	#[tokio::test]
	async fn a_gop_longer_than_the_minimum_is_one_segment() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		video.record(0, ms(0), true);
		video.record(1, ms(30_000), true);
		video.end(ms(60_000));
		drop(video);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 30_000, &[("video0", &[(0, 0)])]),
				entry(1, 30_000, 30_000, &[("video0", &[(1, 1)])]),
			]
		);
	}

	// Groups shorter than the minimum pack into one segment rather than each becoming one.
	#[tokio::test]
	async fn short_groups_pack_up_to_the_minimum() {
		let (broadcast, mut timeline) = setup_with(Config {
			duration_min: Duration::from_millis(1_500),
			..Default::default()
		});
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		for seq in 0..8u64 {
			audio.record(seq, ms(seq * 500), true);
		}
		drop(audio);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		// The first group at or past 1500ms ends segment 0, so it holds groups 0..=2.
		assert_eq!(entries[0], entry(0, 0, 1_500, &[("audio0", &[(0, 2)])]));
		assert_eq!(entries[1], entry(1, 1_500, 1_500, &[("audio0", &[(3, 5)])]));
	}

	// A track that hasn't reached the minimum yet holds the segment open, so a record never
	// omits a rendition that was about to contribute to it.
	#[tokio::test]
	async fn a_segment_waits_for_every_track() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		// Video alone would close segment 0 here, but audio hasn't crossed 2s.
		assert!(drain(&broadcast, &timeline).await.is_empty());

		audio.record(1, ms(2_000), true);
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries,
			vec![entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 0)])])]
		);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();
	}

	// An application that knows its own boundaries overrides the pacing.
	#[tokio::test]
	async fn explicit_cuts_override_the_pacing() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		// Keyframes every second, cut every three: the segments follow the cuts, not the GOPs.
		timeline.cut(ms(3_000)).unwrap();
		timeline.cut(ms(6_000)).unwrap();
		for seq in 0..7u64 {
			video.record(seq, ms(seq * 1_000), true);
		}
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries[0], entry(0, 0, 3_000, &[("video0", &[(0, 2)])]));
		assert_eq!(entries[1], entry(1, 3_000, 3_000, &[("video0", &[(3, 5)])]));
	}

	// Every rendition of one import declares the same boundaries, so a cut that would make a
	// segment shorter than the minimum is dropped rather than producing a stray segment.
	#[tokio::test]
	async fn a_cut_below_the_minimum_is_ignored() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		video.record(0, ms(0), true);
		timeline.cut(ms(2_000)).unwrap();
		// A sibling rendition's duplicate, and a boundary too close to be a segment.
		timeline.cut(ms(2_000)).unwrap();
		timeline.cut(ms(2_500)).unwrap();
		timeline.cut(ms(4_000)).unwrap();
		video.record(1, ms(2_000), true);
		video.record(2, ms(4_000), true);
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries.len(), 3, "the duplicate and the 500ms cut are both dropped");
		assert_eq!(entries[0], entry(0, 0, 2_000, &[("video0", &[(0, 0)])]));
		assert_eq!(entries[1], entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]));
	}

	// The declared maximum is a contract with consumers (an HLS EXT-X-TARGETDURATION), so a
	// segment that breaks it fails the timeline rather than publishing a record that
	// contradicts the catalog.
	#[tokio::test]
	async fn exceeding_the_declared_maximum_fails_the_timeline() {
		let (broadcast, mut timeline) = setup_with(Config {
			duration_min: Duration::from_secs(1),
			duration_max: Some(Duration::from_secs(3)),
			..Default::default()
		});
		let mut video = enroll(&broadcast, &timeline, "video0");

		let mut consumer = {
			video.record(0, ms(0), true);
			video.record(1, ms(2_000), true);
			Consumer::<()>::subscribe(&broadcast.consume(), &timeline.section())
				.await
				.unwrap()
		};

		// A 4s GOP against a declared 3s maximum.
		video.record(2, ms(6_000), true);
		drop(video);

		let err = timeline.finish().unwrap_err();
		assert!(
			matches!(err, crate::Error::TimelineOverrun { segment: 1, .. }),
			"unexpected error: {err}"
		);

		// The record that was still true published; the one that would have contradicted the
		// catalog did not, and the track ended there.
		let waiter = kio::Waiter::noop();
		let mut entries = Vec::new();
		while let Poll::Ready(Ok(Some(Update::Append(appended)))) = consumer.poll_next(&waiter) {
			entries.push(appended);
		}
		assert_eq!(entries, vec![entry(0, 0, 2_000, &[("video0", &[(0, 0)])])]);
	}

	#[tokio::test]
	async fn an_undeclared_maximum_is_omitted_from_the_catalog() {
		let (_broadcast, timeline) = setup();
		assert_eq!(timeline.section().duration_max, None);

		let (_broadcast, timeline) = setup_with(Config {
			duration_max: Some(Duration::from_millis(2_500)),
			..Default::default()
		});
		assert_eq!(timeline.section().duration_max, Some(2_500));
	}

	// The last group of a broadcast has no successor to bound it, so without a reported end the
	// final segment's duration collapses to zero (an HLS EXTINF:0).
	#[tokio::test]
	async fn the_final_segment_runs_to_the_reported_end() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		video.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		// A finished `container::Producer` reports where its content stops.
		video.end(ms(4_000));
		drop(video);
		timeline.finish().unwrap();

		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("video0", &[(0, 0)])]),
				entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]),
			]
		);
	}

	// A record is immutable and judged against the tracks enrolled when it flushes, so a
	// producer that enrolls its tracks one batch at a time (an HLS import walking its
	// renditions) holds flushing back until they are all in.
	#[tokio::test]
	async fn a_reservation_defers_flushing_until_every_track_enrolls() {
		let (broadcast, mut timeline) = setup();
		let reserved = timeline.reserve();

		// The primary rendition runs a whole batch of segments through before its sibling has
		// even loaded an init segment.
		let mut first = enroll(&broadcast, &timeline, "video0");
		for (seq, t) in [(0u64, 0u64), (1, 2_000), (2, 4_000)] {
			first.record(seq, ms(t), true);
		}

		let mut second = enroll(&broadcast, &timeline, "video1");
		for (seq, t) in [(0u64, 0u64), (1, 2_000), (2, 4_000)] {
			second.record(seq, ms(t), true);
		}

		drop(reserved);
		drop(first);
		drop(second);
		timeline.finish().unwrap();

		// Both renditions are indexed from segment 0. Without the reservation, segments 0 and 1 would
		// have flushed knowing only video0 (a permanent gap for video1), and video1's first two
		// groups would have folded into segment 2.
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries[0],
			entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("video1", &[(0, 0)])])
		);
		assert_eq!(
			entries[1],
			entry(1, 2_000, 2_000, &[("video0", &[(1, 1)]), ("video1", &[(1, 1)])])
		);
	}

	// A track that races ahead of the others still lands in the segment its content falls in.
	#[tokio::test]
	async fn groups_before_the_first_boundary_join_the_first_segment() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		// Audio races ahead of video's first keyframe (the startup race): its early group
		// belongs to segment 0, which starts where the earliest content does.
		audio.record(0, ms(0), true);
		video.record(0, ms(30), true);
		for (seq, t) in [(1u64, 500u64), (2, 1_000), (3, 1_500), (4, 2_000)] {
			audio.record(seq, ms(t), true);
		}
		video.record(1, ms(2_030), true);
		audio.record(5, ms(2_500), true);
		drop(video);
		drop(audio);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries[0],
			entry(0, 0, 2_030, &[("video0", &[(0, 0)]), ("audio0", &[(0, 4)])]),
		);
	}

	#[tokio::test]
	async fn sequence_gaps_split_ranges() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

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

	// A track with nothing to contribute is a whole-segment gap: the record simply omits it
	// (an HLS exporter renders EXT-X-GAP). Only a *closed* track produces one, since a live
	// track that has merely gone quiet still gets to say where the boundary is.
	#[tokio::test]
	async fn a_closed_track_leaves_a_whole_segment_gap() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		drop(audio);
		video.record(1, ms(2_000), true);
		video.record(2, ms(4_000), true);
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries[1], entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]));
	}

	#[tokio::test]
	async fn a_non_keyframe_range_start_is_flagged() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		video.record(0, ms(0), true);
		// A mid-stream join: the group doesn't open on an IDR.
		video.record(1, ms(2_000), false);
		video.record(2, ms(4_000), true);
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		let range = entries[1].tracks["video0"][0];
		assert!(!range.keyframe, "a range whose first group isn't an IDR says so");
	}

	// A dead track would otherwise hold every later segment: dropping its recorder is what
	// says it will never report again.
	#[tokio::test]
	async fn a_closed_track_stops_gating() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let mut audio = enroll(&broadcast, &timeline, "audio0");

		video.record(0, ms(0), true);
		audio.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		assert!(drain(&broadcast, &timeline).await.is_empty(), "audio still gates");

		drop(audio);
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries,
			vec![entry(0, 0, 2_000, &[("video0", &[(0, 0)]), ("audio0", &[(0, 0)])])]
		);
		drop(video);
		timeline.finish().unwrap();
	}

	/// Track info with an explicit retention window.
	fn retention(latency_max: Duration) -> moq_net::track::Info {
		moq_net::track::Info::default().with_latency_max(latency_max)
	}

	/// The timeline lists what the publisher can still serve, so a record goes when the groups
	/// behind it age out of the cache. A late joiner therefore gets the live window, not the
	/// broadcast's whole history.
	#[tokio::test]
	async fn a_record_is_retracted_once_its_groups_expire() {
		tokio::time::pause();

		let (broadcast, mut timeline) = setup();
		let mut video = enroll_with(&broadcast, &timeline, "video0", retention(Duration::from_secs(10)));

		// Three segments, each a 2s group, published a group apart in wall-clock time too.
		for seq in 0..4u64 {
			video.record(seq, ms(seq * 2_000), true);
			tokio::time::advance(Duration::from_secs(2)).await;
		}

		// Nothing has aged out yet at t=8s, so a fresh subscriber sees every segment.
		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(entries.len(), 3, "3 complete segments, the 4th group is still open");

		// Past the first group's window. The next report is what sweeps, matching moq-net, which
		// only evicts as groups are committed.
		tokio::time::advance(Duration::from_secs(5)).await;
		video.record(4, ms(8_000), true);

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries.iter().map(|e| e.segment).collect::<Vec<_>>(),
			vec![2, 3],
			"segments whose groups left the cache are no longer advertised"
		);

		drop(video);
		timeline.finish().unwrap();
	}

	/// A consumer that was told about a segment is told when it goes, naming the oldest one still
	/// available so an exporter can bound its playlist without diffing.
	#[tokio::test]
	async fn a_live_consumer_sees_the_retraction() {
		tokio::time::pause();

		let (broadcast, mut timeline) = setup();
		let mut video = enroll_with(&broadcast, &timeline, "video0", retention(Duration::from_secs(10)));

		let mut consumer = Consumer::<()>::subscribe(&broadcast.consume(), &timeline.section())
			.await
			.unwrap();
		let waiter = kio::Waiter::noop();

		for seq in 0..3u64 {
			video.record(seq, ms(seq * 2_000), true);
			tokio::time::advance(Duration::from_secs(2)).await;
		}

		let mut updates = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			updates.push(update);
		}
		assert_eq!(updates.len(), 2, "two complete segments appended, no retraction yet");

		// Land past the first segment's deadline but inside the second's, then report to drive
		// the sweep.
		tokio::time::advance(Duration::from_secs(5)).await;
		video.record(3, ms(6_000), true);

		let mut updates = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			updates.push(update);
		}
		assert!(
			updates.contains(&Update::Trim { segment: 1 }),
			"expected segment 0 to be retracted, got {updates:?}"
		);

		drop(video);
		timeline.finish().unwrap();
	}

	/// A segment spans every track, so it is only fetchable while all of them still hold their
	/// groups: the shortest window bounds the record, without the timeline needing a rule for it.
	#[tokio::test]
	async fn the_shortest_retention_bounds_the_record() {
		tokio::time::pause();

		let (broadcast, mut timeline) = setup();
		let mut video = enroll_with(&broadcast, &timeline, "video0", retention(Duration::from_secs(60)));
		let mut audio = enroll_with(&broadcast, &timeline, "audio0", retention(Duration::from_secs(5)));

		for seq in 0..3u64 {
			video.record(seq, ms(seq * 2_000), true);
			audio.record(seq, ms(seq * 2_000), true);
			tokio::time::advance(Duration::from_secs(2)).await;
		}
		assert_eq!(drain(&broadcast, &timeline).await.len(), 2);

		// Past audio's 5s window but far inside video's 60s one.
		tokio::time::advance(Duration::from_secs(4)).await;
		video.record(3, ms(6_000), true);
		audio.record(3, ms(6_000), true);

		assert_eq!(
			drain(&broadcast, &timeline).await.first().map(|e| e.segment),
			Some(2),
			"the segment went with audio's groups even though video still holds its own"
		);

		drop(video);
		drop(audio);
		timeline.finish().unwrap();
	}

	/// The retraction has to lead the eviction it predicts, or a consumer fetches on a record
	/// whose groups are already gone by the time the request lands. moq-net keeps a group for a
	/// grace period past `latency_max`, and the timeline trims at `latency_max` exactly.
	#[tokio::test]
	async fn the_retraction_leads_the_eviction() {
		tokio::time::pause();

		let latency_max = Duration::from_secs(4);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let timeline = Producer::new(&broadcast, Config::default());

		// A real media track, so the groups the record names are really cached and really evicted.
		let mut track = broadcast.create_track("video0", retention(latency_max)).unwrap();
		let mut video = timeline.track(&track).unwrap();

		// t=0. Segment 0 ends up being exactly this group.
		track.create_group(0u64.into()).unwrap().finish().unwrap();
		video.record(0, ms(0), true);

		// t=2: the next group closes segment 0 and publishes its record.
		tokio::time::advance(Duration::from_secs(2)).await;
		track.create_group(1u64.into()).unwrap().finish().unwrap();
		video.record(1, ms(2_000), true);
		assert!(
			drain(&broadcast, &timeline).await.iter().any(|e| e.segment == 0),
			"segment 0 should be published while its group is fresh"
		);

		// t=4.5: group 0 is past `latency_max`, so its record goes, but it is still inside the
		// grace moq-net adds on top. Half a second is well clear of the cache's 100ms tick, so
		// this lands strictly between the two deadlines rather than on the boundary.
		tokio::time::advance(Duration::from_millis(2_500)).await;
		track.create_group(2u64.into()).unwrap().finish().unwrap();
		video.record(2, ms(4_000), true);

		let entries = drain(&broadcast, &timeline).await;
		assert!(
			!entries.iter().any(|e| e.segment == 0),
			"the record should be retracted at latency_max"
		);
		assert!(
			broadcast
				.consume()
				.track("video0")
				.unwrap()
				.fetch_group(0, None)
				.await
				.is_ok(),
			"the group must outlive its record, or a FETCH on it races eviction"
		);
	}

	// The timeline track (and its catalog section) exist only once a media track enrolls.
	#[tokio::test]
	async fn the_track_is_created_on_first_enrollment() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let timeline = Producer::new(&broadcast, Config::default());
		assert!(
			broadcast.create_track(DEFAULT_NAME, None).is_ok(),
			"nothing took the name yet"
		);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let timeline2 = Producer::new(&broadcast, Config::default());
		let _recorder = enroll(&broadcast, &timeline2, "video0");
		assert!(
			broadcast.create_track(DEFAULT_NAME, None).is_err(),
			"the timeline took the name once a track enrolled"
		);
		drop(timeline);
	}

	#[tokio::test]
	async fn section_advertises_track_and_wall() {
		let (_broadcast, timeline) = setup();
		let section = timeline.section();
		assert_eq!(section.track, DEFAULT_NAME);
		assert_eq!(section.timescale, 1000);
		assert_eq!(section.wall, None);

		// The wire counts from the moq epoch, so pts 0 at exactly the epoch advertises 0.
		let (_broadcast, timeline) = setup_with(Config {
			wall: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS)),
			..Default::default()
		});
		assert_eq!(timeline.section().wall, Some(0));

		// A second later is a second's worth of timescale units.
		let (_broadcast, timeline) = setup_with(Config {
			wall: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS + 1_000)),
			..Default::default()
		});
		assert_eq!(timeline.section().wall, Some(1_000));
	}

	// Clones nest like the catalog's, so the first batch to finish doesn't publish records the
	// others are still filling in.
	#[tokio::test]
	async fn reservations_nest() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		let outer = timeline.reserve();
		let inner = outer.clone();

		for (seq, t) in [(0u64, 0u64), (1, 2_000), (2, 4_000)] {
			video.record(seq, ms(t), true);
		}

		drop(inner);
		assert!(
			drain(&broadcast, &timeline).await.is_empty(),
			"the other clone still gates"
		);

		drop(outer);
		assert_eq!(
			drain(&broadcast, &timeline).await.len(),
			2,
			"the last clone dropped flushes"
		);

		drop(video);
		timeline.finish().unwrap();
	}

	// A reservation outstanding when the broadcast ends must not strand the terminal flush.
	#[tokio::test]
	async fn finish_overrides_an_outstanding_reservation() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");
		let reserved = timeline.reserve();

		video.record(0, ms(0), true);
		video.record(1, ms(2_000), true);
		video.end(ms(4_000));
		drop(video);

		timeline.finish().unwrap();
		assert_eq!(
			drain(&broadcast, &timeline).await,
			vec![
				entry(0, 0, 2_000, &[("video0", &[(0, 0)])]),
				entry(1, 2_000, 2_000, &[("video0", &[(1, 1)])]),
			]
		);

		// Dropping it afterwards is a no-op rather than an underflow.
		drop(reserved);
	}

	#[tokio::test]
	async fn rejects_an_invalid_timescale() {
		let (broadcast, timeline) = setup();
		let _recorder = enroll(&broadcast, &timeline, "video0");
		let mut section = timeline.section();
		section.timescale = 0;
		let err = Consumer::<()>::subscribe(&broadcast.consume(), &section).await;
		assert!(matches!(err, Err(crate::Error::InvalidTimescale(0))));
	}

	// A cut registered before the media, landing exactly on the first reported group (what the
	// fMP4 importer does: it cuts at the keyframe fragment's timestamp, then records that group).
	#[tokio::test]
	async fn a_cut_on_the_first_group_does_not_poison_later_cuts() {
		let (broadcast, mut timeline) = setup();
		let mut video = enroll(&broadcast, &timeline, "video0");

		// Source segments every 3s, keyframes every 1s: 3s segments, not the 1s the
		// duration_min pacing would produce on its own.
		timeline.cut(ms(0)).unwrap();
		video.record(0, ms(0), true);
		for seq in 1..10u64 {
			if seq % 3 == 0 {
				timeline.cut(ms(seq * 1_000)).unwrap();
			}
			video.record(seq, ms(seq * 1_000), true);
		}
		drop(video);
		timeline.finish().unwrap();

		let entries = drain(&broadcast, &timeline).await;
		assert_eq!(
			entries[0],
			entry(0, 0, 3_000, &[("video0", &[(0, 2)])]),
			"the source's 3s boundaries should be reproduced, not duration_min pacing"
		);
	}
}

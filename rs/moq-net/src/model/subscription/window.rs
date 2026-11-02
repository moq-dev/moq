//! A subscriber's own ranges, resolved to the group sequences it reads.
//!
//! The aggregate a publisher sees is the union of every subscriber's ranges, so it says
//! what arrives, not what one subscriber asked for. A [`Window`] is the per-subscriber
//! side: resolved at the hop serving the subscriber, it filters that subscriber's reads.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops,
	sync::Arc,
};

use super::{Anchor, Layered, Range, Runs};
use crate::{Error, Result, Timestamp};

/// One range resolved to groups: from `start` up to the exclusive `end`, `None` while it
/// stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) struct Resolved {
	pub start: u64,
	pub end: Option<u64>,
}

/// When a serving hop's groups start, for resolving time anchors.
///
/// Each query answers from ordered indexes, so a time end that walks them forward a step
/// at a time pays for each group or run it passes once, however long it stays unsettled.
pub(in crate::model) trait Holdings {
	/// Whether the track carries timestamps at all.
	fn timed(&self) -> bool;

	/// Whether the groups are a copy received over a wire that can't resolve relative or
	/// time anchors as the publisher would, so only what [`Range::is_wire`] covers reads.
	fn drops_anchors(&self) -> bool {
		false
	}

	/// Where `pts` falls among the groups with a known start.
	fn containing(&self, pts: Timestamp) -> Containing;

	/// The lowest group at or above `from` known to start, with where it starts.
	fn next_start(&self, from: u64) -> Option<(u64, Timestamp)>;

	/// The lowest held group at or above `from`, with what it says about its start.
	fn next_held(&self, from: u64) -> Option<(u64, Held)>;

	/// The exclusive end of the run of sequences from `sequence` that can no longer arrive
	/// (dropped, given up, or below where the feed starts), `None` while `sequence` may.
	fn gone(&self, sequence: u64) -> Option<u64>;

	/// Whether nothing more can arrive or start here, so a time end has nothing to wait
	/// on and is left uncapped.
	fn complete(&self) -> bool;

	/// The runs of sequences whose arrivals or drops changed since `seen`, which this then
	/// moves on to now, or `None` once anything else a parked walk reads may have changed
	/// (see [`Window::parked`]): the declared start, completion, or a group the live feed
	/// never delivered. `None` by default, which wakes every parked walk.
	fn changes(&self, seen: &mut Seen) -> Option<Vec<ops::Range<u64>>> {
		let _ = seen;
		None
	}
}

/// How far a [`Window`] has taken in its [`Holdings::changes`], in whatever marks the
/// holdings keep: the revisions of the indexes reporting changed runs, and what they report
/// no runs for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::model) struct Seen {
	pub runs: [u64; 2],
	pub global: Option<[u64; 3]>,
}

/// Where a presentation time falls among the groups a hop holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Containing {
	/// The newest group starting at or before it.
	Group(u64),
	/// Every group with a known start begins after it.
	Before,
	/// No group has a known start.
	Unknown,
}

/// What a held group says about where it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Held {
	/// Its frame 0 presented at this time.
	Started(Timestamp),
	/// It holds frame 0 and has not presented it yet.
	Pending,
	/// It never will: it ended empty, was aborted, or is held from past frame 0.
	Never,
}

/// A hop that holds no groups of its own, such as a reader following route copies that
/// resolve its ranges for it: a time end there filters nothing.
pub(in crate::model) struct Unheld;

impl Holdings for Unheld {
	fn timed(&self) -> bool {
		true
	}

	fn containing(&self, _: Timestamp) -> Containing {
		Containing::Unknown
	}

	fn next_start(&self, _: u64) -> Option<(u64, Timestamp)> {
		None
	}

	fn next_held(&self, _: u64) -> Option<(u64, Held)> {
		None
	}

	fn gone(&self, _: u64) -> Option<u64> {
		None
	}

	fn complete(&self) -> bool {
		true
	}
}

/// Whether `time` is strictly after `other`, as instants: exact across scales, unlike
/// [`Timestamp`]'s `Ord`, which breaks a tie between scales.
pub(in crate::model) fn later(time: Timestamp, other: Timestamp) -> bool {
	instant(time, other).is_gt()
}

/// How `time` orders against `other` as instants, exact across scales.
fn instant(time: Timestamp, other: Timestamp) -> std::cmp::Ordering {
	(u128::from(time.value()) * u128::from(other.scale().as_u64()))
		.cmp(&(u128::from(other.value()) * u128::from(time.scale().as_u64())))
}

/// A subscriber's [`Range`]s resolved to the group sequences it reads.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, so only replacing the ranges resolves again.
	ranges: Vec<Range>,
	spans: Vec<Span>,
	/// The unsettled time ends, grouped by walk.
	walks: Vec<Walk>,
	/// The walks waiting out a gap below the group their ends fall before, which move only
	/// once a sequence from the gap up to that group arrives or is given up, so a settle
	/// wakes only the walks its [`Holdings::changes`] reach. Ends left in different gaps of
	/// a sparse feed never merge, so walking each of them on every settle would cost the
	/// ranges. Keyed by where each [`Parked`] starts.
	parked: BTreeMap<u64, Parked>,
	/// How far the parked walks have taken in the holdings' changes.
	seen: Seen,
	/// The groups the spans cover, an unsettled time end through its cap: every settled span
	/// and parked walk in the fixed runs, shared with what reads them, and one run per walk
	/// not parked layered over them, so a walk that moves rebuilds only the layer.
	groups: Layered,
	/// The part of `groups` a miss may ask for, layered the same way: see [`Self::missable`].
	missable: Layered,
	/// The groups resolved for good: the spans with a settled end, which the fixed runs hold
	/// with the parked walks' runs.
	resolved: Runs,
	resolved_missable: Runs,
	/// The fixed runs no longer hold the settled spans and parked walks: rebuilt once the
	/// settle under way is done, which a settle or a walk parking or waking does.
	stale: bool,
	/// A relative anchor waits on a first group, so nothing is filtered yet.
	pending: bool,
	/// Nothing is left to resolve: see [`Self::is_settled`]. Kept so a read checks it once
	/// per group, not once per range.
	settled: bool,
	/// The floor the last [`Self::covered_by`] checked from, and where it stopped finding the
	/// groups accounted for.
	covered: Option<(u64, u64)>,
	/// The lowest start among the spans that name theirs, and whether every span does: see
	/// [`Self::named`] and [`Self::start`]. Kept so a probe reads them without a walk.
	named: Option<u64>,
	all_named: bool,
	/// The ranges' bounds on a datagram, indexed for [`Self::admits_datagram`].
	admission: Admission,
}

/// Parked walks whose waits overlap, so a change reaching any of them wakes them all: the
/// parked groups are disjoint, each found by one search.
#[derive(Debug)]
struct Parked {
	/// The exclusive end of what they wait on, from the key it is parked under.
	end: u64,
	walks: Vec<Walk>,
}

/// One non-empty range, resolved.
#[derive(Clone, Copy, Debug)]
struct Span {
	/// Which of the ranges it came from.
	index: usize,
	start: u64,
	end: End,
	/// Whether the start names a group, rather than reaching back to the oldest one.
	floored: bool,
}

#[derive(Clone, Copy, Debug)]
enum End {
	/// Before this group.
	Before(u64),
	/// Past every group.
	Open,
	/// Before the first group starting after this time, once a [`Walk`] settles it.
	Time(Timestamp),
}

/// Unsettled time ends whose walks agree, so one walk serves them all.
///
/// Ends that start alike walk the same groups until one starts between their times, so
/// the ends sharing a walk differ only in when they end. A group starting at time `t`
/// splits off the ends before `t`, which walk on, and settle, as a walk of their own.
/// Walks that come to agree merge. Each end settles once, and a settle costs the walks
/// rather than the ends.
///
/// Timestamps never rewind, so an end falls before the lowest group starting past it, once
/// every sequence between it and the newest group starting at or before it is accounted
/// for. Every position below only moves forward, so each group or run of sequences is
/// passed once, whatever a peer leaves missing.
#[derive(Debug)]
struct Walk {
	/// The ends sharing it, earliest first.
	ends: BTreeSet<Ending>,
	trail: Trail,
	/// The lowest group any of its spans reads, and the lowest a miss asks for: each span
	/// reads on from its start up to the cap, so the walk's groups are one run from here.
	low: u64,
	low_reach: u64,
	/// The cap when the settle under way began, inherited by the walks split off since, so
	/// each knows whether the groups it reads moved.
	was: Option<u64>,
}

/// An unsettled time end: the span it ends, ordered by when, as instants.
#[derive(Clone, Copy, Debug)]
struct Ending {
	pts: Timestamp,
	span: usize,
}

impl Ord for Ending {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		instant(self.pts, other.pts).then(self.span.cmp(&other.span))
	}
}

impl PartialOrd for Ending {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl PartialEq for Ending {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other).is_eq()
	}
}

impl Eq for Ending {}

/// How far a [`Walk`] got: the same for every end sharing it, so walks merge once theirs
/// are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Trail {
	/// The lowest group the walk has yet to put in range for good. Each of its spans starts
	/// here, or at or below `known`, past which [`Walk::fix`] moves it.
	floor: u64,
	/// The lowest group a miss asks for in what the walk reads: a time start before every
	/// group held asks none below the oldest, since which of them it reaches is unknown
	/// until they start.
	reach: u64,
	/// The newest group known to start at or before every end: it and everything below it
	/// in the spans is in range.
	known: Option<u64>,
	/// The lowest group known to start after every end: where they fall once settled.
	after: Option<u64>,
	/// Where the walk over held groups resumes: those below it started, or never will.
	held: u64,
	/// Where settling resumes: every sequence from above `known` up to it is accounted for.
	accounted: u64,
	/// The exclusive cap on what is read meanwhile, `None` for none.
	limit: Option<u64>,
	/// The group the cap admits on speculation: the first above `known` that may still
	/// start, which may start past the ends.
	speculative: Option<u64>,
}

impl Trail {
	fn new(start: u64, reach: u64) -> Self {
		Self {
			floor: start,
			reach,
			known: None,
			after: None,
			held: start,
			accounted: start,
			limit: Some(start),
			speculative: None,
		}
	}

	/// The lowest sequence not known to be in range, `None` once the last sequence is.
	///
	/// `known` is only ever found at or above every span's start, so it alone decides once
	/// set.
	fn unknown(&self) -> Option<u64> {
		match self.known {
			Some(known) => known.checked_add(1),
			None => Some(self.floor),
		}
	}
}

impl Walk {
	/// Whether every end is before `time`, so a group starting then lies past each.
	///
	/// When only some are, those leave as a walk of their own from here, queued in `forks`
	/// to walk on, and this one keeps the rest: `false`.
	fn past(&mut self, time: Timestamp, forks: &mut Vec<Walk>) -> bool {
		let (Some(first), Some(last)) = (self.ends.first(), self.ends.last()) else {
			return false;
		};
		if later(time, last.pts) {
			return true;
		}
		if !later(time, first.pts) {
			return false;
		}
		let rest = self.ends.split_off(&Ending { pts: time, span: 0 });
		forks.push(Walk {
			ends: std::mem::replace(&mut self.ends, rest),
			trail: self.trail,
			low: self.low,
			low_reach: self.low_reach,
			was: self.was,
		});
		false
	}

	/// Walk the held groups on from where the last call stopped, splitting off into `forks`
	/// the ends a group starts between. Returns where the ends fall once they settle.
	///
	/// A split-off walks again from the state it split at: each call resumes where the
	/// last stopped, so it takes the other branch there, passing nothing twice.
	fn advance(&mut self, holdings: &impl Holdings, forks: &mut Vec<Walk>) -> Option<u64> {
		// The starts index passes a group that may still start, so a stalled one never
		// keeps a later group known to start inside the ends from being read.
		while let Some(from) = self.trail.unknown()
			&& let Some((sequence, at)) = holdings.next_start(from)
			&& self.trail.after.is_none_or(|after| sequence < after)
		{
			match self.past(at, forks) {
				true => self.trail.after = Some(sequence),
				false => self.trail.known = Some(sequence),
			}
		}

		// The first sequence above `known` that may still start is the one read on
		// speculation: a held group yet to start, or a missing one that may arrive. Only a
		// group starting inside the ends is jumped to, since it puts all below it in range.
		self.trail.speculative = None;
		while let Some(from) = self.trail.unknown() {
			let at = self.trail.held.max(from);
			if self.trail.after.is_some_and(|after| at >= after) {
				break;
			}
			let next = holdings
				.next_held(at)
				.filter(|(sequence, _)| self.trail.after.is_none_or(|after| *sequence < after));
			// A later group starting inside some ends but not others splits them first: those
			// it starts past account for `at` on their own.
			let jump = match next {
				Some((sequence, Held::Started(time))) if sequence != at => {
					(!self.past(time, forks)).then_some(sequence)
				}
				_ => None,
			};
			match next {
				Some((sequence, held)) if sequence == at => match held {
					Held::Started(time) => match self.past(time, forks) {
						true => self.trail.after = Some(at),
						false => self.trail.known = Some(at),
					},
					Held::Never => {}
					Held::Pending => {
						self.trail.held = at;
						self.trail.speculative = Some(at);
						break;
					}
				},
				Some(_) if jump.is_some() => self.trail.known = jump,
				// Past every held group: the next to arrive is the speculative one, judged
				// once a settle sees it, before it is read.
				None if self.trail.after.is_none() => {
					self.trail.held = at;
					break;
				}
				_ => match holdings.gone(at) {
					Some(end) if end > at => {
						self.trail.held = end;
						continue;
					}
					_ => {
						self.trail.held = at;
						self.trail.speculative = (!holdings.complete()).then_some(at);
						break;
					}
				},
			}
			// Nothing sits above the last sequence, so the walk ends on it.
			let Some(next) = at.checked_add(1) else { break };
			self.trail.held = next;
		}
		self.trail.limit = match (self.trail.speculative, self.trail.after) {
			_ if holdings.complete() => None,
			(Some(speculative), _) => speculative.checked_add(1),
			(None, Some(after)) => Some(after),
			// Every held group is accounted for: the next to arrive is the speculative one.
			(None, None) => None,
		};

		// Settle once every sequence up to the first group starting past the ends can no
		// longer start inside them.
		//
		// Datagrams stay ephemeral and never index where their group starts, so a sequence
		// that arrived only as a datagram counts as gone here. A stream copy of that group
		// arriving later is read even if it starts past the ends: accepted, since a
		// publisher that streams a group it also sent as a datagram is rare.
		let after = self.trail.after?;
		// Everything at or below `known` is in range, and `after` lies above it.
		self.trail.accounted = self.trail.accounted.max(self.trail.unknown().unwrap_or(after));
		while self.trail.accounted < after {
			let next = holdings.next_held(self.trail.accounted);
			match next {
				Some((sequence, held)) if sequence == self.trail.accounted => match held {
					Held::Pending => return None,
					// It started past the ends since the walk above stopped on it: everything
					// below is accounted for, so they fall here.
					Held::Started(at) if self.past(at, forks) => {
						self.trail.after = Some(sequence);
						return self.trail.after;
					}
					_ => self.trail.accounted += 1,
				},
				_ => {
					let below = next.map_or(after, |(sequence, _)| sequence.min(after));
					self.trail.accounted = match holdings.complete() {
						true => below,
						false => holdings.gone(self.trail.accounted)?.min(below),
					};
				}
			}
		}
		self.trail.after
	}

	/// Walk on only from above `known`, what the walk has put in range for good staying in
	/// its run from [`Self::low`]. Walks that started apart then agree once they get here.
	fn fix(&mut self) {
		let Some(known) = self.trail.known else { return };
		if self.trail.floor > known {
			return;
		}
		self.trail.floor = known.saturating_add(1);
		self.trail.reach = self.trail.reach.max(self.trail.floor);
	}

	/// The walk's groups now, and the part a miss may ask for: one run each, from where its
	/// spans start up to its cap. Everything from `low` up to `known` is in range for good,
	/// and the cap never falls below it.
	fn runs(&self) -> (Option<ops::RangeInclusive<u64>>, Option<ops::RangeInclusive<u64>>) {
		let last = match self.trail.limit {
			Some(end) => end.checked_sub(1).filter(|last| *last >= self.low),
			None => Some(u64::MAX),
		};
		let Some(last) = last else {
			return (None, None);
		};
		let missable = (self.low_reach <= last).then_some(self.low_reach..=last);
		(Some(self.low..=last), missable)
	}

	/// What this walk waits on, if it can park: settling stopped at a gap that may still
	/// arrive, below the group its ends fall before, holding from above `known` up to that
	/// group only groups below the gap that never start.
	///
	/// Until a sequence there arrives or is given up, the walk reads the same answers. One
	/// arriving late, even into a run given up, is in the holdings' changes, so the walk
	/// wakes then. Costs the groups held there, which never start.
	fn waits(&self, holdings: &impl Holdings) -> Option<ops::Range<u64>> {
		let (gap, after, from) = (self.trail.accounted, self.trail.after?, self.trail.unknown()?);
		if gap >= after || holdings.complete() || holdings.gone(gap).is_some() {
			return None;
		}
		let mut at = from;
		while let Some((sequence, held)) = holdings.next_held(at)
			&& sequence < after
		{
			if sequence >= gap || held != Held::Never {
				return None;
			}
			at = sequence + 1;
		}
		Some(from..after)
	}

	/// Take on `other`'s ends, its trail being this one's. Both runs end where the trails
	/// put them, so the merged runs start at the lower of each.
	fn merge(&mut self, mut other: Walk) {
		self.low = self.low.min(other.low);
		self.low_reach = self.low_reach.min(other.low_reach);
		if other.ends.len() > self.ends.len() {
			std::mem::swap(&mut self.ends, &mut other.ends);
		}
		self.ends.extend(other.ends);
	}
}

impl Window {
	/// Resolve `ranges`, counting relative anchors back from `latest`.
	///
	/// Refuses a time anchor on an untimed track with [`Error::TimestampMismatch`], and an
	/// anchor a received copy's wire drops with [`Error::Unsupported`].
	pub fn new(ranges: &[Range], latest: Option<u64>, holdings: &impl Holdings) -> Result<Self> {
		let mut window = Self {
			ranges: ranges.to_vec(),
			spans: Vec::new(),
			walks: Vec::new(),
			parked: BTreeMap::new(),
			seen: Seen::default(),
			groups: Layered::default(),
			missable: Layered::default(),
			resolved: Runs::default(),
			resolved_missable: Runs::default(),
			stale: false,
			pending: false,
			settled: false,
			covered: None,
			named: None,
			all_named: true,
			admission: Admission::default(),
		};
		window.place(latest, holdings)?;
		Ok(window)
	}

	/// Whether nothing is left to resolve as groups arrive and start.
	pub fn is_settled(&self) -> bool {
		self.settled
	}

	/// Whether a relative anchor still waits on a first group.
	pub fn is_pending(&self) -> bool {
		self.pending
	}

	/// Resolve what the groups now held settle: relative anchors once `latest` exists, and
	/// time ends as groups start. Returns whether the groups in range moved.
	pub fn settle(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> bool {
		if self.pending {
			if latest.is_none() {
				return false;
			}
			// Checked when the ranges were first resolved, against the same track.
			let _ = self.place(latest, holdings);
			return !self.pending;
		}
		if self.walks.is_empty() && self.parked.is_empty() {
			return false;
		}
		let moved = self.walk(holdings);
		if moved || self.stale {
			self.rebuild();
		}
		moved
	}

	/// Walk each unsettled time end on, settling those that can be, and merge the walks
	/// that come to agree. Returns whether the groups in range moved.
	fn walk(&mut self, holdings: &impl Holdings) -> bool {
		let mut moved = false;
		let mut walks = std::mem::take(&mut self.walks);
		self.wake(holdings, &mut walks);
		for walk in &mut walks {
			walk.was = walk.trail.limit;
		}
		let mut kept = Vec::with_capacity(walks.len());
		while let Some(mut walk) = walks.pop() {
			match walk.advance(holdings, &mut walks) {
				Some(end) => {
					// What settles joins the fixed runs, which readers share: copied once per
					// settle under way, since a settle is once per end.
					(moved, self.stale) = (true, true);
					if walk.low < end {
						self.resolved.insert(walk.low..=end - 1);
					}
					if walk.low_reach < end {
						self.resolved_missable.insert(walk.low_reach..=end - 1);
					}
					for ending in walk.ends {
						self.spans[ending.span].end = End::Before(end);
					}
				}
				None => {
					moved |= walk.trail.limit != walk.was;
					walk.fix();
					match walk.waits(holdings) {
						Some(waits) => self.park(waits, walk),
						None => kept.push(walk),
					}
				}
			}
		}
		kept.sort_unstable_by_key(|walk| walk.trail);
		for walk in kept {
			match self.walks.last_mut() {
				Some(last) if last.trail == walk.trail => last.merge(walk),
				_ => self.walks.push(walk),
			}
		}
		moved
	}

	/// Move into `walks` the parked walks a change since the last settle reaches: all of
	/// them once the holdings can't say which.
	fn wake(&mut self, holdings: &impl Holdings, walks: &mut Vec<Walk>) {
		if self.parked.is_empty() {
			// Kept current, so the first walk to park wakes only on what changes after it.
			let _ = holdings.changes(&mut self.seen);
			return;
		}
		let woke = walks.len();
		match holdings.changes(&mut self.seen) {
			None => walks.extend(
				std::mem::take(&mut self.parked)
					.into_values()
					.flat_map(|parked| parked.walks),
			),
			Some(changed) => {
				for run in changed {
					for (_, parked) in self.overlapping(run) {
						walks.extend(parked.walks);
					}
				}
			}
		}
		// The fixed runs held those it woke.
		self.stale |= walks.len() > woke;
	}

	/// Park `walk`, waiting on `waits`, with every parked walk whose wait overlaps it.
	fn park(&mut self, waits: ops::Range<u64>, walk: Walk) {
		self.stale = true;
		let mut parked = Parked {
			end: waits.end,
			walks: vec![walk],
		};
		let mut start = waits.start;
		for overlapping in self.overlapping(waits.clone()) {
			start = start.min(overlapping.0);
			parked.end = parked.end.max(overlapping.1.end);
			parked.walks.extend(overlapping.1.walks);
		}
		self.parked.insert(start, parked);
	}

	/// Take out the parked walks whose waits overlap `run`.
	fn overlapping(&mut self, run: ops::Range<u64>) -> Vec<(u64, Parked)> {
		if run.is_empty() {
			return Vec::new();
		}
		// They are disjoint: the one holding the run's start, then each starting inside it.
		let below = self
			.parked
			.range(..=run.start)
			.next_back()
			.filter(|(_, parked)| parked.end > run.start)
			.map(|(start, _)| *start);
		let inside = self
			.parked
			.range(run.start.saturating_add(1)..run.end)
			.map(|(start, _)| *start);
		let starts: Vec<u64> = below.into_iter().chain(inside).collect();
		starts
			.into_iter()
			.filter_map(|start| self.parked.remove(&start).map(|parked| (start, parked)))
			.collect()
	}

	/// The groups an unsettled time end reads on speculation, which may start past it: a
	/// read parked on the cap wakes when one of them starts.
	///
	/// A parked walk waits on a group arriving, never one starting, and an arrival changes
	/// the track a read parks on anyway, so only the walks not parked count, here and in
	/// [`Self::watch_starts`].
	pub fn speculative(&self) -> impl Iterator<Item = u64> + '_ {
		self.walks.iter().filter_map(|walk| walk.trail.speculative)
	}

	/// The lowest sequence whose start could still move an unsettled time end not parked,
	/// `None` without one or once the last sequence is in range.
	pub fn watch_starts(&self) -> Option<u64> {
		self.walks.iter().filter_map(|walk| walk.trail.unknown()).min()
	}

	/// Whether group `sequence` is in range. Everything is while a relative anchor waits.
	pub fn contains(&self, sequence: u64) -> bool {
		self.pending || self.groups.contains(sequence)
	}

	/// Whether a datagram is in range, judged on its own and never through a time end's
	/// settling: its `timestamp` against each range's times, and its `sequence` against the
	/// range's absolute groups and the relative ones resolved so far.
	pub fn admits_datagram(&self, sequence: u64, timestamp: Option<Timestamp>) -> bool {
		let Some(timestamp) = timestamp else {
			return self.contains(sequence);
		};
		self.admission.admits(sequence, timestamp)
	}

	/// The lowest group in range, `None` while a relative anchor waits. Nothing in range
	/// floors nowhere: group 0.
	pub fn floor(&self) -> Option<u64> {
		match self.pending {
			true => None,
			false => Some(self.groups.lowest().unwrap_or(0)),
		}
	}

	/// The lowest group the ranges name, `None` while one reaches back to the oldest group
	/// or a relative anchor waits on a first group.
	pub fn start(&self) -> Option<u64> {
		match self.pending || !self.all_named {
			true => None,
			false => self.named,
		}
	}

	/// The lowest group a range names, `None` when none does or a relative anchor waits.
	/// Unlike [`Self::start`], a range reaching back to the oldest group names nothing.
	pub fn named(&self) -> Option<u64> {
		match self.pending {
			true => None,
			false => self.named,
		}
	}

	/// Each range resolved, in the order given: `None` for one empty as given or still
	/// waiting on a first group. One that only resolves empty keeps its span, ending at or
	/// below its start. An unsettled time end is reported open, never at its cap.
	pub fn resolved(&self) -> Vec<Option<Resolved>> {
		let mut resolved = vec![None; self.ranges.len()];
		for span in &self.spans {
			resolved[span.index] = Some(Resolved {
				start: span.start,
				end: match span.end {
					End::Before(end) => Some(end),
					End::Open | End::Time(_) => None,
				},
			});
		}
		resolved
	}

	/// Where each range starts once delivery starts at `from`, in the order given: `None`
	/// for one with no group there, or still waiting on a first group.
	pub fn starts(&self, from: u64) -> Vec<Option<u64>> {
		self.resolved()
			.into_iter()
			.map(|r| {
				let r = r?;
				let start = r.start.max(from);
				r.end.is_none_or(|end| end > start).then_some(start)
			})
			.collect()
	}

	/// The exclusive end past every group in range, `None` when one stays open. An
	/// unsettled time end counts as open: its cap only holds reads back meanwhile.
	pub fn end(&self) -> Option<u64> {
		if !self.settled {
			return None;
		}
		match self.groups.highest() {
			Some(last) => last.checked_add(1),
			None => Some(0),
		}
	}

	/// Whether `accounted` holds every group in range from `from` up to the exclusive `cap`,
	/// `false` while a range stays open (see [`Self::end`]).
	///
	/// The caller only grows `accounted` between checks, so each resumes where the last
	/// stopped unless `from` fell below its floor: one made on every wake visits each range
	/// once over the subscription, not once per wake. The last stop never passes its cap, so
	/// a wider one is checked past it anyway.
	pub fn covered_by(&mut self, from: u64, cap: Option<u64>, accounted: &rangemap::RangeSet<u64>) -> bool {
		let Some(end) = self.end() else {
			return false;
		};
		let end = cap.map_or(end, |cap| cap.min(end));
		let mut through = match self.covered {
			Some((floor, through)) if floor <= from => through.max(from),
			_ => from,
		};
		let covered = self.runs(through).take_while(|run| *run.start() < end).all(|run| {
			let run_end = run.end().checked_add(1).map_or(end, |next| next.min(end));
			let whole = accounted.gaps(&(*run.start()..run_end)).next().is_none();
			if whole {
				through = run_end;
			}
			whole
		});
		self.covered = Some((from, through));
		covered
	}

	/// The groups in range from `from` on, lowest first.
	pub fn runs(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		self.from(&self.groups, from)
	}

	/// The groups in range from `from` on that a miss may ask for, lowest first: all of
	/// them but those below the oldest group held when a time start fell before it.
	pub fn missable(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		self.from(&self.missable, from)
	}

	fn from<'a>(&'a self, groups: &'a Layered, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + 'a {
		let all = self.pending.then_some(from..=u64::MAX);
		let runs = (!self.pending).then(|| groups.runs(from));
		all.into_iter().chain(runs.into_iter().flatten())
	}

	/// The groups in range, `None` while a relative anchor waits and every group is.
	pub fn groups(&self) -> Option<&Layered> {
		(!self.pending).then_some(&self.groups)
	}

	/// The groups a miss may ask for, as [`Self::groups`].
	pub fn missable_groups(&self) -> Option<&Layered> {
		(!self.pending).then_some(&self.missable)
	}

	/// The first run of `range` that is in range.
	pub fn clip(&self, range: ops::Range<u64>) -> Option<ops::Range<u64>> {
		let last = range.end.checked_sub(1).filter(|last| *last >= range.start)?;
		if self.pending {
			return Some(range);
		}
		let run = self.groups.first(range.start, last)?;
		let end = match run.end().checked_add(1) {
			Some(end) => end.min(range.end),
			None => range.end,
		};
		Some((*run.start()).max(range.start)..end)
	}

	fn place(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> Result<()> {
		let time = |anchor: Option<Anchor>| matches!(anchor, Some(Anchor::Time(_)));
		if !holdings.timed()
			&& self
				.ranges
				.iter()
				.any(|range| time(Some(range.start)) || time(range.end))
		{
			return Err(Error::TimestampMismatch);
		}
		if holdings.drops_anchors() && !self.ranges.iter().all(Range::is_wire) {
			return Err(Error::Unsupported);
		}
		self.spans.clear();
		self.walks.clear();
		self.parked.clear();
		let (mut fixed, mut fixed_missable) = (Vec::new(), Vec::new());
		self.pending = false;
		// The time ends that start alike walk alike, until a group starts between them.
		let mut walks = std::collections::BTreeMap::<(u64, u64), BTreeSet<Ending>>::new();
		for (index, range) in self.ranges.iter().enumerate().filter(|(_, range)| !range.is_empty()) {
			let (Some((start, floored)), Some(end)) = (start(range.start, latest, holdings), end(range.end, latest))
			else {
				self.pending = true;
				continue;
			};
			// The lowest group a miss asks for: a time start before every group held asks none
			// below the oldest, since which of them it reaches is unknown until they start.
			let reach = match range.start {
				Anchor::Time(_) if !floored => holdings.next_start(0).map_or(start, |(oldest, _)| oldest),
				_ => start,
			};
			let limit = match end {
				End::Before(end) => Some(end),
				End::Open => None,
				End::Time(pts) => {
					let span = self.spans.len();
					walks.entry((start, reach)).or_default().insert(Ending { pts, span });
					Some(start)
				}
			};
			if let Some(last) = limit.map_or(Some(u64::MAX), |end| end.checked_sub(1))
				&& last >= start
			{
				fixed.push(start..=last);
				if reach <= last {
					fixed_missable.push(reach..=last);
				}
			}
			self.spans.push(Span {
				index,
				start,
				end,
				floored,
			});
		}
		self.admission = Admission::new(&self.ranges, &self.spans);
		self.named = self
			.spans
			.iter()
			.filter(|span| span.floored)
			.map(|span| span.start)
			.min();
		self.all_named = self.spans.iter().all(|span| span.floored);
		self.resolved = Runs::new(fixed);
		self.resolved_missable = Runs::new(fixed_missable);
		self.groups = Layered::new(Arc::new(self.resolved.clone()), Runs::default());
		self.missable = Layered::new(Arc::new(self.resolved_missable.clone()), Runs::default());
		self.stale = false;
		self.walks = walks
			.into_iter()
			.map(|((start, reach), ends)| Walk {
				ends,
				trail: Trail::new(start, reach),
				low: start,
				low_reach: reach,
				was: Some(start),
			})
			.collect();
		self.walk(holdings);
		self.rebuild();
		Ok(())
	}

	/// The groups in range: the fixed ones, and the run each walk not parked reads now
	/// layered over them. Costs those walks, and the fixed runs only once a settle or a walk
	/// parking or waking changed them.
	fn rebuild(&mut self) {
		self.covered = None;
		self.settled = !self.pending && self.walks.is_empty() && self.parked.is_empty();
		let (groups, missable): (Vec<_>, Vec<_>) = self.walks.iter().map(Walk::runs).unzip();
		self.groups.set_layer(Runs::new(groups.into_iter().flatten()));
		self.missable.set_layer(Runs::new(missable.into_iter().flatten()));
		if std::mem::take(&mut self.stale) {
			let parked = || {
				self.parked
					.values()
					.flat_map(|parked| parked.walks.iter())
					.map(Walk::runs)
			};
			let groups = self
				.resolved
				.iter()
				.cloned()
				.chain(parked().filter_map(|(groups, _)| groups));
			let missable =
				(self.resolved_missable.iter().cloned()).chain(parked().filter_map(|(_, missable)| missable));
			let (groups, missable) = (Runs::new(groups), Runs::new(missable));
			self.groups.set_fixed(groups);
			self.missable.set_fixed(missable);
		}
	}
}

/// The ranges' bounds on a datagram, split by the kind of anchor at each end, so judging one
/// costs a few sorted lookups however many ranges there are.
///
/// A datagram is one frame, so a time anchor judges its own timestamp, and a group anchor
/// its sequence: each range bounds it from below by one and from above by one. Within each
/// pairing, the ranges sorted by their lower bound carry the highest upper bound of any
/// range at or below them, so the ranges whose lower bound admits a datagram admit it once
/// the last of them reaches it.
#[derive(Debug, Default)]
struct Admission {
	groups: Stab<u64, u64>,
	group_to_time: Stab<u64, Timestamp>,
	time_to_group: Stab<Timestamp, u64>,
	times: Stab<Timestamp, Timestamp>,
}

/// One bound of a range on a datagram: the first or last sequence it admits, or the time
/// it presents from or through.
#[derive(Clone, Copy)]
enum Bound {
	Group(u64),
	Time(Timestamp),
}

impl Admission {
	/// Index `ranges`, with `spans` their resolution in the same order.
	fn new(ranges: &[Range], spans: &[Span]) -> Self {
		let mut admission = Self::default();
		let mut spans = spans.iter().peekable();
		for (index, range) in ranges.iter().enumerate().filter(|(_, range)| !range.is_empty()) {
			// A relative anchor still waiting on a latest group bounds nothing yet.
			let span = spans.next_if(|span| span.index == index);
			let lower = match range.start {
				Anchor::Time(pts) => Bound::Time(pts),
				Anchor::Absolute(start) => Bound::Group(start),
				Anchor::Relative(_) => Bound::Group(span.map_or(0, |span| span.start)),
			};
			// The last sequence a group end admits: one ending at group 0 admits none.
			let last = |end: u64| end.checked_sub(1).map(Bound::Group);
			let upper = match range.end {
				None => Some(Bound::Group(u64::MAX)),
				Some(Anchor::Time(pts)) => Some(Bound::Time(pts)),
				Some(Anchor::Absolute(end)) => last(end),
				Some(Anchor::Relative(_)) => match span.map(|span| span.end) {
					Some(End::Before(end)) => last(end),
					_ => Some(Bound::Group(u64::MAX)),
				},
			};
			match (lower, upper) {
				(_, None) => {}
				(Bound::Group(lower), Some(Bound::Group(upper))) => admission.groups.push(lower, upper),
				(Bound::Group(lower), Some(Bound::Time(upper))) => admission.group_to_time.push(lower, upper),
				(Bound::Time(lower), Some(Bound::Group(upper))) => admission.time_to_group.push(lower, upper),
				(Bound::Time(lower), Some(Bound::Time(upper))) => admission.times.push(lower, upper),
			}
		}
		admission.groups.index();
		admission.group_to_time.index();
		admission.time_to_group.index();
		admission.times.index();
		admission
	}

	/// Whether any one range admits a datagram's `sequence` and `timestamp` together.
	fn admits(&self, sequence: u64, timestamp: Timestamp) -> bool {
		self.groups.admits(sequence, sequence)
			|| self.group_to_time.admits(sequence, timestamp)
			|| self.time_to_group.admits(timestamp, sequence)
			|| self.times.admits(timestamp, timestamp)
	}
}

/// A point a datagram is judged at, ordered as instants for a time.
trait Point: Copy {
	fn order(self, other: Self) -> std::cmp::Ordering;
}

impl Point for u64 {
	fn order(self, other: Self) -> std::cmp::Ordering {
		self.cmp(&other)
	}
}

impl Point for Timestamp {
	fn order(self, other: Self) -> std::cmp::Ordering {
		instant(self, other)
	}
}

/// Ranges bounded below by an `L` and above by a `U`, both inclusive, sorted by the lower
/// bound, each carrying the highest upper bound at or before it once indexed.
#[derive(Debug)]
struct Stab<L, U> {
	bounds: Vec<(L, U)>,
}

impl<L, U> Default for Stab<L, U> {
	fn default() -> Self {
		Self { bounds: Vec::new() }
	}
}

impl<L: Point, U: Point> Stab<L, U> {
	fn push(&mut self, lower: L, upper: U) {
		self.bounds.push((lower, upper));
	}

	fn index(&mut self) {
		self.bounds.sort_by(|a, b| a.0.order(b.0));
		let mut highest: Option<U> = None;
		for (_, upper) in &mut self.bounds {
			let top = match highest {
				Some(top) if top.order(*upper).is_gt() => top,
				_ => *upper,
			};
			*upper = top;
			highest = Some(top);
		}
	}

	/// Whether a range bounded at or below `lower` reaches `upper`.
	fn admits(&self, lower: L, upper: U) -> bool {
		let below = self.bounds.partition_point(|(start, _)| start.order(lower).is_le());
		below
			.checked_sub(1)
			.is_some_and(|last| upper.order(self.bounds[last].1).is_le())
	}
}

/// The first group a range starts at, and whether it names that group rather than
/// reaching back to the oldest one. `None` while it waits on a latest group.
fn start(anchor: Anchor, latest: Option<u64>, holdings: &impl Holdings) -> Option<(u64, bool)> {
	match anchor {
		Anchor::Absolute(start) => Some((start, true)),
		// Nothing sits below group 0, so the farthest offset needs no latest group.
		Anchor::Relative(u64::MAX) => Some((0, false)),
		Anchor::Relative(back) => Some((latest?.saturating_sub(back), true)),
		Anchor::Time(pts) => match holdings.containing(pts) {
			Containing::Group(group) => Some((group, true)),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Some((0, false)),
			// Nothing has started yet: like the live edge, the latest group.
			Containing::Unknown => Some((latest?, true)),
		},
	}
}

/// Where a range stops, or `None` while it waits on a latest group. A time end's walk
/// starts once its start is known.
fn end(anchor: Option<Anchor>, latest: Option<u64>) -> Option<End> {
	Some(match anchor {
		None => End::Open,
		Some(Anchor::Absolute(end)) => End::Before(end),
		Some(Anchor::Relative(back)) => latest?
			.saturating_sub(back)
			.checked_add(1)
			.map_or(End::Open, End::Before),
		Some(Anchor::Time(pts)) => End::Time(pts),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	use rangemap::RangeInclusiveSet;

	use std::cell::Cell;

	fn at(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// A hop's groups by sequence, the runs that can no longer arrive, and how many held
	/// groups and starts a walk asked about.
	///
	/// Reports changes as a track does: a group arriving and a run given up, never a group
	/// starting or ending empty, which its ledger never sees.
	#[derive(Default)]
	struct Groups {
		held: std::collections::BTreeMap<u64, Held>,
		gone: Vec<ops::Range<u64>>,
		complete: bool,
		asked: Cell<usize>,
		/// Every change so far, oldest first, and what was last logged: the held sequences
		/// and how many runs were given up.
		log: std::cell::RefCell<(Vec<ops::Range<u64>>, BTreeSet<u64>, usize)>,
	}

	impl Groups {
		fn new(held: impl IntoIterator<Item = (u64, Held)>) -> Self {
			Self {
				held: held.into_iter().collect(),
				..Self::default()
			}
		}

		fn started(sequence: u64, millis: u64) -> (u64, Held) {
			(sequence, Held::Started(at(millis)))
		}
	}

	impl Holdings for Groups {
		fn timed(&self) -> bool {
			true
		}

		fn containing(&self, pts: Timestamp) -> Containing {
			let mut stamped = false;
			for (&sequence, held) in self.held.iter().rev() {
				let Held::Started(start) = held else { continue };
				stamped = true;
				if !later(*start, pts) {
					return Containing::Group(sequence);
				}
			}
			match stamped {
				true => Containing::Before,
				false => Containing::Unknown,
			}
		}

		fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
			self.asked.set(self.asked.get() + 1);
			self.held.range(from..).find_map(|(&sequence, held)| match held {
				Held::Started(start) => Some((sequence, *start)),
				_ => None,
			})
		}

		fn next_held(&self, from: u64) -> Option<(u64, Held)> {
			self.asked.set(self.asked.get() + 1);
			self.held
				.range(from..)
				.next()
				.map(|(&sequence, held)| (sequence, *held))
		}

		fn gone(&self, sequence: u64) -> Option<u64> {
			self.gone
				.iter()
				.find(|gone| gone.contains(&sequence))
				.map(|gone| gone.end)
		}

		fn complete(&self) -> bool {
			self.complete
		}

		fn changes(&self, seen: &mut Seen) -> Option<Vec<ops::Range<u64>>> {
			let mut log = self.log.borrow_mut();
			let (log, held, gone) = &mut *log;
			let arrived = self.held.keys().filter(|sequence| !held.contains(sequence));
			log.extend(arrived.map(|sequence| *sequence..sequence.saturating_add(1)));
			log.extend(self.gone[*gone..].iter().cloned());
			*held = self.held.keys().copied().collect();
			*gone = self.gone.len();
			let from = usize::try_from(seen.runs[0]).unwrap();
			let changed = log[from..].to_vec();
			let global = [u64::from(self.complete), 0, 0];
			let primed = seen.global == Some(global);
			*seen = Seen {
				runs: [log.len() as u64, 0],
				global: Some(global),
			};
			primed.then_some(changed)
		}
	}

	struct Untimed;

	impl Holdings for Untimed {
		fn timed(&self) -> bool {
			false
		}

		fn containing(&self, _: Timestamp) -> Containing {
			Containing::Unknown
		}

		fn next_start(&self, _: u64) -> Option<(u64, Timestamp)> {
			None
		}

		fn next_held(&self, _: u64) -> Option<(u64, Held)> {
			None
		}

		fn gone(&self, _: u64) -> Option<u64> {
			None
		}

		fn complete(&self) -> bool {
			false
		}
	}

	fn time(millis: u64) -> Anchor {
		Anchor::Time(Timestamp::from_millis(millis).unwrap())
	}

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn covered(window: &Window, upto: u64) -> Vec<u64> {
		(0..upto).filter(|sequence| window.contains(*sequence)).collect()
	}

	/// Disjoint ranges filter to exactly their groups; the gap between them is out.
	#[test]
	fn ranges_cover_only_their_groups() {
		let window = Window::new(&[Range::groups(2..4), Range::groups(7..)], Some(9), &Untimed).unwrap();
		assert_eq!(covered(&window, 10), vec![2, 3, 7, 8, 9]);
		assert_eq!(window.floor(), Some(2));
		assert_eq!(window.end(), None);
		assert_eq!(window.runs(3).collect::<Vec<_>>(), vec![3..=3, 7..=u64::MAX]);
		assert_eq!(window.clip(0..10), Some(2..4));
		assert_eq!(window.clip(4..10), Some(7..10));
		assert_eq!(window.clip(4..7), None);
		assert_eq!(
			window.resolved(),
			vec![
				Some(Resolved { start: 2, end: Some(4) }),
				Some(Resolved { start: 7, end: None }),
			]
		);
		// Delivery starting inside a range raises its start; past its end leaves it none.
		assert_eq!(window.starts(3), [Some(3), Some(7)]);
		assert_eq!(window.starts(5), [None, Some(7)]);

		// Nothing asked for is nothing in range, floored nowhere.
		let window = Window::new(&[], None, &Untimed).unwrap();
		assert_eq!(covered(&window, 4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// Relative anchors count back from the latest group, both ends whole groups, and wait
	/// for a first group when there is none.
	#[test]
	fn relative_anchors_count_back_from_the_latest_group() {
		let window = Window::new(&[range(Anchor::Relative(3), Anchor::Relative(1))], Some(7), &Untimed).unwrap();
		assert_eq!(covered(&window, 10), vec![4, 5, 6]);

		let mut window = Window::new(&[Range::live()], None, &Untimed).unwrap();
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Untimed));
		assert!(window.settle(Some(5), &Untimed));
		assert_eq!(covered(&window, 8), vec![5, 6, 7]);
		assert!(window.is_settled());

		// The farthest offset needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None, &Untimed).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let start = |ranges: &[Range], latest| Window::new(ranges, latest, &Untimed).unwrap().start();
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(9)), Some(4));
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(2)), Some(2));
		assert_eq!(start(&[Range::groups(0..)], None), Some(0));
		assert_eq!(start(&[Range::groups(4..), Range::live()], None), None);
		assert_eq!(start(&[Range::groups(4..), Range::oldest()], Some(9)), None);
		assert_eq!(start(&[Range::groups(4..), Range::groups(..2)], Some(9)), None);
	}

	/// A time start is the group containing it, and an end stops just past that group once
	/// a later group is known to start after it with nothing between left to arrive.
	#[test]
	fn time_anchors_resolve_from_group_starts() {
		let groups = Groups::new([
			Groups::started(2, 2000),
			Groups::started(3, 3000),
			Groups::started(4, 4000),
			Groups::started(5, 5000),
			(6, Held::Pending),
		]);
		let window = Window::new(&[range(time(3500), time(4500))], Some(6), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![3, 4]);
		assert!(window.is_settled());
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 3, end: Some(5) })]);

		// Group 6 may yet start at 5000 too: read on speculation, the end left open.
		let window = Window::new(&[range(time(3500), time(5000))], Some(6), &groups).unwrap();
		assert!(!window.is_settled());
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6]);
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 3, end: None })]);
		assert_eq!(window.end(), None, "the cap is not the end");
		assert_eq!(window.speculative().collect::<Vec<_>>(), vec![6]);

		// Before every start held: reach back to the oldest.
		let window = Window::new(&[range(time(1000), time(2500))], Some(6), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![0, 1, 2]);

		// Nothing started yet: a time start waits like the live edge.
		let mut window = Window::new(&[range(time(1000), None)], None, &Groups::default()).unwrap();
		assert!(window.is_pending());
		assert!(window.settle(Some(4), &Groups::new([(4, Held::Pending)])));
		assert_eq!(window.floor(), Some(4));
	}

	/// An end settles only once every sequence between the newest group starting inside it
	/// and the first starting past it is accounted for.
	#[test]
	fn a_time_end_settles_once_nothing_below_its_end_can_arrive() {
		let mut groups = Groups::new([Groups::started(3, 3000), Groups::started(6, 6000)]);
		let mut window = Window::new(&[range(time(3000), time(4000))], Some(6), &groups).unwrap();
		assert!(!window.is_settled(), "4 and 5 may still arrive inside the end");
		assert_eq!(
			covered(&window, 8),
			vec![3, 4],
			"only 4, the first that may still arrive, on speculation, and never 6"
		);

		// 4 arrives yet to start: still the one read on speculation, so 5 waits.
		groups.held.insert(4, Held::Pending);
		assert!(!window.settle(Some(6), &groups), "4 was already the speculative one");
		assert_eq!(covered(&window, 8), vec![3, 4]);
		groups.held.insert(4, Held::Started(at(3500)));
		assert!(window.settle(Some(6), &groups));
		assert!(!window.is_settled(), "5 may still arrive");
		groups.gone.push(5..6);
		assert!(window.settle(Some(6), &groups));
		assert!(window.is_settled());
		assert_eq!(covered(&window, 8), vec![3, 4, 5]);

		// A group that never starts proves nothing, so it never holds the end open.
		let groups = Groups::new([Groups::started(3, 3000), (4, Held::Never), Groups::started(5, 5000)]);
		let window = Window::new(&[range(time(3000), time(4000))], Some(5), &groups).unwrap();
		assert!(window.is_settled());
		assert_eq!(covered(&window, 8), vec![3, 4]);
	}

	/// An unsettled time end reads at most one group on speculation: the first above the
	/// newest known to start inside it that may still start. A group that never starts
	/// does not take that place, and a stalled one does not hold back a later group known
	/// to start inside the end.
	#[test]
	fn an_unsettled_time_end_reads_one_group_on_speculation() {
		let mut groups = Groups::new([(3, Held::Pending), (4, Held::Pending), (5, Held::Pending)]);
		let mut window = Window::new(&[range(Anchor::Absolute(3), time(3500))], Some(5), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![3]);

		groups.held.insert(3, Held::Started(at(3000)));
		assert!(window.settle(Some(5), &groups));
		assert_eq!(covered(&window, 8), vec![3, 4]);

		// 4 never starts, so 5 is the one read on speculation.
		groups.held.insert(4, Held::Never);
		assert!(window.settle(Some(5), &groups));
		assert_eq!(covered(&window, 8), vec![3, 4, 5]);

		// 5 stalls, 6 starts inside the end: 5 is in range then, and 7 is speculative.
		groups.held.insert(6, Held::Started(at(3400)));
		groups.held.insert(7, Held::Pending);
		assert!(window.settle(Some(7), &groups));
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6, 7]);

		// 7 starts past the end, which settles with 5 still stalled below it.
		groups.held.insert(7, Held::Started(at(3600)));
		assert!(window.settle(Some(7), &groups));
		assert!(window.is_settled());
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6]);

		// Every held group accounted for: the next to arrive is the speculative one.
		let groups = Groups::new([Groups::started(3, 3000)]);
		let window = Window::new(&[range(time(3000), time(3500))], Some(3), &groups).unwrap();
		assert!(window.contains(4));

		// Nothing more can arrive or start: uncapped.
		let mut groups = Groups::new([Groups::started(3, 3000), (4, Held::Pending), (5, Held::Pending)]);
		groups.complete = true;
		let window = Window::new(&[range(time(3000), time(3500))], Some(5), &groups).unwrap();
		assert_eq!(covered(&window, 7), vec![3, 4, 5, 6]);
	}

	/// A peer that leaves an early sequence missing and keeps sending later groups costs a
	/// time end each group once, not a rescan of everything since the gap on every settle.
	#[test]
	fn a_time_end_walks_each_group_once_behind_a_missing_sequence() {
		let mut groups = Groups::new([Groups::started(0, 0)]);
		let mut window = Window::new(&[range(time(0), time(1_000_000))], Some(0), &groups).unwrap();
		let count = 2_000;
		for sequence in 2..count {
			groups.held.insert(sequence, Held::Started(at(sequence * 10)));
			window.settle(Some(sequence), &groups);
		}
		assert!(!window.is_settled(), "group 1 never arrived");
		assert!(window.contains(count - 1));
		let asked = groups.asked.get();
		assert!(asked < 8 * count as usize, "{asked} queries for {count} groups");
	}

	/// Time ends that start alike share one walk, so a settle costs the walk, not the ends.
	/// A group starting between them splits off and settles the ends before it.
	#[test]
	fn time_ends_that_start_alike_share_one_walk() {
		let ends = 1_000;
		let mut groups = Groups::new([Groups::started(0, 0)]);
		let ranges: Vec<Range> = (0..ends).map(|i| range(time(0), time(1_000_000 + i))).collect();
		let mut window = Window::new(&ranges, Some(0), &groups).unwrap();
		assert_eq!(window.walks.len(), 1);

		let count = 200;
		let asked = groups.asked.get();
		for sequence in 1..count {
			groups.held.insert(sequence, Held::Started(at(sequence)));
			window.settle(Some(sequence), &groups);
		}
		let asked = groups.asked.get() - asked;
		assert!(asked < 8 * count as usize, "{asked} queries for {count} groups");
		assert_eq!(window.walks.len(), 1);
		assert!(!window.is_settled() && window.contains(count));

		// Group 200 starts inside the later half of the ends: the earlier half settles before it.
		groups.held.insert(count, Held::Started(at(1_000_000 + ends / 2)));
		assert!(window.settle(Some(count), &groups));
		assert_eq!(window.walks.len(), 1);
		let resolved = window.resolved();
		let split = usize::try_from(ends / 2).unwrap();
		assert!(resolved[..split].iter().all(|r| *r
			== Some(Resolved {
				start: 0,
				end: Some(count)
			})));
		assert!(
			resolved[split..]
				.iter()
				.all(|r| *r == Some(Resolved { start: 0, end: None }))
		);

		// Once a group starts past every end, they all settle.
		groups.held.insert(count + 1, Held::Started(at(2_000_000)));
		assert!(window.settle(Some(count + 1), &groups));
		assert!(window.is_settled() && window.walks.is_empty());
		assert_eq!(covered(&window, count + 3), (0..=count).collect::<Vec<_>>());
	}

	/// Shared walks resolve every range as walking it alone would, as groups arrive, start,
	/// never start, or go missing in any order, with ends in mixed scales.
	#[test]
	fn shared_walks_match_each_end_walked_alone() {
		// A small deterministic generator, so a failure replays.
		let seed = Cell::new(0x2545_f491_4f6c_dd1d_u64);
		let next = |bound: u64| {
			let mut x = seed.get();
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			seed.set(x);
			x % bound
		};
		let micros = |value| Anchor::Time(Timestamp::new(value, crate::Timescale::MICRO).unwrap());
		const SEQUENCES: u64 = 16;
		for round in 0..400 {
			// Each group starts in its own ten milliseconds, so starts ascend with the sequence.
			let starts: Vec<u64> = (0..SEQUENCES).map(|sequence| sequence * 10 + next(10)).collect();
			let pts = |end: bool| match next(3) {
				0 => micros(next(SEQUENCES * 10_000)),
				// Exactly a group's start, in either scale.
				1 if next(2) == 0 => time(starts[next(SEQUENCES) as usize]),
				1 => micros(starts[next(SEQUENCES) as usize] * 1000),
				_ => time(next(SEQUENCES * 10) + u64::from(end) * 5),
			};
			let ranges: Vec<Range> = (0..1 + next(12))
				.map(|_| {
					let start = match next(3) {
						0 => Anchor::Absolute(next(SEQUENCES)),
						1 => Anchor::Relative(next(4)),
						_ => pts(false),
					};
					let end = match next(6) {
						0 => None,
						1 => Some(Anchor::Absolute(next(SEQUENCES + 2))),
						_ => Some(pts(true)),
					};
					range(start, end)
				})
				.collect();

			let mut groups = Groups::default();
			for _ in 0..next(6) {
				let sequence = next(SEQUENCES);
				groups
					.held
					.insert(sequence, Held::Started(at(starts[sequence as usize])));
			}
			let latest = Some(next(SEQUENCES));
			let mut window = Window::new(&ranges, latest, &groups).unwrap();
			let mut alone: Vec<Window> = ranges
				.iter()
				.map(|range| Window::new(std::slice::from_ref(range), latest, &groups).unwrap())
				.collect();

			for step in 0..40 {
				let sequence = next(SEQUENCES);
				let started = Held::Started(at(starts[sequence as usize]));
				match next(6) {
					0 | 1 => {
						groups.held.entry(sequence).or_insert(Held::Pending);
					}
					2 | 3 => {
						if groups.held.get(&sequence).is_none_or(|held| *held == Held::Pending) {
							groups.held.insert(sequence, started);
						}
					}
					4 => {
						if groups.held.get(&sequence).is_none_or(|held| *held == Held::Pending) {
							groups.held.insert(sequence, Held::Never);
						}
					}
					_ => {
						let end = sequence + 1 + next(3);
						if groups.held.range(sequence..end).next().is_none() {
							groups.gone.push(sequence..end);
						}
					}
				}
				groups.complete |= step == 39 && round % 4 == 0;

				let before: Vec<u64> = covered(&window, SEQUENCES + 4);
				let moved = window.settle(latest, &groups);
				for window in &mut alone {
					window.settle(latest, &groups);
				}
				let context = || format!("round {round} step {step}: {ranges:?} over {:?}", groups.held);

				let union: Vec<u64> = (0..SEQUENCES + 4)
					.filter(|sequence| alone.iter().any(|window| window.contains(*sequence)))
					.collect();
				let now = covered(&window, SEQUENCES + 4);
				assert_eq!(now, union, "{}", context());
				assert!(moved || now == before, "{}: moved unreported", context());

				let mut missable = RangeInclusiveSet::new();
				for run in alone.iter().flat_map(|window| window.missable(0)) {
					missable.insert(run);
				}
				assert!(window.missable(0).eq(missable.iter().cloned()), "{}", context());

				let resolved: Vec<_> = alone.iter().map(|window| window.resolved()[0]).collect();
				assert_eq!(window.resolved(), resolved, "{}", context());
				assert_eq!(
					window.is_settled(),
					alone.iter().all(Window::is_settled),
					"{}",
					context()
				);
				let speculative: std::collections::BTreeSet<u64> = window.speculative().collect();
				let each: std::collections::BTreeSet<u64> = alone.iter().flat_map(Window::speculative).collect();
				assert_eq!(speculative, each, "{}", context());
				assert_eq!(
					window.watch_starts(),
					alone.iter().filter_map(Window::watch_starts).min(),
					"{}",
					context()
				);
			}
		}
	}

	/// A datagram is one frame, so its own timestamp places it against a time end.
	#[test]
	fn a_datagram_is_judged_by_its_own_timestamp() {
		let window = Window::new(&[range(Anchor::Absolute(0), time(3500))], None, &Groups::default()).unwrap();
		assert!(window.admits_datagram(9, Some(at(3500))));
		assert!(!window.admits_datagram(9, Some(at(3600))));
	}

	/// Groups whose `racing` one starts past the end after the first time it is asked
	/// about, as when its first frame lands between the walks of one settle.
	struct Racing {
		groups: Groups,
		racing: u64,
		asked: Cell<bool>,
	}

	impl Holdings for Racing {
		fn timed(&self) -> bool {
			true
		}

		fn containing(&self, pts: Timestamp) -> Containing {
			self.groups.containing(pts)
		}

		fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
			self.groups.next_start(from)
		}

		fn next_held(&self, from: u64) -> Option<(u64, Held)> {
			let next = self.groups.next_held(from);
			if next.is_some_and(|(sequence, _)| sequence == self.racing) && self.asked.replace(true) {
				return Some((self.racing, Held::Started(at(6000))));
			}
			next
		}

		fn gone(&self, sequence: u64) -> Option<u64> {
			self.groups.gone(sequence)
		}

		fn complete(&self) -> bool {
			false
		}
	}

	#[test]
	fn a_time_end_settles_on_a_group_that_starts_between_its_walks() {
		let racing = Racing {
			groups: Groups::new([Groups::started(0, 0), (1, Held::Pending), Groups::started(2, 7000)]),
			racing: 1,
			asked: Cell::new(false),
		};
		let window = Window::new(&[range(Anchor::Absolute(0), time(5000))], None, &racing).unwrap();
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(1) })]);
	}

	#[test]
	fn a_missing_sequence_past_a_time_end_is_the_one_read_on_speculation() {
		// 1 and 2 are missing, and 3 never starts: the walk may not pass the gap, or every
		// sequence in it that arrives later is read past the end.
		let mut groups = Groups::new([Groups::started(0, 3000), (3, Held::Never)]);
		let mut window = Window::new(&[range(time(3000), time(3500))], Some(3), &groups).unwrap();
		assert_eq!(covered(&window, 5), vec![0, 1]);
		assert_eq!(window.speculative().collect::<Vec<_>>(), vec![1]);
		groups.held.insert(2, Held::Pending);
		groups.held.insert(1, Held::Pending);
		window.settle(Some(3), &groups);
		assert_eq!(covered(&window, 5), vec![0, 1], "2 waits on 1 starting");
		groups.held.insert(1, Held::Started(at(3600)));
		window.settle(Some(3), &groups);
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(1) })]);
	}

	#[test]
	fn a_time_end_stops_walking_at_the_last_sequence() {
		let groups = Groups::new([Groups::started(u64::MAX, 3000)]);
		let window = Window::new(&[range(Anchor::Absolute(u64::MAX), time(5000))], None, &groups).unwrap();
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: u64::MAX,
				end: None
			})]
		);
		assert!(window.contains(u64::MAX));
		assert_eq!(window.watch_starts(), None, "nothing above the last sequence can start");
	}

	#[test]
	fn a_settled_window_still_judges_a_datagram_by_its_times() {
		// 1 is a datagram the starts index never sees: the end settles before 2 by
		// sequence, but 1 presents past it.
		let groups = Groups {
			gone: std::iter::once(1..2).collect(),
			..Groups::new([Groups::started(0, 0), Groups::started(2, 10_000)])
		};
		let window = Window::new(&[range(time(0), time(5000))], None, &groups).unwrap();
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(2) })]);
		assert!(window.admits_datagram(1, Some(at(4000))));
		assert!(!window.admits_datagram(1, Some(at(7000))));
	}

	#[test]
	fn a_waiting_window_judges_a_datagram_against_both_times() {
		let window = Window::new(&[range(time(3000), time(3500))], None, &Groups::default()).unwrap();
		assert!(window.is_pending(), "no group has started");
		assert!(!window.admits_datagram(0, Some(at(2900))));
		assert!(window.admits_datagram(1, Some(at(3000))));
		assert!(window.admits_datagram(2, Some(at(3500))));
		assert!(!window.admits_datagram(3, Some(at(3600))));
	}

	#[test]
	fn a_waiting_window_judges_a_datagram_against_absolute_groups_too() {
		let window = Window::new(
			&[range(time(3000), Some(Anchor::Absolute(10)))],
			None,
			&Groups::default(),
		)
		.unwrap();
		assert!(window.is_pending(), "no group has started");
		assert!(window.admits_datagram(9, Some(at(3500))));
		assert!(!window.admits_datagram(10, Some(at(3500))));
		assert!(!window.admits_datagram(9, Some(at(2900))));
	}

	/// A datagram is in range only when one range admits its sequence and time together,
	/// not when each passes a different range.
	#[test]
	fn a_datagram_is_judged_against_one_range_at_a_time() {
		let groups = Groups::new([Groups::started(5, 5000)]);
		// The time range covers group 5 whole, but only from 5500 on.
		let window = Window::new(
			&[range(Anchor::Absolute(6), None), range(time(5500), None)],
			Some(5),
			&groups,
		)
		.unwrap();
		assert!(!window.admits_datagram(5, Some(at(5000))));
		assert!(window.admits_datagram(5, Some(at(5500))));
		assert!(window.admits_datagram(6, Some(at(5000))));
	}

	/// The indexed admission agrees with judging each range on its own, over mixed anchors,
	/// scales, and relative anchors resolved or still waiting.
	#[test]
	fn datagram_admission_matches_each_range_judged_alone() {
		fn admits_alone(window: &Window, sequence: u64, timestamp: Timestamp) -> bool {
			window.ranges.iter().enumerate().any(|(index, range)| {
				if range.is_empty() {
					return false;
				}
				let span = window.spans.iter().find(|span| span.index == index);
				let started = match range.start {
					Anchor::Time(pts) => !later(pts, timestamp),
					Anchor::Absolute(start) => sequence >= start,
					Anchor::Relative(_) => span.is_none_or(|span| sequence >= span.start),
				};
				let ended = match range.end {
					None => true,
					Some(Anchor::Time(pts)) => !later(timestamp, pts),
					Some(Anchor::Absolute(end)) => sequence < end,
					Some(Anchor::Relative(_)) => {
						span.is_none_or(|span| !matches!(span.end, End::Before(end) if sequence >= end))
					}
				};
				started && ended
			})
		}

		// A small deterministic generator, so a failure replays.
		let seed = Cell::new(0x9e37_79b9_7f4a_7c15_u64);
		let next = |bound: u64| {
			let mut x = seed.get();
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			seed.set(x);
			x % bound
		};
		let micros = |value| Timestamp::new(value, crate::Timescale::MICRO).unwrap();
		let groups = Groups::new((0..20).map(|sequence| Groups::started(sequence, sequence * 100)));
		for round in 0..300 {
			let anchor = |end: bool| match next(if end { 4 } else { 3 }) {
				0 => Anchor::Absolute(next(20)),
				1 => Anchor::Relative(next(6)),
				2 if next(2) == 0 => time(next(2000)),
				2 => Anchor::Time(micros(next(2_000_000))),
				_ => Anchor::Absolute(u64::MAX),
			};
			let ranges: Vec<Range> = (0..1 + next(6))
				.map(|_| {
					let start = anchor(false);
					let end = match next(4) {
						0 => None,
						_ => Some(anchor(true)),
					};
					range(start, end)
				})
				.collect();
			let latest = (round % 3 != 0).then(|| next(20));
			let held = match latest {
				Some(_) => &groups,
				None => &Groups::default(),
			};
			let window = Window::new(&ranges, latest, held).unwrap();
			for _ in 0..40 {
				let sequence = next(24);
				let timestamp = match next(2) {
					0 => at(next(2400)),
					_ => micros(next(2_400_000)),
				};
				assert_eq!(
					window.admits_datagram(sequence, Some(timestamp)),
					admits_alone(&window, sequence, timestamp),
					"{ranges:?} at {latest:?}: datagram {sequence} at {timestamp:?}"
				);
			}
		}
	}

	/// A coverage check resumes where the last stopped, but not below its floor or past its
	/// cap: a lower floor or a wider cap reaches groups no check has seen.
	#[test]
	fn coverage_resumes_within_what_was_checked() {
		let mut window = Window::new(&[Range::groups(0..2), Range::groups(4..6)], None, &Untimed).unwrap();
		let accounted = rangemap::RangeSet::from_iter([0..1, 4..6]);
		assert!(window.covered_by(4, None, &accounted));
		assert!(
			!window.covered_by(0, None, &accounted),
			"group 1 is owed below the old floor"
		);

		let accounted = rangemap::RangeSet::from_iter([0..2, 4..5]);
		assert!(window.covered_by(0, Some(5), &accounted));
		assert!(
			!window.covered_by(0, None, &accounted),
			"group 5 is owed past the old cap"
		);
		assert!(window.covered_by(1, Some(5), &accounted), "a raised floor resumes");
	}

	#[test]
	fn an_untimed_track_refuses_time_anchors() {
		assert!(matches!(
			Window::new(&[range(time(0), None)], None, &Untimed),
			Err(Error::TimestampMismatch)
		));
		assert!(matches!(
			Window::new(&[range(Anchor::Relative(0), time(0))], None, &Untimed),
			Err(Error::TimestampMismatch)
		));
	}
}

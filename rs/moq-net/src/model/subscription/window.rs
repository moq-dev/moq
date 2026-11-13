//! A subscriber's own ranges, resolved to the group starts it reads.
//!
//! The aggregate a publisher sees is the union of every subscriber's ranges, so it says
//! what arrives, not what one subscriber asked for. A [`Window`] is the per-subscriber
//! side: resolved at the hop serving the subscriber, it filters that subscriber's reads.

use std::{ops, sync::Arc};

use super::{Anchor, Layered, Range, Runs};
use crate::{Error, Result};

/// One range resolved to groups: from `start` up to the exclusive `end`, `None` while it
/// stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
	pub start: u64,
	pub end: Option<u64>,
	/// Whether the start names a group, rather than reaching back to the oldest one (a
	/// time start older than every group held, or the oldest start).
	pub floored: bool,
}

/// The groups a publisher accounted for, which [`Window::covered_by`] checks against: a
/// set, or a map keyed by them, read in place so a check copies nothing.
pub(crate) trait Accounted {
	/// The first group in `range` not accounted for, `None` when every one is.
	fn first_gap(&self, range: &ops::Range<u64>) -> Option<u64>;
}

impl Accounted for rangemap::RangeSet<u64> {
	fn first_gap(&self, range: &ops::Range<u64>) -> Option<u64> {
		self.gaps(range).next().map(|gap| gap.start)
	}
}

impl<V: Clone + Eq> Accounted for rangemap::RangeMap<u64, V> {
	fn first_gap(&self, range: &ops::Range<u64>) -> Option<u64> {
		self.gaps(range).next().map(|gap| gap.start)
	}
}

/// The groups a serving hop holds, by start, for resolving relative and time anchors.
pub(in crate::model) trait Holdings {
	/// Whether the groups are a copy received over a wire that can't resolve relative or
	/// time anchors as the publisher would, so only what [`Range::is_wire`] covers reads.
	fn drops_anchors(&self) -> bool {
		false
	}

	/// Whether this hop resolves ranges against groups of its own, so it refuses an
	/// [`Anchor::Oldest`] end, which names no group. A hop whose copies resolve them leaves
	/// the refusal to them.
	fn resolves(&self) -> bool {
		true
	}

	/// Whether a [`Requests`](crate::track::Requests) handler serving the track's history
	/// may look up a time its groups can't resolve: a time start before every group held
	/// then waits on that lookup.
	fn looks_up(&self) -> bool {
		false
	}

	/// Where `time` falls among the groups held.
	fn containing(&self, time: u64) -> Containing;

	/// The lowest group held starting at or after `from`.
	fn next_start(&self, from: u64) -> Option<u64>;

	/// The group `back` groups before `latest`, which counts as the first, among the groups
	/// held: `None` once fewer are held than that.
	fn back(&self, latest: u64, back: u64) -> Option<u64>;

	/// Whether nothing more can arrive here, so a time start nothing contains reaches back
	/// to the oldest group rather than wait.
	fn complete(&self) -> bool;
}

/// Where a time falls among the groups a hop holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Containing {
	/// The newest group starting at or before it.
	Group(u64),
	/// Every group held starts after it.
	Before,
	/// No group is held.
	Unknown,
}

/// A hop that holds no groups of its own, such as a reader following route copies that
/// resolve its ranges for it.
pub(in crate::model) struct Unheld;

impl Holdings for Unheld {
	fn resolves(&self) -> bool {
		false
	}

	fn containing(&self, _: u64) -> Containing {
		Containing::Unknown
	}

	fn next_start(&self, _: u64) -> Option<u64> {
		None
	}

	fn back(&self, latest: u64, back: u64) -> Option<u64> {
		(back == 0).then_some(latest)
	}

	fn complete(&self) -> bool {
		true
	}
}

/// A subscriber's [`Range`]s resolved to the group starts it reads.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, so only replacing the ranges resolves again.
	ranges: Vec<Range>,
	spans: Vec<Span>,
	/// The spans still looking up their time start (see [`Self::found`]), which read nothing
	/// meanwhile: kept apart so a probe finds them without visiting every span.
	looking: Vec<usize>,
	/// The spans whose time start waits on a group arriving, which read nothing meanwhile.
	waiting: Vec<usize>,
	/// The groups the spans cover, shared with what reads them.
	groups: Layered,
	/// The part of `groups` a miss may ask for: see [`Self::missable`].
	missable: Layered,
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
}

/// One non-empty range, resolved.
#[derive(Clone, Copy, Debug)]
struct Span {
	/// Which of the ranges it came from.
	index: usize,
	start: u64,
	/// The exclusive end, `None` past every group.
	end: Option<u64>,
	/// Whether the start names a group, rather than reaching back to the oldest one.
	floored: bool,
	/// A time start the groups held can't place, looked up from the track's history: the
	/// span reads nothing until the lookup answers.
	looking: Option<Looking>,
	/// A time start with no group held yet: the span reads nothing until one arrives.
	waiting: bool,
}

/// A time start being looked up: its time, and the exclusive end of the run below every
/// group held, which holds the group containing it if the track's history does.
#[derive(Clone, Copy, Debug)]
struct Looking {
	time: u64,
	below: u64,
}

impl Window {
	/// Resolve `ranges`, counting relative anchors back from `latest`.
	///
	/// Refuses an [`Anchor::Oldest`] end with [`Error::InvalidRange`] where the hop
	/// [resolves](Holdings::resolves) ranges itself, and an anchor a received copy's wire
	/// drops with [`Error::Unsupported`].
	pub fn new(ranges: &[Range], latest: Option<u64>, holdings: &impl Holdings) -> Result<Self> {
		let mut window = Self {
			ranges: ranges.to_vec(),
			spans: Vec::new(),
			looking: Vec::new(),
			waiting: Vec::new(),
			groups: Layered::default(),
			missable: Layered::default(),
			pending: false,
			settled: false,
			covered: None,
			named: None,
			all_named: true,
		};
		window.place(latest, holdings)?;
		Ok(window)
	}

	/// Whether nothing is left to resolve as groups arrive.
	pub fn is_settled(&self) -> bool {
		self.settled
	}

	/// Whether a relative anchor still waits on a first group.
	pub fn is_pending(&self) -> bool {
		self.pending
	}

	/// Resolve what the groups now held settle: relative anchors once `latest` exists, and
	/// time starts once a group arrives. Returns whether the groups in range moved.
	pub fn settle(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> bool {
		if self.pending {
			if latest.is_none() {
				return false;
			}
			// Checked when the ranges were first resolved, against the same track.
			let _ = self.place(latest, holdings);
			return !self.pending;
		}
		let moved = !self.waiting.is_empty() && self.start_waiting(holdings);
		if moved {
			self.rebuild(holdings);
		}
		moved
	}

	/// Start the spans whose time start waits on a group, once one arrived, or once nothing
	/// can arrive any more. Returns whether any did.
	fn start_waiting(&mut self, holdings: &impl Holdings) -> bool {
		let mut started = false;
		for position in std::mem::take(&mut self.waiting) {
			let Anchor::Time(time) = self.ranges[self.spans[position].index].start else {
				unreachable!("only a time start waits on a group");
			};
			let (start, floored) = match holdings.containing(time) {
				Containing::Group(group) => (group, true),
				// The group holding it is older than any held, or nothing can arrive.
				Containing::Before => (0, false),
				Containing::Unknown if holdings.complete() => (0, false),
				Containing::Unknown => {
					self.waiting.push(position);
					continue;
				}
			};
			started = true;
			self.start_span(position, time, start, floored, holdings);
		}
		if started {
			self.name();
		}
		started
	}

	/// Start the span at `position`, whose time start `time` waited, at `start`: looked up
	/// where the groups held can't place it, as when first resolved, and otherwise reading
	/// its groups.
	fn start_span(&mut self, position: usize, time: u64, start: u64, floored: bool, holdings: &impl Holdings) {
		let span = &mut self.spans[position];
		(span.start, span.floored, span.waiting) = (start, floored, false);
		if let Some(below) = unresolved(start, floored, holdings) {
			span.looking = Some(Looking { time, below });
			self.looking.push(position);
		}
	}

	/// Whether group `key` is in range. Everything is while a relative anchor waits.
	pub fn contains(&self, key: u64) -> bool {
		self.pending || self.groups.contains(key)
	}

	/// The lowest group in range, `None` while an anchor waits. Nothing in range floors
	/// nowhere: group 0.
	pub fn floor(&self) -> Option<u64> {
		match self.pending || !self.waiting.is_empty() {
			true => None,
			false => Some(self.groups.lowest().unwrap_or(0)),
		}
	}

	/// The lowest group the ranges name, `None` while one reaches back to the oldest group
	/// or an anchor waits.
	pub fn start(&self) -> Option<u64> {
		match self.pending || !self.all_named {
			true => None,
			false => self.named,
		}
	}

	/// The lowest group a range names, `None` when none does or an anchor waits.
	/// Unlike [`Self::start`], a range reaching back to the oldest group names nothing.
	pub fn named(&self) -> Option<u64> {
		match self.pending || !self.waiting.is_empty() {
			true => None,
			false => self.named,
		}
	}

	/// Each range resolved, in the order given: `None` for one whose start still waits, or
	/// still looking up its start. One empty as given ends at or below its start.
	pub fn resolved(&self) -> Vec<Option<Resolved>> {
		let mut resolved: Vec<_> = self
			.ranges
			.iter()
			.map(|range| match (range.start, range.end) {
				(Anchor::Time(start), Some(Anchor::Time(_))) if range.is_empty() => Some(Resolved {
					start,
					end: Some(start),
					floored: true,
				}),
				_ => None,
			})
			.collect();
		for span in self.spans.iter().filter(|span| span.looking.is_none() && !span.waiting) {
			resolved[span.index] = Some(Resolved {
				start: span.start,
				end: span.end,
				floored: span.floored,
			});
		}
		resolved
	}

	/// Where each range starts once delivery starts at `from`, in the order given: `None`
	/// for one with no group there, or whose start still waits.
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

	/// The exclusive end past every group in range, `None` when one stays open or an anchor
	/// waits.
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
	/// The caller only grows `accounted` between checks, so each resumes at the first group
	/// the last found missing unless `from` fell below its floor: one made on every wake
	/// visits each accounted entry once over the subscription, not once per wake, even
	/// within one range. The last stop never passes its cap, so a wider one is checked past
	/// it anyway.
	pub fn covered_by(&mut self, from: u64, cap: Option<u64>, accounted: &impl Accounted) -> bool {
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
			match accounted.first_gap(&(*run.start()..run_end)) {
				None => {
					through = run_end;
					true
				}
				Some(gap) => {
					through = gap;
					false
				}
			}
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

	/// The time starts still looked up, each with the exclusive end of the run below every
	/// group held, which holds the group containing it if the track's history does.
	pub fn lookups(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
		self.looking
			.iter()
			.filter_map(|span| self.spans[*span].looking.map(|looking| (looking.time, looking.below)))
	}

	/// The lookup of time start `time` answered with the group containing it, or the oldest
	/// group of the history when `time` is older than all of it: its spans start there, once
	/// that group is held. `None`, or an answer not held, means the history couldn't tell, so
	/// they start where the groups held say. Returns whether the groups in range moved.
	pub fn found(&mut self, time: u64, group: Option<u64>, holdings: &impl Holdings) -> bool {
		let mut found = Vec::new();
		self.looking.retain(|span| match self.spans[*span].looking {
			Some(looking) if looking.time == time => {
				found.push(*span);
				false
			}
			_ => true,
		});
		if found.is_empty() {
			return false;
		}
		let held = group.filter(|group| holdings.next_start(*group) == Some(*group));
		for index in found {
			let span = &mut self.spans[index];
			if span.looking.take().is_none() {
				continue;
			}
			(span.start, span.floored) = match (held, holdings.containing(time)) {
				(Some(group), _) | (None, Containing::Group(group)) => (group, true),
				(None, _) => (0, false),
			};
		}
		self.name();
		self.rebuild(holdings);
		true
	}

	fn place(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> Result<()> {
		if holdings.resolves() && self.ranges.iter().any(|range| range.end == Some(Anchor::Oldest)) {
			return Err(Error::InvalidRange);
		}
		if holdings.drops_anchors() && !self.ranges.iter().all(Range::is_wire) {
			return Err(Error::Unsupported);
		}
		self.spans.clear();
		self.looking.clear();
		self.waiting.clear();
		self.pending = false;
		for (index, range) in self.ranges.iter().enumerate().filter(|(_, range)| !range.is_empty()) {
			let Some(end) = end(range.end, latest, holdings) else {
				self.pending = true;
				continue;
			};
			let (start, floored) = match start(range.start, latest, holdings) {
				Start::At(start, floored) => (start, floored),
				Start::Pending => {
					self.pending = true;
					continue;
				}
				// Reads nothing until a group arrives; see [`Self::start_waiting`].
				Start::Waiting => {
					self.waiting.push(self.spans.len());
					self.spans.push(Span {
						index,
						start: 0,
						end,
						floored: false,
						looking: None,
						waiting: true,
					});
					continue;
				}
			};
			// A time start the groups held can't place reads nothing until its lookup answers.
			let looking = match range.start {
				Anchor::Time(time) => unresolved(start, floored, holdings).map(|below| Looking { time, below }),
				_ => None,
			};
			if looking.is_some() {
				self.looking.push(self.spans.len());
			}
			self.spans.push(Span {
				index,
				start,
				end,
				floored,
				looking,
				waiting: false,
			});
		}
		self.name();
		self.rebuild(holdings);
		Ok(())
	}

	/// Keep [`Self::named`] and [`Self::start`] current with where the spans start.
	fn name(&mut self) {
		self.named = self
			.spans
			.iter()
			.filter(|span| span.floored)
			.map(|span| span.start)
			.min();
		self.all_named = self.spans.iter().all(|span| span.floored);
	}

	/// The groups in range: every span that knows where it starts, through its end.
	fn rebuild(&mut self, holdings: &impl Holdings) {
		self.covered = None;
		// A span still looking up or waiting on its start reads nothing until it knows which
		// group that is.
		self.settled = !self.pending && self.looking.is_empty() && self.waiting.is_empty();
		let (mut groups, mut missable) = (Vec::new(), Vec::new());
		for span in self.spans.iter().filter(|span| span.looking.is_none() && !span.waiting) {
			let Some(last) = span.end.map_or(Some(u64::MAX), |end| end.checked_sub(1)) else {
				continue;
			};
			if last < span.start {
				continue;
			}
			groups.push(span.start..=last);
			// A time start before every group held asks no miss below the oldest, since which of
			// them it reaches is unknown.
			let reach = match (self.ranges[span.index].start, span.floored) {
				(Anchor::Time(_), false) => holdings.next_start(0).unwrap_or(span.start),
				_ => span.start,
			};
			if reach <= last {
				missable.push(reach..=last);
			}
		}
		self.groups = Layered::new(Arc::new(Runs::new(groups)), Runs::default());
		self.missable = Layered::new(Arc::new(Runs::new(missable)), Runs::default());
	}
}

/// Where a range starts.
enum Start {
	/// At this group, and whether it names that group rather than reaching back to the
	/// oldest one.
	At(u64, bool),
	/// A time start with no group held yet, so it reads nothing.
	Waiting,
	/// Waiting on a latest group, reading everything meanwhile.
	Pending,
}

fn start(anchor: Anchor, latest: Option<u64>, holdings: &impl Holdings) -> Start {
	match anchor {
		// Nothing sits below group 0, so the oldest start needs no latest group.
		Anchor::Oldest => Start::At(0, false),
		Anchor::Relative(back) => match latest {
			None => Start::Pending,
			Some(latest) => match holdings.back(latest, back) {
				Some(group) => Start::At(group, true),
				// Fewer groups are held than it counts back, so reach back to the oldest.
				None => Start::At(0, false),
			},
		},
		Anchor::Time(time) => match holdings.containing(time) {
			Containing::Group(group) => Start::At(group, true),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Start::At(0, false),
			// Before any group, as for a relative anchor, which a hop that holds none (such as
			// a reader following route copies) leaves to the copies to resolve.
			Containing::Unknown if latest.is_none() => Start::Pending,
			// Nothing can arrive any more, so nothing held reaches past it.
			Containing::Unknown if holdings.complete() => Start::At(0, false),
			// Nothing is held yet, so wait for a group rather than guess from the latest: a copy
			// can see the live edge before the groups its publisher resolved the time to.
			Containing::Unknown => Start::Waiting,
		},
	}
}

/// The exclusive end of the run a lookup of a time start resolved to `start` searches, when
/// the groups held can't tell which group contains it and a handler may: it is before every
/// group held, or in a gap after `start`, the newest held at or before it. Even a gap the
/// live feed accounted for may sit in the history once evicted.
fn unresolved(start: u64, floored: bool, holdings: &impl Holdings) -> Option<u64> {
	if !holdings.looks_up() {
		return None;
	}
	if !floored {
		return holdings.next_start(0);
	}
	let next = start.checked_add(1)?;
	let after = holdings.next_start(next)?;
	(after > next).then_some(after)
}

/// Where a range stops, exclusive, or `None` while it waits on a latest group. `Some(None)`
/// is past every group.
fn end(anchor: Option<Anchor>, latest: Option<u64>, holdings: &impl Holdings) -> Option<Option<u64>> {
	Some(match anchor {
		None => None,
		// Every end includes its group.
		Some(Anchor::Time(time)) => time.checked_add(1),
		Some(Anchor::Relative(back)) => match holdings.back(latest?, back) {
			Some(group) => group.checked_add(1),
			// It counts back past every group held, so it ends before them all.
			None => Some(0),
		},
		// Refused where groups are held; a hop that holds none lets its copies refuse it, and
		// delivers nothing meanwhile.
		Some(Anchor::Oldest) => Some(0),
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

	/// A hop's groups by key, the runs that can no longer arrive, and how many held
	/// groups and starts a walk asked about.
	///
	/// Reports changes as a track does: a group arriving and a run given up, never a group
	/// starting or ending empty, which its ledger never sees.
	#[derive(Default)]
	struct Groups {
		held: std::collections::BTreeMap<u64, Held>,
		gone: Vec<ops::Range<u64>>,
		complete: bool,
		looks_up: bool,
		asked: Cell<usize>,
		/// How many fetched copies gave a group held past its frame 0 a start, which a
		/// track counts rather than logs.
		fetched: Cell<u64>,
		/// Every change so far, oldest first, and what was last logged: the held keys
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

		fn started(key: u64, millis: u64) -> (u64, Held) {
			(key, Held::Started(at(millis)))
		}
	}

	impl Holdings for Groups {
		fn timed(&self) -> bool {
			true
		}

		fn looks_up(&self) -> bool {
			self.looks_up
		}

		fn containing(&self, pts: Timestamp) -> Containing {
			let mut stamped = false;
			for (&key, held) in self.held.iter().rev() {
				let Held::Started(start) = held else { continue };
				stamped = true;
				if !later(*start, pts) {
					return Containing::Group(key);
				}
			}
			match stamped {
				true => Containing::Before,
				false => Containing::Unknown,
			}
		}

		fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
			self.asked.set(self.asked.get() + 1);
			self.held.range(from..).find_map(|(&key, held)| match held {
				Held::Started(start) => Some((key, *start)),
				_ => None,
			})
		}

		fn next_held(&self, from: u64) -> Option<(u64, Held)> {
			self.asked.set(self.asked.get() + 1);
			self.held
				.range(from..)
				.next()
				.map(|(&key, held)| (key, *held))
		}

		fn gone(&self, key: u64) -> Option<u64> {
			self.gone
				.iter()
				.find(|gone| gone.contains(&key))
				.map(|gone| gone.end)
		}

		fn complete(&self) -> bool {
			self.complete
		}

		fn changes(&self, seen: &mut Seen) -> Option<Vec<ops::Range<u64>>> {
			let mut log = self.log.borrow_mut();
			let (log, held, gone) = &mut *log;
			let arrived = self.held.keys().filter(|key| !held.contains(key));
			log.extend(arrived.map(|key| *key..key.saturating_add(1)));
			log.extend(self.gone[*gone..].iter().cloned());
			*held = self.held.keys().copied().collect();
			*gone = self.gone.len();
			let from = usize::try_from(seen.runs[0]).unwrap();
			let changed = log[from..].to_vec();
			let global = [
				u64::from(self.complete),
				self.fetched.get(),
				u64::from(self.looks_up),
				0,
			];
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
		(0..upto).filter(|key| window.contains(*key)).collect()
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
				Some(Resolved {
					start: 2,
					end: Some(4),
					floored: true
				}),
				Some(Resolved {
					start: 7,
					end: None,
					floored: true
				}),
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

		// The oldest start needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None, &Untimed).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));

		// Any count back, however far, waits on a latest group and names the group it clamps
		// to, as a peer's largest counts must agree.
		for back in [u64::MAX, u64::MAX - 1] {
			let open = Range {
				start: Anchor::Relative(back),
				end: None,
			};
			assert!(!open.is_wire());
			assert!(Window::new(&[open], None, &Untimed).unwrap().is_pending());
			assert_eq!(Window::new(&[open], Some(7), &Untimed).unwrap().start(), Some(0));
		}
	}

	/// No hop can place the oldest group as an end, so it is refused.
	#[test]
	fn an_oldest_end_is_refused() {
		let ranges = [range(Anchor::Absolute(0), Anchor::Oldest)];
		assert!(matches!(
			Window::new(&ranges, Some(3), &Untimed),
			Err(Error::InvalidRange)
		));
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
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: 3,
				end: Some(5),
				floored: true
			})]
		);

		// Group 6 may yet start at 5000 too: read on speculation, the end left open.
		let window = Window::new(&[range(time(3500), time(5000))], Some(6), &groups).unwrap();
		assert!(!window.is_settled());
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6]);
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: 3,
				end: None,
				floored: true
			})]
		);
		assert_eq!(window.end(), None, "the cap is not the end");
		assert_eq!(window.speculative().collect::<Vec<_>>(), vec![6]);

		// Before every start held: reach back to the oldest.
		let window = Window::new(&[range(time(1000), time(2500))], Some(6), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![0, 1, 2]);

		// Nothing started yet: a time start waits on a first group, then on one starting.
		let mut window = Window::new(&[range(time(1000), None)], None, &Groups::default()).unwrap();
		assert!(window.is_pending());
		let mut held = Groups::new([(4, Held::Pending)]);
		assert!(window.settle(Some(4), &held));
		assert!(!window.contains(4), "4 has not started");
		held.held.insert(4, Held::Started(at(1000)));
		assert!(window.settle(Some(4), &held));
		assert_eq!(window.floor(), Some(4));
	}

	/// An end settles only once every key between the newest group starting inside it
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

	/// A peer that leaves an early key missing and keeps sending later groups costs a
	/// time end each group once, not a rescan of everything since the gap on every settle.
	#[test]
	fn a_time_end_walks_each_group_once_behind_a_missing_key() {
		let mut groups = Groups::new([Groups::started(0, 0)]);
		let mut window = Window::new(&[range(time(0), time(1_000_000))], Some(0), &groups).unwrap();
		let count = 2_000;
		for key in 2..count {
			groups.held.insert(key, Held::Started(at(key * 10)));
			window.settle(Some(key), &groups);
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
		for key in 1..count {
			groups.held.insert(key, Held::Started(at(key)));
			window.settle(Some(key), &groups);
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
				end: Some(count),
				floored: true,
			})));
		assert!(resolved[split..].iter().all(|r| *r
			== Some(Resolved {
				start: 0,
				end: None,
				floored: true
			})));

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
		const KEYS: u64 = 16;
		for round in 0..400 {
			// Each group starts in its own ten milliseconds, so starts ascend with the key.
			let starts: Vec<u64> = (0..KEYS).map(|key| key * 10 + next(10)).collect();
			let pts = |end: bool| match next(3) {
				0 => micros(next(KEYS * 10_000)),
				// Exactly a group's start, in either scale.
				1 if next(2) == 0 => time(starts[next(KEYS) as usize]),
				1 => micros(starts[next(KEYS) as usize] * 1000),
				_ => time(next(KEYS * 10) + u64::from(end) * 5),
			};
			let ranges: Vec<Range> = (0..1 + next(12))
				.map(|_| {
					let start = match next(3) {
						0 => Anchor::Absolute(next(KEYS)),
						1 => Anchor::Relative(next(4)),
						_ => pts(false),
					};
					let end = match next(6) {
						0 => None,
						1 => Some(Anchor::Absolute(next(KEYS + 2))),
						_ => Some(pts(true)),
					};
					range(start, end)
				})
				.collect();

			let mut groups = Groups::default();
			for _ in 0..next(6) {
				let key = next(KEYS);
				groups
					.held
					.insert(key, Held::Started(at(starts[key as usize])));
			}
			let latest = Some(next(KEYS));
			let mut window = Window::new(&ranges, latest, &groups).unwrap();
			let mut alone: Vec<Window> = ranges
				.iter()
				.map(|range| Window::new(std::slice::from_ref(range), latest, &groups).unwrap())
				.collect();

			for step in 0..40 {
				let key = next(KEYS);
				let started = Held::Started(at(starts[key as usize]));
				match next(6) {
					0 | 1 => {
						groups.held.entry(key).or_insert(Held::Pending);
					}
					2 | 3 => {
						if groups.held.get(&key).is_none_or(|held| *held == Held::Pending) {
							groups.held.insert(key, started);
						}
					}
					4 => match groups.held.get(&key) {
						None | Some(Held::Pending) => {
							groups.held.insert(key, Held::Never);
						}
						// A fetched head gives a group held past its frame 0 a start.
						Some(Held::Never) if next(2) == 0 => {
							groups.held.insert(key, started);
							groups.fetched.set(groups.fetched.get() + 1);
						}
						_ => {}
					},
					_ => {
						let end = key + 1 + next(3);
						if groups.held.range(key..end).next().is_none() {
							groups.gone.push(key..end);
						}
					}
				}
				groups.complete |= step == 39 && round % 4 == 0;

				let before: Vec<u64> = covered(&window, KEYS + 4);
				let moved = window.settle(latest, &groups);
				for window in &mut alone {
					window.settle(latest, &groups);
				}
				let context = || format!("round {round} step {step}: {ranges:?} over {:?}", groups.held);

				let union: Vec<u64> = (0..KEYS + 4)
					.filter(|key| alone.iter().any(|window| window.contains(*key)))
					.collect();
				let now = covered(&window, KEYS + 4);
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
			if next.is_some_and(|(key, _)| key == self.racing) && self.asked.replace(true) {
				return Some((self.racing, Held::Started(at(6000))));
			}
			next
		}

		fn gone(&self, key: u64) -> Option<u64> {
			self.groups.gone(key)
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
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: 0,
				end: Some(1),
				floored: true
			})]
		);
	}

	#[test]
	fn a_missing_key_past_a_time_end_is_the_one_read_on_speculation() {
		// 1 and 2 are missing, and 3 never starts: the walk may not pass the gap, or every
		// key in it that arrives later is read past the end.
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
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: 0,
				end: Some(1),
				floored: true
			})]
		);
	}

	#[test]
	fn a_time_end_stops_walking_at_the_last_key() {
		let groups = Groups::new([Groups::started(u64::MAX, 3000)]);
		let window = Window::new(&[range(Anchor::Absolute(u64::MAX), time(5000))], None, &groups).unwrap();
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: u64::MAX,
				end: None,
				floored: true,
			})]
		);
		assert!(window.contains(u64::MAX));
		assert_eq!(window.watch_starts(), None, "nothing above the last key can start");
	}

	/// A time start no held group has started for yet reads nothing of its range, rather
	/// than guess the latest group, and resolves once one starts. A track that can start
	/// nothing more reaches back to the oldest group.
	#[test]
	fn a_time_start_waits_for_a_group_to_start() {
		let pending = (2..10).map(|key| (key, Held::Pending));
		let mut groups = Groups::new(pending.clone());
		let ranges = [
			range(time(2500), None),
			range(Anchor::Absolute(20), Anchor::Absolute(21)),
		];
		let mut window = Window::new(&ranges, Some(9), &groups).unwrap();
		assert!(!window.is_pending() && !window.is_settled());
		assert_eq!(covered(&window, 22), vec![20], "only the other range reads");
		assert_eq!((window.floor(), window.start(), window.end()), (None, None, None));
		assert_eq!(
			window.resolved(),
			vec![
				None,
				Some(Resolved {
					start: 20,
					end: Some(21),
					floored: true,
				})
			]
		);
		assert_eq!(window.watch_starts(), Some(0), "any group starting may resolve it");
		assert!(!window.settle(Some(9), &groups), "nothing started");

		groups.held.insert(3, Held::Started(at(3000)));
		assert!(window.settle(Some(9), &groups));
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 0,
				end: None,
				floored: false
			}),
			"3 starts past it"
		);
		groups.held.insert(2, Held::Started(at(2000)));
		let window = Window::new(&ranges, Some(9), &groups).unwrap();
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 2,
				end: None,
				floored: true
			})
		);
		assert!(window.is_settled());

		let groups = Groups {
			complete: true,
			..Groups::new(pending)
		};
		let window = Window::new(&ranges, Some(9), &groups).unwrap();
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 0,
				end: None,
				floored: false
			})
		);
	}

	/// A relative end counts back from the latest group it was asked against, however many
	/// arrive while its time start waits.
	#[test]
	fn a_waiting_time_start_keeps_its_relative_end() {
		for latest in [None, Some(9)] {
			let mut groups = Groups::default();
			let ranges = [range(time(2500), Anchor::Relative(0))];
			let mut window = Window::new(&ranges, latest, &groups).unwrap();
			assert_eq!(window.is_pending(), latest.is_none());
			groups.held.extend((2..10).map(|key| (key, Held::Pending)));
			window.settle(Some(9), &groups);
			assert!(!window.is_pending());
			groups.held.insert(10, Held::Pending);
			assert!(!window.settle(Some(10), &groups), "nothing started");

			groups.held.insert(2, Held::Started(at(2000)));
			assert!(window.settle(Some(10), &groups));
			assert_eq!(
				window.resolved()[0],
				Some(Resolved {
					start: 2,
					end: Some(10),
					floored: true,
				})
			);
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

	/// A check that stops partway into a range resumes there, and still sees a gap filled
	/// behind where it stopped.
	#[test]
	fn coverage_resumes_within_a_range() {
		let mut window = Window::new(&[Range::groups(0..8)], None, &Untimed).unwrap();
		let mut accounted = rangemap::RangeSet::from_iter(std::iter::once(0..3));
		assert!(!window.covered_by(0, None, &accounted));
		assert_eq!(window.covered, Some((0, 3)), "stops at the first missing group");
		accounted.insert(3..5);
		accounted.insert(6..8);
		assert!(!window.covered_by(0, None, &accounted));
		assert_eq!(window.covered, Some((0, 5)));
		accounted.insert(5..6);
		assert!(window.covered_by(0, None, &accounted));
	}

	/// A map keyed by the groups accounted for counts as a set of them, whatever its values.
	#[test]
	fn coverage_reads_a_map_of_the_groups_accounted_for() {
		let mut window = Window::new(&[Range::groups(0..2), Range::groups(4..6)], None, &Untimed).unwrap();
		let mut accounted = rangemap::RangeMap::new();
		accounted.insert(0..2, 'a');
		accounted.insert(4..5, 'b');
		assert!(!window.covered_by(0, None, &accounted), "group 5 is owed");
		accounted.insert(5..6, 'a');
		assert!(window.covered_by(0, None, &accounted));
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

	/// With a handler serving the history, a time start the groups held can't place is looked
	/// up and reads nothing meanwhile: before every start, or in a gap after the newest group
	/// starting at or before it, even one the live feed accounted for, since an evicted group
	/// still sits in the history. The answer places it; none leaves what the groups held say.
	#[test]
	fn a_time_start_the_groups_cant_place_is_looked_up() {
		let mut groups = Groups::new([Groups::started(4, 4000), Groups::started(9, 9000)]);
		groups.gone.push(5..9);
		groups.looks_up = true;
		let mut window = Window::new(&[range(time(8500), None)], Some(9), &groups).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(at(8500), 9)]);
		assert!(!window.is_settled() && !window.contains(9) && window.resolved() == [None]);
		groups.held.insert(8, Held::Started(at(8000)));
		assert!(window.found(at(8500), Some(8), &groups));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 8,
				end: None,
				floored: true
			})]
		);
		assert!(window.is_settled());

		let window = Window::new(&[range(time(8500), None)], Some(9), &groups).unwrap();
		assert_eq!(window.lookups().count(), 0, "group 8 is held, with 9 after it");

		let mut window = Window::new(&[range(time(500), None)], Some(9), &groups).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(at(500), 4)]);
		assert!(window.found(at(500), None, &groups));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 0,
				end: None,
				floored: false
			})]
		);
		assert!(window.contains(4) && !window.missable(0).any(|run| *run.start() < 4));
	}

	/// A time end on a start the history couldn't place walks from the oldest group held,
	/// since nothing asks for those below it: none holds the end open.
	#[test]
	fn an_unanswered_lookup_settles_a_time_end() {
		let mut groups = Groups::new([Groups::started(4, 4000), Groups::started(9, 9000)]);
		groups.looks_up = true;
		let mut window = Window::new(&[range(time(500), Some(time(3000)))], Some(9), &groups).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(at(500), 4)]);
		assert!(window.found(at(500), None, &groups));
		assert!(window.is_settled());
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 0,
				end: Some(4),
				floored: false
			})]
		);
	}

	/// Without a handler serving the history, the groups held decide alone.
	#[test]
	fn a_time_start_is_not_looked_up_without_a_history() {
		let groups = Groups::new([Groups::started(4, 4000), Groups::started(9, 9000)]);
		let window = Window::new(&[range(time(8500), None), range(time(500), None)], Some(9), &groups).unwrap();
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(
			window.resolved(),
			[
				Some(Resolved {
					start: 4,
					end: None,
					floored: true
				}),
				Some(Resolved {
					start: 0,
					end: None,
					floored: false
				}),
			]
		);
	}
}

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

	/// Whether the producer declared every key in `keys` skipped, so no group starts there
	/// even in the history a handler serves.
	fn never_produced(&self, keys: ops::Range<u64>) -> bool {
		keys.is_empty()
	}
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
	/// The spans whose time start waits on a group arriving, which read only the groups from
	/// that time meanwhile.
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
	/// A time start the groups held can't place yet: the span reads only the groups from
	/// that time until it can.
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

	/// Whether a time start still waits on the groups held to place it.
	pub fn is_waiting(&self) -> bool {
		!self.waiting.is_empty()
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
		if let Some(below) = unresolved(time, start, floored, holdings) {
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
	/// them but those starting before a time start that fell before every group held, or
	/// below the oldest group held when a relative start counted back past them all.
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
				// Reads only the groups from its time until the groups held place it; see
				// [`Self::start_waiting`].
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
				Anchor::Time(time) => unresolved(time, start, floored, holdings).map(|below| Looking { time, below }),
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

	/// The groups in range: every span that knows where it starts, through its end, and
	/// every span waiting on a time start from that time.
	fn rebuild(&mut self, holdings: &impl Holdings) {
		self.covered = None;
		// A span still looking up its start reads nothing until it knows which group that is.
		self.settled = !self.pending && self.looking.is_empty() && self.waiting.is_empty();
		let (mut groups, mut missable) = (Vec::new(), Vec::new());
		for span in self.spans.iter().filter(|span| span.looking.is_none()) {
			let Some(last) = span.end.map_or(Some(u64::MAX), |end| end.checked_sub(1)) else {
				continue;
			};
			// A span waiting on its time start reads from that time meanwhile, and may ask a miss
			// there, as one reaching back past every group held does: whichever group contains
			// it starts at or before it, so every group from there on is in range.
			if span.waiting {
				if let Anchor::Time(time) = self.ranges[span.index].start
					&& time <= last
				{
					groups.push(time..=last);
					missable.push(time..=last);
				}
				continue;
			}
			if last < span.start {
				continue;
			}
			groups.push(span.start..=last);
			// A time start before every group held reaches back to the group containing it,
			// which is unknown here, so it asks no miss below its time: every key from there
			// on is in range, the key at the time itself being the containing group if it
			// exists. A relative one counting back past every group held asks none below the
			// oldest, since which of them it reaches is unknown.
			let reach = match (self.ranges[span.index].start, span.floored) {
				(Anchor::Time(time), false) => time,
				(Anchor::Relative(_), false) => holdings.next_start(0).unwrap_or(span.start),
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
	/// A time start the groups held can't place yet, so it reads only the groups from that
	/// time.
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
		// No group starts before 0, so time 0 names group 0 whatever is held.
		Anchor::Time(0) => Start::At(0, true),
		Anchor::Time(time) => match holdings.containing(time) {
			Containing::Group(group) => Start::At(group, true),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Start::At(0, false),
			// Before any group, as for a relative anchor, which a hop that holds none (such as
			// a reader following route copies) leaves to the copies to resolve.
			Containing::Unknown if latest.is_none() && !holdings.resolves() => Start::Pending,
			// Nothing can arrive any more, so nothing held reaches past it.
			Containing::Unknown if holdings.complete() => Start::At(0, false),
			// The groups held can't place it, so wait for one that can rather than guess from the
			// latest: a copy can see the live edge before the groups its publisher resolved the
			// time to.
			Containing::Unknown => Start::Waiting,
		},
	}
}

/// The exclusive end of the run a lookup of time start `time`, resolved to `start`, searches,
/// when the groups held can't tell which group contains it and a handler may: it is before
/// every group held, or a group the cache evicted may start after `start`, the newest held
/// at or before it, and still at or before `time`. Keys the producer skipped hold none.
fn unresolved(time: u64, start: u64, floored: bool, holdings: &impl Holdings) -> Option<u64> {
	if !holdings.looks_up() {
		return None;
	}
	if !floored {
		return holdings.next_start(0);
	}
	let next = start.checked_add(1)?;
	let after = holdings.next_start(next)?;
	let gap = next..after.min(time.saturating_add(1));
	(!holdings.never_produced(gap)).then_some(after)
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

	use std::collections::BTreeSet;

	/// The group starts a hop holds, the key runs its producer skipped, and whether a
	/// handler serves its history.
	#[derive(Default)]
	struct Keys {
		held: BTreeSet<u64>,
		skipped: Vec<ops::Range<u64>>,
		complete: bool,
		looks_up: bool,
	}

	impl Keys {
		fn new(held: impl IntoIterator<Item = u64>) -> Self {
			Self {
				held: held.into_iter().collect(),
				..Self::default()
			}
		}
	}

	impl Holdings for Keys {
		fn looks_up(&self) -> bool {
			self.looks_up
		}

		fn containing(&self, time: u64) -> Containing {
			match self.held.range(..=time).next_back() {
				Some(key) => Containing::Group(*key),
				None if self.held.is_empty() => Containing::Unknown,
				None => Containing::Before,
			}
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			self.held.range(from..).next().copied()
		}

		fn back(&self, latest: u64, back: u64) -> Option<u64> {
			let Some(back) = back.checked_sub(1) else {
				return Some(latest);
			};
			self.held.range(..latest).rev().nth(back as usize).copied()
		}

		fn complete(&self) -> bool {
			self.complete
		}

		fn never_produced(&self, keys: ops::Range<u64>) -> bool {
			keys.is_empty()
				|| self
					.skipped
					.iter()
					.any(|run| run.start <= keys.start && keys.end <= run.end)
		}
	}

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn time(start: u64) -> Anchor {
		Anchor::Time(start)
	}

	fn covered(window: &Window, keys: impl IntoIterator<Item = u64>) -> Vec<u64> {
		keys.into_iter().filter(|key| window.contains(*key)).collect()
	}

	/// Disjoint ranges filter to exactly their groups; the gap between them is out.
	#[test]
	fn ranges_cover_only_their_groups() {
		let keys = Keys::new(0..10);
		let ranges = [range(time(2), time(3)), range(time(7), None)];
		let window = Window::new(&ranges, Some(9), &keys).unwrap();
		assert_eq!(covered(&window, 0..10), vec![2, 3, 7, 8, 9]);
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
		let window = Window::new(&[], None, &keys).unwrap();
		assert_eq!(covered(&window, 0..4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// A time start names the group containing it, the newest starting at or before it, and
	/// a time end includes every group starting at or before it: sparse starts need no walk.
	#[test]
	fn time_anchors_name_groups_by_their_starts() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(&[range(time(50), time(70))], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);
		assert!(window.is_settled());
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: 33,
				end: Some(71),
				floored: true
			})]
		);

		// A start exactly on a group names that group, and an end on one includes it.
		let window = Window::new(&[range(time(33), time(66))], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);

		// An end before its start is empty, and so is one ending before every group.
		let window = Window::new(&[range(time(66), time(33))], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		// Before every start held: reach back to the group containing it, naming no floor.
		let keys = Keys::new([10, 20]);
		let window = Window::new(&[range(time(5), None)], Some(20), &keys).unwrap();
		assert_eq!(covered(&window, [10, 20]), vec![10, 20]);
		assert_eq!((window.floor(), window.start(), window.named()), (Some(0), None, None));
		assert_eq!(window.missable(0).next(), Some(5..=u64::MAX), "nothing below its time");
	}

	/// Relative anchors count back through the groups held, however sparse their starts,
	/// and wait for a first group when there is none.
	#[test]
	fn relative_anchors_count_groups_back_from_the_latest() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(&[range(Anchor::Relative(2), Anchor::Relative(1))], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);
		assert_eq!(window.start(), Some(33));

		// Counting back past every group held reaches back to the oldest as a start, and ends
		// before them all as an end.
		let window = Window::new(&[range(Anchor::Relative(10), None)], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![0, 33, 66, 99]);
		assert_eq!(window.start(), None);
		let window = Window::new(&[range(Anchor::Oldest, Anchor::Relative(10))], Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		let mut window = Window::new(&[Range::live()], None, &Keys::default()).unwrap();
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Keys::default()));
		let keys = Keys::new([5]);
		assert!(window.settle(Some(5), &keys));
		assert_eq!(covered(&window, 0..8), vec![5, 6, 7]);
		assert!(window.is_settled());

		// The oldest start needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None, &keys).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));

		// Any count back, however far, waits on a latest group.
		for back in [u64::MAX, u64::MAX - 1] {
			let open = range(Anchor::Relative(back), None);
			assert!(!open.is_wire());
			assert!(Window::new(&[open], None, &keys).unwrap().is_pending());
			assert_eq!(Window::new(&[open], Some(5), &keys).unwrap().start(), None);
		}
	}

	/// No hop can place the oldest group as an end, so it is refused.
	#[test]
	fn an_oldest_end_is_refused() {
		let ranges = [range(time(0), Anchor::Oldest)];
		assert!(matches!(
			Window::new(&ranges, Some(3), &Keys::new(0..4)),
			Err(Error::InvalidRange)
		));
		// A hop holding no groups leaves the refusal to the copies that do.
		assert!(Window::new(&ranges, Some(3), &Unheld).is_ok());
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let keys = Keys::new(0..10);
		let start = |ranges: &[Range], latest| Window::new(ranges, latest, &keys).unwrap().start();
		assert_eq!(start(&[range(time(4), None), Range::live()], Some(9)), Some(4));
		assert_eq!(start(&[range(time(4), None), Range::live()], Some(2)), Some(2));
		assert_eq!(start(&[range(time(0), None)], None), Some(0));
		assert_eq!(start(&[range(time(4), None), Range::live()], None), None);
		assert_eq!(start(&[range(time(4), None), Range::oldest()], Some(9)), None);
	}

	/// A time start with no group held yet reads only the groups from its time, rather than
	/// guess the latest group, and resolves once a group arrives. A track that can receive
	/// nothing more reaches back to the oldest group.
	#[test]
	fn a_time_start_waits_for_a_group() {
		let mut keys = Keys::default();
		let ranges = [range(time(2500), None), range(Anchor::Relative(0), Anchor::Relative(0))];
		let mut window = Window::new(&ranges, Some(9), &keys).unwrap();
		assert!(!window.is_pending() && !window.is_settled());
		assert_eq!(covered(&window, 0..22), vec![9], "only the other range reads below it");
		assert_eq!(
			covered(&window, 2498..2502),
			vec![2500, 2501],
			"the groups from the time are in range whatever contains it"
		);
		assert_eq!((window.floor(), window.start(), window.end()), (None, None, None));
		assert_eq!(
			window.resolved(),
			vec![
				None,
				Some(Resolved {
					start: 9,
					end: Some(10),
					floored: true,
				})
			]
		);
		assert!(!window.settle(Some(9), &keys), "nothing arrived");

		keys.held.insert(3000);
		assert!(window.settle(Some(3000), &keys));
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 0,
				end: None,
				floored: false
			}),
			"3000 starts past it"
		);
		keys.held.insert(2000);
		let window = Window::new(&ranges, Some(3000), &keys).unwrap();
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 2000,
				end: None,
				floored: true
			})
		);
		assert!(window.is_settled());

		let keys = Keys {
			complete: true,
			..Keys::default()
		};
		let window = Window::new(&ranges, Some(9), &keys).unwrap();
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
			let mut keys = Keys::default();
			let ranges = [range(time(2500), Anchor::Relative(0))];
			let mut window = Window::new(&ranges, latest, &keys).unwrap();
			assert_eq!(window.is_pending(), latest.is_none());
			keys.held.extend([2000, 3000]);
			assert!(window.settle(Some(3000), &keys));
			assert!(!window.is_pending());
			keys.held.insert(4000);
			assert!(!window.settle(Some(4000), &keys), "resolved already");
			assert_eq!(
				window.resolved()[0].map(|resolved| (resolved.start, resolved.end)),
				Some((2000, Some(latest.map_or(3001, |latest| latest + 1))))
			);
		}
	}

	/// A coverage check resumes where the last stopped, but not below its floor or past its
	/// cap: a lower floor or a wider cap reaches groups no check has seen.
	#[test]
	fn coverage_resumes_within_what_was_checked() {
		let keys = Keys::new(0..6);
		let ranges = [range(time(0), time(1)), range(time(4), time(5))];
		let mut window = Window::new(&ranges, None, &keys).unwrap();
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
		let mut window = Window::new(&[range(time(0), time(7))], None, &Keys::new(0..8)).unwrap();
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
		let ranges = [range(time(0), time(1)), range(time(4), time(5))];
		let mut window = Window::new(&ranges, None, &Keys::new(0..6)).unwrap();
		let mut accounted = rangemap::RangeMap::new();
		accounted.insert(0..2, 'a');
		accounted.insert(4..5, 'b');
		assert!(!window.covered_by(0, None, &accounted), "group 5 is owed");
		accounted.insert(5..6, 'a');
		assert!(window.covered_by(0, None, &accounted));
	}

	/// With a handler serving the history, a time start the groups held can't place is looked
	/// up and reads nothing meanwhile: before every start, or with a group the cache evicted
	/// possibly starting between the newest held at or before it and the time. The answer
	/// places it; none leaves what the groups held say.
	#[test]
	fn a_time_start_the_groups_cant_place_is_looked_up() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(8500), None)], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(8500, 9000)]);
		assert!(!window.is_settled() && !window.contains(9000) && window.resolved() == [None]);
		keys.held.insert(8000);
		assert!(window.found(8500, Some(8000), &keys));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 8000,
				end: None,
				floored: true
			})]
		);
		assert!(window.is_settled());

		let window = Window::new(&[range(time(8500), None)], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().count(), 1, "something may start between 8000 and 8500");
		let window = Window::new(&[range(time(8000), None)], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().count(), 0, "8000 starts exactly there");

		let mut window = Window::new(&[range(time(500), None)], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(500, 4000)]);
		assert!(window.found(500, None, &keys));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 0,
				end: None,
				floored: false
			})]
		);
		assert!(window.contains(4000) && !window.missable(0).any(|run| *run.start() < 500));
	}

	/// Keys the producer skipped hold no group, even in the history, so a start in a skipped
	/// span between two held groups is placed without a lookup.
	#[test]
	fn a_skipped_gap_needs_no_lookup() {
		let mut keys = Keys::new([0, 33, 66]);
		keys.looks_up = true;
		keys.skipped = vec![1..33, 34..66];
		let window = Window::new(&[range(time(50), None)], Some(66), &keys).unwrap();
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(33));
	}

	/// A time end on a start the history couldn't place ends where it says, since time ends
	/// name their groups by start.
	#[test]
	fn an_unanswered_lookup_settles_with_its_time_end() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(500), time(3000))], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(500, 4000)]);
		assert!(window.found(500, None, &keys));
		assert!(window.is_settled());
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 0,
				end: Some(3001),
				floored: false
			})]
		);
	}

	/// Without a handler serving the history, the groups held decide alone.
	#[test]
	fn a_time_start_is_not_looked_up_without_a_history() {
		let keys = Keys::new([4000, 9000]);
		let window = Window::new(&[range(time(8500), None), range(time(500), None)], Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(
			window.resolved(),
			[
				Some(Resolved {
					start: 4000,
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

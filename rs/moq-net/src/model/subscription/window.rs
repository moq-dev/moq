//! A subscriber's own ranges, resolved to the group starts it reads.
//!
//! The aggregate a publisher sees is the union of every subscriber's ranges, so it says
//! what arrives, not what one subscriber asked for. A [`Window`] is the per-subscriber
//! side: resolved at the hop serving the subscriber, it filters that subscriber's reads.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops,
	time::Duration,
};

use super::{Anchor, Cover, Range};
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

	/// Runs of times that [`Self::containing`] can't place, so a window skips its time starts
	/// in them rather than try each again whenever the groups held change. `None` when it
	/// can't tell, so each is tried.
	fn unplaced(&self) -> Option<Unplaced> {
		None
	}

	/// The lowest group held starting at or after `from`.
	fn next_start(&self, from: u64) -> Option<u64>;

	/// The group `back` groups before `latest`, which counts as the first, among the groups
	/// held: `None` once fewer are held than that.
	fn back(&self, latest: u64, back: u64) -> Option<u64>;

	/// Where a range counting `back` groups before `latest`, up to the exclusive `end`, starts,
	/// which a relative start, but never an end, takes over [`Self::back`]: where the publisher
	/// reported starting it,
	/// since a delivery start may sit past the group counted, such as above groups too stale
	/// for the budget, or [`Containing::Unknown`] while a received copy can count it neither
	/// that way nor over its own groups yet. `None` counts it with [`Self::back`].
	fn counted_start(&self, latest: u64, back: u64, end: Option<u64>) -> Option<Containing> {
		let _ = (latest, back, end);
		None
	}

	/// Whether nothing more can arrive here, so a time start nothing contains reaches back
	/// to the oldest group rather than wait.
	fn complete(&self) -> bool;

	/// Where a live join holding `min` behind the live edge starts, `latest` being the latest
	/// group: a group held, the newest one starting at or before the newest frame held less
	/// `min`, or the oldest one held when none does, never a raw time a copy would ask its
	/// upstream to fill. `None` while nothing held can measure it, so the join waits for a
	/// group as a relative start does.
	fn reach_back(&self, latest: u64, min: Duration) -> Option<Anchor> {
		let _ = (latest, min);
		None
	}

	/// Whether the producer declared every key in `keys` skipped, so no group starts there
	/// even in the history a handler serves.
	fn never_produced(&self, keys: ops::Range<u64>) -> bool {
		keys.is_empty()
	}
}

/// Where a time, or a count back from the latest group, falls among the groups a hop holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Containing {
	/// The newest group starting at or before it, or the group counted back to.
	Group(u64),
	/// Every group held starts after it.
	Before,
	/// The groups held can't place it yet.
	Unknown,
}

/// The times a hop's [`Holdings::containing`] can't place, as up to two runs: every time in
/// them is [`Containing::Unknown`], except the few that `mark` places another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::model) struct Unplaced {
	pub runs: [ops::RangeInclusive<u64>; 2],
	/// Changes whenever a time inside the runs may have come to be placed by something other
	/// than a group arriving or a drop, so a window that saw another mark tries every start.
	pub mark: u64,
}

impl Unplaced {
	/// An empty run, for a hop that places every time on one side.
	pub const NONE: ops::RangeInclusive<u64> = ops::RangeInclusive::new(1, 0);
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

/// A subscriber's [`Range`]s resolved to the group starts it reads, each by the key its
/// caller gave it, so an edit resolves only the ranges it adds and leaves every other where
/// it resolved.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, by key, in the order given.
	ranges: BTreeMap<u64, Range>,
	/// The subscriber's Min Age, which reaches a live join back from the live edge.
	min: Option<Duration>,
	/// Each non-empty range placed, by key.
	spans: BTreeMap<u64, Span>,
	/// The ranges waiting on a first group, by key: a relative anchor, or a live join with
	/// nothing to reach back from. Everything is in range meanwhile.
	pending: BTreeSet<u64>,
	/// The spans still looking up their time start (see [`Self::found`]), which read nothing
	/// meanwhile: by the time looked up, then key, so a probe finds them without visiting
	/// every span.
	looking: BTreeSet<(u64, u64)>,
	/// The spans whose time start waits on a group arriving, which read only the groups from
	/// that time meanwhile: by that time, then key, so a change to the groups held tries only
	/// those its [`Holdings::unplaced`] leaves out, found without visiting the rest.
	waiting: BTreeSet<(u64, u64)>,
	/// The [`Unplaced::mark`] every waiting span was last tried under.
	waited: Option<u64>,
	/// The ranges of a live join whose reach back waits on the groups held to measure the
	/// live edge, which read nothing meanwhile: a group held is no reason to start at it.
	reaching: BTreeSet<u64>,
	/// The ranges whose relative anchor the groups held can't count back to yet (see
	/// [`Holdings::back`]), which read nothing meanwhile: a group held may sit below where
	/// the publisher counted to.
	counting: BTreeSet<u64>,
	/// The groups the spans cover, shared with what reads them.
	groups: Cover,
	/// The part of `groups` a miss may ask for: see [`Self::missable`].
	missable: Cover,
	/// The floor the last [`Self::covered_by`] checked from, and where it stopped finding the
	/// groups accounted for.
	covered: Option<(u64, u64)>,
	/// How many spans start at each group they name, so a probe reads the lowest without a
	/// walk: see [`Self::named`] and [`Self::start`].
	named: BTreeMap<u64, usize>,
	/// How many spans reach back to the oldest group instead of naming one.
	unnamed: usize,
}

/// One non-empty range, resolved.
#[derive(Clone, Debug)]
struct Span {
	/// Where it starts: its range's start, or where a live join's Min Age reached it back to.
	anchor: Anchor,
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
	/// What it counts in [`Window::groups`] and [`Window::missable`], so taking it out
	/// uncounts exactly that.
	groups: Option<ops::RangeInclusive<u64>>,
	missable: Option<ops::RangeInclusive<u64>>,
}

/// A time start being looked up: its time, and the exclusive end of the run below every
/// group held, which holds the group containing it if the track's history does.
#[derive(Clone, Copy, Debug)]
struct Looking {
	time: u64,
	below: u64,
}

impl Window {
	/// Resolve `ranges`, keyed by position, counting relative anchors back from `latest`, and
	/// reaching a live join back by `min` (see [`joins`]).
	///
	/// Refuses an [`Anchor::Oldest`] end with [`Error::InvalidRange`] where the hop
	/// [resolves](Holdings::resolves) ranges itself, and an anchor a received copy's wire
	/// drops with [`Error::Unsupported`].
	pub fn new(ranges: &[Range], min: Option<Duration>, latest: Option<u64>, holdings: &impl Holdings) -> Result<Self> {
		Self::keyed((0..).zip(ranges.iter().copied()), min, latest, holdings)
	}

	/// [`Self::new`] for ranges the caller keys itself, in key order.
	pub fn keyed(
		ranges: impl IntoIterator<Item = (u64, Range)>,
		min: Option<Duration>,
		latest: Option<u64>,
		holdings: &impl Holdings,
	) -> Result<Self> {
		let mut window = Self {
			ranges: ranges.into_iter().collect(),
			min,
			spans: BTreeMap::new(),
			pending: BTreeSet::new(),
			looking: BTreeSet::new(),
			waiting: BTreeSet::new(),
			waited: None,
			reaching: BTreeSet::new(),
			counting: BTreeSet::new(),
			groups: Cover::default(),
			missable: Cover::default(),
			covered: None,
			named: BTreeMap::new(),
			unnamed: 0,
		};
		window.place(latest, holdings)?;
		Ok(window)
	}

	/// Retire the ranges keyed `retired`, then resolve `added`, leaving every other range
	/// where it resolved. A key added again replaces its range. Costs the ranges edited, never
	/// the rest.
	///
	/// Refuses what [`Self::new`] does, changing nothing.
	pub fn edit(
		&mut self,
		retired: impl IntoIterator<Item = u64>,
		added: &[(u64, Range)],
		latest: Option<u64>,
		holdings: &impl Holdings,
	) -> Result<()> {
		check(added.iter().map(|(_, range)| range), holdings)?;
		for key in retired {
			self.retire(key);
		}
		for &(key, range) in added {
			self.retire(key);
			self.ranges.insert(key, range);
			if !range.is_empty() {
				self.place_range(key, latest, holdings);
			}
		}
		self.covered = None;
		Ok(())
	}

	/// Take out the range keyed `key`, if any.
	fn retire(&mut self, key: u64) {
		if self.ranges.remove(&key).is_none() {
			return;
		}
		self.pending.remove(&key);
		self.reaching.remove(&key);
		self.counting.remove(&key);
		if let Some(span) = self.spans.remove(&key) {
			self.uncount(key, &span);
		}
	}

	/// Whether nothing is left to resolve as groups arrive.
	pub fn is_settled(&self) -> bool {
		self.pending.is_empty() && self.looking.is_empty() && !self.unplaced()
	}

	/// Whether a relative anchor still waits on a first group.
	pub fn is_pending(&self) -> bool {
		!self.pending.is_empty()
	}

	/// Take the subscriber's current Min Age. Only a pending window reads it, when it places
	/// its ranges, so a join already started stays where it started.
	pub fn set_min(&mut self, min: Option<Duration>) {
		self.min = min;
	}

	/// Whether a live join's reach back waits on a group held starting, to measure the live
	/// edge by.
	pub fn is_reaching(&self) -> bool {
		!self.reaching.is_empty()
	}

	/// Whether a time start still waits on the groups held to place it, where the group
	/// containing it may start above `floor`. One at or below the floor needs no placing: the
	/// floor already keeps out every group but one starting there, and serves the first group
	/// from it either way.
	pub fn is_waiting_above(&self, floor: u64) -> bool {
		self.waiting.last().is_some_and(|&(time, _)| time > floor)
	}

	/// Whether a relative anchor still waits on the groups held to count back to it.
	pub fn is_counting(&self) -> bool {
		!self.counting.is_empty()
	}

	/// Resolve what the groups now held settle: relative anchors once `latest` exists, and
	/// time starts once a group arrives. Returns whether the groups in range moved.
	pub fn settle(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> bool {
		if self.is_pending() {
			if latest.is_none() {
				return false;
			}
			// Checked when the ranges were first resolved, against the same track. Every range
			// places again, as when first resolved: the first group arrives once per track.
			let _ = self.place(latest, holdings);
			self.covered = None;
			return !self.is_pending();
		}
		let mut moved = false;
		if !self.reaching.is_empty() {
			let placed = self.spans.len();
			for key in std::mem::take(&mut self.reaching) {
				self.place_range(key, latest, holdings);
			}
			moved = self.spans.len() > placed;
		}
		// Counted from the latest group now, as an edit placing them now would: the publisher
		// counts from its own latest when the range reaches it.
		if !self.counting.is_empty() && latest.is_some() {
			let placed = self.spans.len();
			for key in std::mem::take(&mut self.counting) {
				self.place_range(key, latest, holdings);
			}
			moved |= self.spans.len() > placed;
		}
		moved |= !self.waiting.is_empty() && self.start_waiting(holdings);
		if moved {
			self.covered = None;
		}
		moved
	}

	/// Start the spans whose time start waits on a group, once one arrived, or once nothing
	/// can arrive any more. Only the times the holdings may now place are tried, so a change
	/// placing one of them costs that one, not every span still waiting. Returns whether any
	/// started.
	fn start_waiting(&mut self, holdings: &impl Holdings) -> bool {
		let complete = holdings.complete();
		let unplaced = holdings.unplaced().filter(|_| !complete);
		let tried: Vec<(u64, u64)> = match unplaced {
			Some(unplaced) if self.waited == Some(unplaced.mark) => outside(&unplaced.runs)
				.flat_map(|gap| self.waiting.range((*gap.start(), 0)..=(*gap.end(), u64::MAX)))
				.copied()
				.collect(),
			_ => {
				self.waited = unplaced.map(|unplaced| unplaced.mark);
				self.waiting.iter().copied().collect()
			}
		};
		let mut started = false;
		for (time, key) in tried {
			let (start, floored) = match holdings.containing(time) {
				Containing::Group(group) => (group, true),
				// The group holding it is older than any held, or nothing can arrive.
				Containing::Before => (0, false),
				Containing::Unknown if complete => (0, false),
				Containing::Unknown => continue,
			};
			started = true;
			self.start_span(key, time, start, floored, holdings);
		}
		started
	}

	/// Start the span keyed `key`, whose time start `time` waited, at `start`: looked up
	/// where the groups held can't place it, as when first resolved, and otherwise reading
	/// its groups.
	fn start_span(&mut self, key: u64, time: u64, start: u64, floored: bool, holdings: &impl Holdings) {
		let asked = matches!(self.ranges[&key].start, Anchor::Time(_));
		let mut span = self.spans.remove(&key).expect("a waiting span");
		self.uncount(key, &span);
		(span.start, span.floored, span.waiting) = (start, floored, false);
		if let Some(below) = unresolved(time, start, floored, holdings).filter(|_| asked) {
			span.looking = Some(Looking { time, below });
		}
		self.count(key, span, holdings);
	}

	/// Whether group `key` is in range. Everything is while a relative anchor waits.
	pub fn contains(&self, key: u64) -> bool {
		self.is_pending() || self.groups.contains(key)
	}

	/// The lowest group in range, `None` while an anchor waits. Nothing in range floors
	/// nowhere: group 0.
	pub fn floor(&self) -> Option<u64> {
		match self.is_pending() || self.unplaced() {
			true => None,
			false => Some(self.groups.lowest().unwrap_or(0)),
		}
	}

	/// The lowest group the ranges name, `None` while one reaches back to the oldest group
	/// or an anchor waits.
	pub fn start(&self) -> Option<u64> {
		match self.is_pending() || !self.reaching.is_empty() || !self.counting.is_empty() || self.unnamed > 0 {
			true => None,
			false => self.lowest_named(),
		}
	}

	/// The lowest group a range names, `None` when none does or an anchor waits.
	/// Unlike [`Self::start`], a range reaching back to the oldest group names nothing.
	pub fn named(&self) -> Option<u64> {
		match self.is_pending() || self.unplaced() {
			true => None,
			false => self.lowest_named(),
		}
	}

	fn lowest_named(&self) -> Option<u64> {
		self.named.first_key_value().map(|(start, _)| *start)
	}

	/// The range keyed `key` resolved: `None` for one whose start still waits, or still
	/// looking up its start, or that isn't held. One empty as given ends at or below its start.
	pub fn resolved_key(&self, key: u64) -> Option<Resolved> {
		let range = self.ranges.get(&key)?;
		if let (Anchor::Time(start), Some(Anchor::Time(_))) = (range.start, range.end)
			&& range.is_empty()
		{
			return Some(Resolved {
				start,
				end: Some(start),
				floored: true,
			});
		}
		let span = self
			.spans
			.get(&key)
			.filter(|span| span.looking.is_none() && !span.waiting)?;
		Some(Resolved {
			start: span.start,
			end: span.end,
			floored: span.floored,
		})
	}

	/// The range keyed `key` while its time start waits on the groups held to place it: that
	/// time, and the exclusive end it resolved to, `None` while it stays open.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn waiting_key(&self, key: u64) -> Option<(u64, Option<u64>)> {
		let span = self.spans.get(&key)?;
		span.waits_on().map(|time| (time, span.end))
	}

	/// Each range resolved, in key order, as [`Self::resolved_key`] reads it.
	pub fn resolved(&self) -> Vec<Option<Resolved>> {
		self.ranges.keys().map(|key| self.resolved_key(*key)).collect()
	}

	/// Each range resolved, by key, leaving out those [`Self::resolved_key`] can't place.
	pub fn resolved_by_key(&self) -> BTreeMap<u64, Resolved> {
		self.ranges
			.keys()
			.filter_map(|key| Some((*key, self.resolved_key(*key)?)))
			.collect()
	}

	/// The keys of the ranges not yet settled, which keep [`Self::is_settled`] false: waiting
	/// on a first group, on the groups held to place or count back to a start, or on a lookup. Costs those.
	pub fn unsettled(&self) -> BTreeSet<u64> {
		let mut keys = self.pending.clone();
		keys.extend(self.reaching.iter().copied());
		keys.extend(self.counting.iter().copied());
		keys.extend(self.waiting.iter().map(|(_, key)| *key));
		keys.extend(self.looking.iter().map(|(_, key)| *key));
		keys
	}

	/// The exclusive end past every group in range, `None` when one stays open or an anchor
	/// waits.
	pub fn end(&self) -> Option<u64> {
		if !self.is_settled() {
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

	fn from<'a>(&'a self, groups: &'a Cover, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + 'a {
		let all = self.is_pending().then_some(from..=u64::MAX);
		let runs = (!self.is_pending()).then(|| groups.runs(from));
		all.into_iter().chain(runs.into_iter().flatten())
	}

	/// The groups in range, `None` while a relative anchor waits and every group is.
	pub fn groups(&self) -> Option<&Cover> {
		(!self.is_pending()).then_some(&self.groups)
	}

	/// The groups a miss may ask for, as [`Self::groups`].
	pub fn missable_groups(&self) -> Option<&Cover> {
		(!self.is_pending()).then_some(&self.missable)
	}

	/// The first run of `range` that is in range.
	pub fn clip(&self, range: ops::Range<u64>) -> Option<ops::Range<u64>> {
		let last = range.end.checked_sub(1).filter(|last| *last >= range.start)?;
		if self.is_pending() {
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
			.filter_map(|(_, key)| self.spans[key].looking.map(|looking| (looking.time, looking.below)))
	}

	/// The lookup of time start `time` answered with the group containing it, or the oldest
	/// group of the history when `time` is older than all of it: its spans start there, once
	/// that group is held. `None`, or an answer not held, means the history couldn't tell, so
	/// they start where the groups held say. Returns whether the groups in range moved.
	pub fn found(&mut self, time: u64, group: Option<u64>, holdings: &impl Holdings) -> bool {
		let found: Vec<u64> = self
			.looking
			.range((time, 0)..=(time, u64::MAX))
			.map(|(_, key)| *key)
			.collect();
		if found.is_empty() {
			return false;
		}
		let held = group.filter(|group| holdings.next_start(*group) == Some(*group));
		for key in found {
			let mut span = self.spans.remove(&key).expect("a span looking up its start");
			self.uncount(key, &span);
			span.looking = None;
			(span.start, span.floored) = match (held, holdings.containing(time)) {
				(Some(group), _) | (None, Containing::Group(group)) => (group, true),
				(None, _) => (0, false),
			};
			self.count(key, span, holdings);
		}
		self.covered = None;
		true
	}

	/// Resolve every range again from scratch.
	fn place(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> Result<()> {
		check(self.ranges.values(), holdings)?;
		self.spans.clear();
		self.pending.clear();
		self.looking.clear();
		self.waiting.clear();
		self.reaching.clear();
		self.counting.clear();
		self.groups = Cover::default();
		self.missable = Cover::default();
		self.named.clear();
		self.unnamed = 0;
		let keys: Vec<u64> = self
			.ranges
			.iter()
			.filter(|(_, range)| !range.is_empty())
			.map(|(key, _)| *key)
			.collect();
		for key in keys {
			self.place_range(key, latest, holdings);
		}
		// Each span waiting was just tried against these holdings.
		self.waited = holdings.unplaced().map(|unplaced| unplaced.mark);
		Ok(())
	}

	/// Resolve the non-empty range keyed `key` into a span, or note what it waits on.
	fn place_range(&mut self, key: u64, latest: Option<u64>, holdings: &impl Holdings) {
		let range = self.ranges[&key];
		let Some(end) = end(range.end, latest, holdings) else {
			self.pending.insert(key);
			return;
		};
		let anchor = match joins(&range, self.min, holdings) {
			None => range.start,
			Some(min) => match latest.map(|latest| holdings.reach_back(latest, min)) {
				Some(Some(anchor)) => anchor,
				// Nothing to measure the live edge by yet: with groups held, read none of them
				// until one measures it; with none, wait as a relative start does.
				Some(None) => {
					self.reaching.insert(key);
					return;
				}
				None => {
					self.pending.insert(key);
					return;
				}
			},
		};
		let (start, floored) = match start(anchor, latest, end, holdings) {
			Start::At(start, floored) => (start, floored),
			Start::Pending => {
				self.pending.insert(key);
				return;
			}
			Start::Counting => {
				self.counting.insert(key);
				return;
			}
			// Reads only the groups from its time until the groups held place it; see
			// [`Self::start_waiting`].
			Start::Waiting => {
				let span = Span::new(anchor, 0, end, false, None);
				self.count(key, Span { waiting: true, ..span }, holdings);
				return;
			}
		};
		// A time start the groups held can't place reads nothing until its lookup answers. A
		// join's reach back never looks one up: it starts from what this hop holds.
		let looking = match (range.start, anchor) {
			(Anchor::Time(_), Anchor::Time(time)) => {
				unresolved(time, start, floored, holdings).map(|below| Looking { time, below })
			}
			_ => None,
		};
		self.count(key, Span::new(anchor, start, end, floored, looking), holdings);
	}

	/// Whether a time start, a join's reach back, or a relative anchor waits on the groups
	/// held to place it.
	fn unplaced(&self) -> bool {
		!self.waiting.is_empty() || !self.reaching.is_empty() || !self.counting.is_empty()
	}

	/// Hold `span` under `key`, counting the groups it reads and where it starts.
	fn count(&mut self, key: u64, mut span: Span, holdings: &impl Holdings) {
		(span.groups, span.missable) = span.reads(holdings);
		if let Some(run) = &span.groups {
			self.groups.add(run.clone(), 1);
		}
		if let Some(run) = &span.missable {
			self.missable.add(run.clone(), 1);
		}
		match span.floored {
			true => *self.named.entry(span.start).or_default() += 1,
			false => self.unnamed += 1,
		}
		if let Some(looking) = span.looking {
			self.looking.insert((looking.time, key));
		}
		if let Some(time) = span.waits_on() {
			self.waiting.insert((time, key));
		}
		self.spans.insert(key, span);
	}

	/// Take back what [`Self::count`] counted for `span`, already out of [`Self::spans`].
	fn uncount(&mut self, key: u64, span: &Span) {
		if let Some(run) = &span.groups {
			self.groups.add(run.clone(), -1);
		}
		if let Some(run) = &span.missable {
			self.missable.add(run.clone(), -1);
		}
		match span.floored {
			true => {
				let count = self.named.get_mut(&span.start).expect("a counted start");
				*count -= 1;
				if *count == 0 {
					self.named.remove(&span.start);
				}
			}
			false => self.unnamed -= 1,
		}
		if let Some(looking) = span.looking {
			self.looking.remove(&(looking.time, key));
		}
		if let Some(time) = span.waits_on() {
			self.waiting.remove(&(time, key));
		}
	}
}

impl Span {
	/// The time start it waits on the groups held to place, if any.
	fn waits_on(&self) -> Option<u64> {
		match (self.waiting, self.anchor) {
			(true, Anchor::Time(time)) => Some(time),
			_ => None,
		}
	}

	fn new(anchor: Anchor, start: u64, end: Option<u64>, floored: bool, looking: Option<Looking>) -> Self {
		Self {
			anchor,
			start,
			end,
			floored,
			looking,
			waiting: false,
			groups: None,
			missable: None,
		}
	}

	/// The groups it reads, and those of them a miss may ask for: from its start through its
	/// end once it knows where it starts, from its time while it waits on its time start, and
	/// none while it looks its start up.
	fn reads(&self, holdings: &impl Holdings) -> (Option<ops::RangeInclusive<u64>>, Option<ops::RangeInclusive<u64>>) {
		if self.looking.is_some() {
			return (None, None);
		}
		let Some(last) = self.end.map_or(Some(u64::MAX), |end| end.checked_sub(1)) else {
			return (None, None);
		};
		// A span waiting on its time start reads from that time meanwhile, and may ask a miss
		// there, as one reaching back past every group held does: whichever group contains
		// it starts at or before it, so every group from there on is in range.
		if self.waiting {
			return match self.anchor {
				Anchor::Time(time) if time <= last => (Some(time..=last), Some(time..=last)),
				_ => (None, None),
			};
		}
		if last < self.start {
			return (None, None);
		}
		// A time start before every group held reaches back to the group containing it,
		// which is unknown here, so it asks no miss below its time: every key from there
		// on is in range, the key at the time itself being the containing group if it
		// exists. A relative one counting back past every group held asks none below the
		// oldest held when it was placed, since which of them it reaches is unknown.
		let reach = match (self.anchor, self.floored) {
			(Anchor::Time(time), false) => time,
			(Anchor::Relative(_), false) => holdings.next_start(0).unwrap_or(self.start),
			_ => self.start,
		};
		(Some(self.start..=last), (reach <= last).then_some(reach..=last))
	}
}

/// Refuse an [`Anchor::Oldest`] end with [`Error::InvalidRange`] where the hop
/// [resolves](Holdings::resolves) ranges itself, and an anchor a received copy's wire drops
/// with [`Error::Unsupported`].
fn check<'a>(mut ranges: impl Iterator<Item = &'a Range> + Clone, holdings: &impl Holdings) -> Result<()> {
	if holdings.resolves() && ranges.clone().any(|range| range.end == Some(Anchor::Oldest)) {
		return Err(Error::InvalidRange);
	}
	if holdings.drops_anchors() && !ranges.all(Range::is_wire) {
		return Err(Error::Unsupported);
	}
	Ok(())
}

/// The times outside `runs`, lowest first, as runs.
fn outside(runs: &[ops::RangeInclusive<u64>]) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
	let mut from = Some(0);
	let mut runs = runs.iter().filter(|run| !run.is_empty());
	std::iter::from_fn(move || {
		loop {
			let at = from?;
			let Some(run) = runs.next() else {
				from = None;
				return Some(at..=u64::MAX);
			};
			from = run.end().checked_add(1).map(|next| next.max(at));
			if at < *run.start() {
				return Some(at..=*run.start() - 1);
			}
		}
	})
}

/// The Min Age a live join reaches back by: `min`, for [`Range::live`], where this hop resolves
/// its own ranges. Any other start names where it starts, the oldest one included, which asks for
/// everything the budget keeps. A zero minimum starts at the latest group anyway.
fn joins(range: &Range, min: Option<Duration>, holdings: &impl Holdings) -> Option<Duration> {
	min.filter(|min| *range == Range::live() && !min.is_zero() && holdings.resolves())
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
	/// A relative start the groups held can't count back to yet, reading nothing meanwhile.
	Counting,
}

/// Where `anchor` starts a range ending at the exclusive `end`.
fn start(anchor: Anchor, latest: Option<u64>, end: Option<u64>, holdings: &impl Holdings) -> Start {
	match anchor {
		// Nothing sits below group 0, so the oldest start needs no latest group.
		Anchor::Oldest => Start::At(0, false),
		Anchor::Relative(back) => match latest {
			None => Start::Pending,
			Some(latest) => match holdings.counted_start(latest, back, end).unwrap_or_else(|| {
				holdings
					.back(latest, back)
					.map_or(Containing::Before, Containing::Group)
			}) {
				Containing::Group(group) => Start::At(group, true),
				// Fewer groups are held than it counts back, so reach back to the oldest.
				Containing::Before => Start::At(0, false),
				Containing::Unknown => Start::Counting,
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
		/// Every key below this was dropped, so a time below it is placed before every
		/// group even while none is held.
		dropped: u64,
		/// What a relative start counts back to, when not over the groups held.
		counted: Option<Containing>,
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
				None if self.held.is_empty() && time >= self.dropped => Containing::Unknown,
				None => Containing::Before,
			}
		}

		fn unplaced(&self) -> Option<Unplaced> {
			let unknown = match self.held.is_empty() {
				true => self.dropped..=u64::MAX,
				false => Unplaced::NONE,
			};
			Some(Unplaced {
				runs: [unknown, Unplaced::NONE],
				mark: 0,
			})
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

		fn counted_start(&self, _: u64, _: u64, _: Option<u64>) -> Option<Containing> {
			self.counted
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

	/// [`Keys`] keeping its [`Holdings::unplaced`] to itself, so a window tries every start
	/// still waiting on each change.
	struct Blind<'a>(&'a Keys);

	impl Holdings for Blind<'_> {
		fn looks_up(&self) -> bool {
			self.0.looks_up()
		}

		fn containing(&self, time: u64) -> Containing {
			self.0.containing(time)
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			self.0.next_start(from)
		}

		fn back(&self, latest: u64, back: u64) -> Option<u64> {
			self.0.back(latest, back)
		}

		fn complete(&self) -> bool {
			self.0.complete()
		}

		fn never_produced(&self, keys: ops::Range<u64>) -> bool {
			self.0.never_produced(keys)
		}
	}

	/// Everything a reader of `window` sees.
	fn seen(window: &Window) -> impl PartialEq + std::fmt::Debug {
		let lookups: BTreeSet<_> = window.lookups().collect();
		(
			window.runs(0).collect::<Vec<_>>(),
			window.missable(0).collect::<Vec<_>>(),
			(window.floor(), window.start(), window.named(), window.end()),
			(
				window.resolved(),
				lookups,
				window.is_settled(),
				!window.waiting.is_empty(),
			),
		)
	}

	proptest::proptest! {
		/// A window that skips the starts its holdings can't place sees what one trying every
		/// start sees, through groups arriving and leaving, drops, edits, lookups answering,
		/// and the track completing.
		#[test]
		fn settling_by_steps_matches_trying_every_start(
			ranges in proptest::collection::vec((proptest::bool::weighted(0.2), 0u64..40, proptest::option::of(0u64..48)), 1..12),
			ops in proptest::collection::vec((0u8..6, 0u64..48), 0..24),
			looks_up: bool,
		) {
			let to_range = |(relative, start, end): (bool, u64, Option<u64>)| {
				let start = match relative {
					true => Anchor::Relative(start % 6),
					false => time(start),
				};
				range(start, end.map(time))
			};
			let ranges: Vec<Range> = ranges.into_iter().map(to_range).collect();
			let mut next_key = ranges.len() as u64;
			let mut keys = Keys {
				looks_up,
				..Keys::default()
			};
			let mut fast = Window::new(&ranges, None, None, &keys).unwrap();
			let mut full = Window::new(&ranges, None, None, &Blind(&keys)).unwrap();
			for (op, value) in ops {
				let latest = keys.held.last().copied();
				match op {
					0 => drop(keys.held.insert(value)),
					1 => keys.dropped = keys.dropped.max(value),
					2 => {
						let Some((time, _)) = fast.lookups().next() else {
							continue;
						};
						let answer = (value % 2 == 0).then_some(value);
						fast.found(time, answer, &keys);
						full.found(time, answer, &Blind(&keys));
					}
					3 => keys.complete = true,
					// Retire one range and add a time start, placed against what is held now.
					4 => {
						let added = [(next_key, range(time(value), None))];
						next_key += 1;
						let retired = [value % next_key];
						fast.edit(retired, &added, latest, &keys).unwrap();
						full.edit(retired, &added, latest, &Blind(&keys)).unwrap();
					}
					// The oldest group held is evicted.
					_ => drop(keys.held.pop_first()),
				}
				let latest = keys.held.last().copied();
				fast.settle(latest, &keys);
				full.settle(latest, &Blind(&keys));
				proptest::prop_assert_eq!(seen(&fast), seen(&full));
			}
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
		let window = Window::new(&ranges, None, Some(9), &keys).unwrap();
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

		// Nothing asked for is nothing in range, floored nowhere.
		let window = Window::new(&[], None, None, &keys).unwrap();
		assert_eq!(covered(&window, 0..4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// A time start names the group containing it, the newest starting at or before it, and
	/// a time end includes every group starting at or before it: sparse starts need no walk.
	#[test]
	fn time_anchors_name_groups_by_their_starts() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(&[range(time(50), time(70))], None, Some(99), &keys).unwrap();
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
		let window = Window::new(&[range(time(33), time(66))], None, Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);

		// An end before its start is empty, and so is one ending before every group.
		let window = Window::new(&[range(time(66), time(33))], None, Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		// Before every start held: reach back to the group containing it, naming no floor.
		let keys = Keys::new([10, 20]);
		let window = Window::new(&[range(time(5), None)], None, Some(20), &keys).unwrap();
		assert_eq!(covered(&window, [10, 20]), vec![10, 20]);
		assert_eq!((window.floor(), window.start(), window.named()), (Some(0), None, None));
		assert_eq!(window.missable(0).next(), Some(5..=u64::MAX), "nothing below its time");
	}

	/// Relative anchors count back through the groups held, however sparse their starts,
	/// and wait for a first group when there is none.
	#[test]
	fn relative_anchors_count_groups_back_from_the_latest() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(
			&[range(Anchor::Relative(2), Anchor::Relative(1))],
			None,
			Some(99),
			&keys,
		)
		.unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);
		assert_eq!(window.start(), Some(33));

		// Counting back past every group held reaches back to the oldest as a start, and ends
		// before them all as an end.
		let window = Window::new(&[range(Anchor::Relative(10), None)], None, Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![0, 33, 66, 99]);
		assert_eq!(window.start(), None);
		let window = Window::new(&[range(Anchor::Oldest, Anchor::Relative(10))], None, Some(99), &keys).unwrap();
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		let mut window = Window::new(&[Range::live()], None, None, &Keys::default()).unwrap();
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Keys::default()));
		let keys = Keys::new([5]);
		assert!(window.settle(Some(5), &keys));
		assert_eq!(covered(&window, 0..8), vec![5, 6, 7]);
		assert!(window.is_settled());

		// The oldest start needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None, None, &keys).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));

		// Any count back, however far, waits on a latest group.
		for back in [u64::MAX, u64::MAX - 1] {
			let open = range(Anchor::Relative(back), None);
			assert!(!open.is_wire());
			assert!(Window::new(&[open], None, None, &keys).unwrap().is_pending());
			assert_eq!(Window::new(&[open], None, Some(5), &keys).unwrap().start(), None);
		}
	}

	/// A relative start the groups held can't count yet reads nothing, rather than every group
	/// as one waiting on a latest group does, and starts once they can, counted from the
	/// latest group then. An end still counts over the groups held.
	#[test]
	fn a_relative_start_reads_nothing_until_counted() {
		let mut keys = Keys::new([1, 2, 9]);
		keys.counted = Some(Containing::Unknown);
		let ranges = [range(Anchor::Relative(2), time(8)), range(time(1), time(1))];
		let mut window = Window::new(&ranges, None, Some(9), &keys).unwrap();
		assert!(window.is_counting() && !window.is_pending() && !window.is_settled());
		assert_eq!(covered(&window, 0..10), vec![1]);
		assert_eq!(
			(window.floor(), window.start(), window.named(), window.end()),
			(None, None, None, None)
		);
		assert_eq!(window.resolved_key(0), None);

		// Nothing to count from while the latest group is unknown.
		keys.counted = Some(Containing::Group(7));
		assert!(!window.settle(None, &keys));
		assert!(window.is_counting());

		assert!(window.settle(Some(9), &keys));
		assert!(window.is_settled() && !window.is_counting());
		assert_eq!(covered(&window, 0..10), vec![1, 7, 8]);
		assert_eq!(window.start(), Some(1));
		assert_eq!(window.end(), Some(9));

		// Retiring a range still counting takes it out of the count.
		keys.counted = Some(Containing::Unknown);
		window
			.edit([], &[(2, range(Anchor::Relative(3), None))], Some(9), &keys)
			.unwrap();
		assert!(window.is_counting());
		window.edit([2], &[], Some(9), &keys).unwrap();
		assert!(window.is_settled());

		// Counting back past every group the track holds reaches back to the oldest.
		keys.counted = Some(Containing::Before);
		let window = Window::new(&[range(Anchor::Relative(5), None)], None, Some(9), &keys).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));
	}

	/// No hop can place the oldest group as an end, so it is refused.
	#[test]
	fn an_oldest_end_is_refused() {
		let ranges = [range(time(0), Anchor::Oldest)];
		assert!(matches!(
			Window::new(&ranges, None, Some(3), &Keys::new(0..4)),
			Err(Error::InvalidRange)
		));
		// A hop holding no groups leaves the refusal to the copies that do.
		assert!(Window::new(&ranges, None, Some(3), &Unheld).is_ok());
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let keys = Keys::new(0..10);
		let start = |ranges: &[Range], latest| Window::new(ranges, None, latest, &keys).unwrap().start();
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
		let mut window = Window::new(&ranges, None, Some(9), &keys).unwrap();
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
		let window = Window::new(&ranges, None, Some(3000), &keys).unwrap();
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
		let window = Window::new(&ranges, None, Some(9), &keys).unwrap();
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
			let mut window = Window::new(&ranges, None, latest, &keys).unwrap();
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
		let mut window = Window::new(&ranges, None, None, &keys).unwrap();
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
		let mut window = Window::new(&[range(time(0), time(7))], None, None, &Keys::new(0..8)).unwrap();
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
		let mut window = Window::new(&ranges, None, None, &Keys::new(0..6)).unwrap();
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
		let mut window = Window::new(&[range(time(8500), None)], None, Some(9000), &keys).unwrap();
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

		let window = Window::new(&[range(time(8500), None)], None, Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().count(), 1, "something may start between 8000 and 8500");
		let window = Window::new(&[range(time(8000), None)], None, Some(9000), &keys).unwrap();
		assert_eq!(window.lookups().count(), 0, "8000 starts exactly there");

		let mut window = Window::new(&[range(time(500), None)], None, Some(9000), &keys).unwrap();
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
		let window = Window::new(&[range(time(50), None)], None, Some(66), &keys).unwrap();
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(33));
	}

	/// A time end on a start the history couldn't place ends where it says, since time ends
	/// name their groups by start.
	#[test]
	fn an_unanswered_lookup_settles_with_its_time_end() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(500), time(3000))], None, Some(9000), &keys).unwrap();
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
		let window = Window::new(
			&[range(time(8500), None), range(time(500), None)],
			None,
			Some(9000),
			&keys,
		)
		.unwrap();
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

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

use super::{Cover, Range, Start, widest};

/// One range resolved to groups: from `start` up to the exclusive `end`, `None` while it
/// stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
	pub start: u64,
	pub end: Option<u64>,
	/// Whether the start names a group, rather than reaching back to the oldest one (a
	/// time start older than every group held, or no start reaching back).
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

/// The groups a serving hop holds, by start, for resolving range starts.
pub(in crate::model) trait Holdings {
	/// Whether this hop resolves ranges against groups of its own, so a live join is placed
	/// here. A hop whose copies resolve them leaves that to them.
	fn resolves(&self) -> bool {
		true
	}

	/// Whether this hop's copies resolve its ranges, so a live join waits on them, reading
	/// everything meanwhile.
	fn defers(&self) -> bool {
		false
	}

	/// Whether a [`Requests`](crate::track::Requests) handler serving the track's history
	/// may look up a time its groups can't resolve: a time start before every group held
	/// then waits on that lookup.
	fn looks_up(&self) -> bool {
		false
	}

	/// Where `time` falls among the groups held, for a range whose groups go stale past
	/// `budget`: a received copy counts a group its upstream gave up as stale only for a range
	/// within the budget it was judged under.
	fn containing(&self, time: u64, budget: Duration) -> Containing;

	/// Runs of times that [`Self::containing`] can't place, so a window skips its time starts
	/// in them rather than try each again whenever the groups held change. `None` when it
	/// can't tell, so each is tried.
	fn unplaced(&self) -> Option<Unplaced> {
		None
	}

	/// The lowest group held starting at or after `from`.
	fn next_start(&self, from: u64) -> Option<u64>;

	/// Whether nothing more can arrive here, so a time start nothing contains reaches back
	/// to the oldest group rather than wait.
	fn complete(&self) -> bool;

	/// Where a live join holding a nonzero `min` behind the live edge, ending at `end`, starts
	/// for a range whose groups go stale past `budget`; see [`Join`]. Only an upstream's answer
	/// recorded after join tick `since`, when the join's range was added, under a budget at
	/// least as wide, counts.
	fn join(&self, min: Option<Duration>, end: Option<u64>, budget: Duration, since: u64) -> Join {
		let _ = (min, end, budget, since);
		Join::Unknown
	}

	/// Whether this hop places a live join only where its upstream's answer to that join
	/// does, never from the groups it holds: a copy holding only part of the track.
	fn answers_joins(&self) -> bool {
		false
	}

	/// The upstream's answer to the live join `min` through `end`, for a reader within `budget`
	/// whose range was added at join tick `since`: the group it starts at, or `None` for nothing
	/// to deliver. It stands once the end is declared too, when [`Self::answers_joins`] no
	/// longer holds: a copy holds only the groups from where its feed started, so they can't
	/// measure a join behind them.
	fn answered_join(
		&self,
		min: Option<Duration>,
		end: Option<u64>,
		budget: Duration,
		since: u64,
	) -> Option<Option<u64>> {
		let _ = (min, end, budget, since);
		None
	}

	/// Whether the producer declared every key in `keys` skipped, so no group starts there
	/// even in the history a handler serves.
	fn never_produced(&self, keys: ops::Range<u64>) -> bool {
		keys.is_empty()
	}
}

/// Where a live join starts, as [`Holdings::join`] finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Join {
	/// The group containing this time, the live edge less the lag, resolved as a range
	/// starting there is, history lookup included.
	Time(u64),
	/// The group starting here, which this hop measured by arrival, or its upstream answered.
	Group(u64),
	/// Past every group, so it starts as a range from the oldest group does, naming no group:
	/// a lower one arriving late is still taken, and none below the groups held is owed.
	Oldest,
	/// Nothing to deliver, as the upstream answered a bounded join.
	Empty,
	/// Nothing measures it yet: a copy waits for its upstream to answer, and any hop for a
	/// group to measure the live edge by.
	Unknown,
}

/// Where a time falls among the groups a hop holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Containing {
	/// A range from it starts at this key: the newest group starting at or before it, or, once
	/// that group ended whole at or before the time, its End, so the range starts at the next
	/// group: a time past a whole group's span lies in a span no group contains.
	At(u64),
	/// Every group held starts after it.
	Before,
	/// No group is held.
	Unknown,
}

/// What the track's history answered a time start's lookup with; see [`Window::found`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
	/// Where the range starts: the group containing the time, or that group's End once it ended
	/// whole at or before the time.
	Group(u64),
	/// No group starts from `floor`, the oldest key the lookup asked from, through the time.
	Dropped { floor: u64 },
	/// Nothing answered: no handler was left to.
	Unanswered,
}

/// The times a hop's [`Holdings::containing`] can't place, as up to four runs, sorted: every
/// time in them is [`Containing::Unknown`], except the few that `mark` places another way.
/// They may leave out some unknown times too, which a window then tries each time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::model) struct Unplaced {
	pub runs: [ops::RangeInclusive<u64>; 4],
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

	fn defers(&self) -> bool {
		true
	}

	fn containing(&self, _: u64, _: Duration) -> Containing {
		Containing::Unknown
	}

	fn next_start(&self, _: u64) -> Option<u64> {
		None
	}

	fn complete(&self) -> bool {
		true
	}
}

/// How long each group stays worth delivering once superseded, as a reader's ranges set it:
/// the widest budget among the ranges covering it, a live join's being its `max`, and a
/// history range's forever, or for as long as the publisher keeps it. A range not yet placed
/// may cover any group up to its end. A group none covers, such as one handed out before the
/// ranges moved, gets the widest budget any range asks: it was reached through one of them, so
/// a history range keeps it from going stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::model) struct Budget {
	/// The groups the placed ranges cover, by the budget they give, widest first.
	layers: Vec<(Duration, Cover)>,
	/// The ranges not yet placed, each as the last group it may cover and its budget: by last
	/// group ascending, budget descending, leaving out any another covers at least as widely.
	unplaced: Vec<(u64, Duration)>,
	/// The widest budget any range asks, placed or not, which a group no range covers gets.
	widest: Duration,
}

impl Budget {
	/// The same budget for every group.
	pub fn uniform(budget: Duration) -> Self {
		Self {
			layers: Vec::new(),
			unplaced: Vec::new(),
			widest: budget,
		}
	}

	/// The budget `layers` and `unplaced` give, in any order, with every other budget zero.
	fn new(mut layers: Vec<(Duration, Cover)>, mut unplaced: Vec<(u64, Duration)>) -> Self {
		layers.retain(|(_, cover)| !cover.is_empty());
		layers.sort_by_key(|(budget, _)| std::cmp::Reverse(*budget));
		// A range covering through a later group at least as widely hides one through an earlier.
		unplaced.sort_unstable();
		let mut kept: Vec<(u64, Duration)> = Vec::new();
		for (last, budget) in unplaced.into_iter().rev() {
			if kept.last().is_none_or(|(_, wider)| budget > *wider) {
				kept.push((last, budget));
			}
		}
		kept.reverse();
		Self {
			layers,
			unplaced: kept,
			widest: Duration::ZERO,
		}
	}

	/// This budget, each group's no wider than `bound`.
	pub fn clamped(mut self, bound: Option<Duration>) -> Self {
		if let Some(bound) = bound {
			for (budget, _) in &mut self.layers {
				*budget = (*budget).min(bound);
			}
			for (_, budget) in &mut self.unplaced {
				*budget = (*budget).min(bound);
			}
			self.widest = self.widest.min(bound);
		}
		self
	}

	/// How long group `key` stays worth delivering once superseded.
	pub fn at(&self, key: u64) -> Duration {
		let placed = (self.layers.iter())
			.find(|(_, cover)| cover.contains(key))
			.map(|(budget, _)| *budget);
		let unplaced = (self.unplaced)
			.get(self.unplaced.partition_point(|(last, _)| *last < key))
			.map(|(_, budget)| *budget);
		placed.max(unplaced).unwrap_or(self.widest)
	}

	/// The widest budget any group gets, counting a range not yet placed.
	pub fn widest(&self) -> Duration {
		self.widest
	}

	/// Whether every group gets the same budget.
	pub fn is_uniform(&self) -> bool {
		(self.layers.iter().map(|(budget, _)| budget))
			.chain(self.unplaced.iter().map(|(_, budget)| budget))
			.all(|budget| *budget == self.widest)
	}

	/// `keys` cut where the budget changes, each stretch with its budget, lowest first.
	pub fn runs(&self, keys: ops::Range<u64>) -> Vec<(ops::Range<u64>, Duration)> {
		let mut runs = Vec::new();
		let mut at = keys.start;
		while at < keys.end {
			let (budget, end) = self.stretch(at);
			let end = end.map_or(keys.end, |end| end.min(keys.end));
			runs.push((at..end, budget));
			at = end;
		}
		runs
	}

	/// The budget of group `key`, and the next key whose budget differs, `None` for none.
	pub fn stretch(&self, key: u64) -> (Duration, Option<u64>) {
		let budget = self.at(key);
		if self.is_uniform() {
			return (budget, None);
		}
		let mut at = key;
		while let Some(cut) = self.cut_after(at) {
			if self.at(cut) != budget {
				return (budget, Some(cut));
			}
			at = cut;
		}
		(budget, None)
	}

	/// The next key past `key` where a layer's run or an unplaced range starts or ends, so the
	/// budget may change.
	fn cut_after(&self, key: u64) -> Option<u64> {
		let layers = self.layers.iter().filter_map(|(_, cover)| {
			let run = cover.first(key, u64::MAX)?;
			match *run.start() == key {
				true => run.end().checked_add(1),
				false => Some(*run.start()),
			}
		});
		let unplaced = (self.unplaced)
			.get(self.unplaced.partition_point(|(last, _)| *last < key))
			.and_then(|(last, _)| last.checked_add(1));
		layers.chain(unplaced).min()
	}

	/// The lowest key a cache whose drift reaches back only to `live_floor` judges: every key
	/// below it with a finite budget is past judging.
	pub fn floor(&self, live_floor: Option<u64>) -> u64 {
		// An unlimited budget anywhere also reaches a group none covers, which may be any.
		match live_floor {
			Some(floor) if self.widest != Duration::MAX => floor,
			_ => 0,
		}
	}
}

/// How a received copy places a live join's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Joining {
	/// From an upstream answer recorded at or after this join tick, the live edge having moved
	/// since any earlier one. See [`Holdings::join`].
	Since(u64),
	/// At this group, where it was already placed: see
	/// [`Subscription::placed`](super::Subscription::placed).
	At(u64),
}

impl Default for Joining {
	fn default() -> Self {
		Self::Since(0)
	}
}

/// A subscriber's [`Range`]s resolved to the group starts it reads, each by the key its
/// caller gave it, so an edit resolves only the ranges it adds and leaves every other where
/// it resolved.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, by key, in the order given.
	ranges: BTreeMap<u64, Range>,
	/// The budgets of the live joins among [`Self::ranges`], counted, for the widest asked.
	budgets: BTreeMap<Duration, usize>,
	/// The non-empty history ranges among [`Self::ranges`], placed or not.
	histories: usize,
	/// The groups the spans read, by the budget of their ranges.
	layers: BTreeMap<Duration, Cover>,
	/// Each non-empty range placed, by key.
	spans: BTreeMap<u64, Span>,
	/// The ranges waiting on a first group, by key: a live join with nothing to reach back
	/// from. Everything is in range meanwhile.
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
	/// The ranges of a live join whose start waits on the groups held to measure the live
	/// edge, or on an upstream's answer, which read nothing meanwhile: a group held is no
	/// reason to start at it.
	reaching: BTreeSet<u64>,
	/// How each live join's range is placed, by key.
	joining: BTreeMap<u64, Joining>,
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
	/// The keys of the spans that reach back to the oldest group instead of naming one.
	unnamed: BTreeSet<u64>,
}

/// One non-empty range, resolved.
#[derive(Clone, Debug)]
struct Span {
	/// The time it starts from: its range's start, or where a live join's lag reached it back
	/// to. `None` reaches back to the oldest group.
	anchor: Option<u64>,
	/// Whether the start is a time the track's history may be asked to place, rather than a
	/// group this hop already measured.
	asks: bool,
	start: u64,
	/// The exclusive end, `None` past every group.
	end: Option<u64>,
	/// Whether the start names a group, rather than reaching back to the oldest one.
	floored: bool,
	/// Its range's budget, which the groups it reads get.
	budget: Duration,
	/// A time start the groups held can't place, whose group a fill asks the track's history
	/// for: the span reads nothing until it answers.
	looking: Option<Looking>,
	/// A time start the groups held can't place yet: the span reads only the groups from
	/// that time until it can.
	waiting: bool,
	/// What it counts in [`Window::groups`] and [`Window::missable`], so taking it out
	/// uncounts exactly that.
	groups: Option<ops::RangeInclusive<u64>>,
	missable: Option<ops::RangeInclusive<u64>>,
}

/// A time start being looked up: its time, and the oldest key the group containing it may
/// start at, past every group held below it.
#[derive(Clone, Copy, Debug)]
struct Looking {
	time: u64,
	floor: u64,
}

impl Window {
	/// Resolve `ranges`, keyed by position, `latest` being the latest group: a live join
	/// reaches back by its lag from the live edge.
	#[cfg(test)]
	pub fn new(ranges: &[Range], latest: Option<u64>, holdings: &impl Holdings) -> Self {
		Self::keyed((0..).zip(ranges.iter().copied()), latest, holdings)
	}

	/// [`Self::new`] for ranges the caller keys itself, in key order.
	#[cfg(test)]
	pub fn keyed(
		ranges: impl IntoIterator<Item = (u64, Range)>,
		latest: Option<u64>,
		holdings: &impl Holdings,
	) -> Self {
		Self::keyed_since(ranges, |_| Joining::default(), latest, holdings)
	}

	/// [`Self::keyed`], each live join's range placed as `joining` of its key says.
	pub fn keyed_since(
		ranges: impl IntoIterator<Item = (u64, Range)>,
		joining: impl Fn(u64) -> Joining,
		latest: Option<u64>,
		holdings: &impl Holdings,
	) -> Self {
		let ranges: BTreeMap<u64, Range> = ranges.into_iter().collect();
		let joining = (ranges.iter())
			.filter(|(_, range)| range.join().is_some())
			.map(|(key, _)| (*key, joining(*key)))
			.collect();
		let mut window = Self {
			ranges: BTreeMap::new(),
			budgets: BTreeMap::new(),
			histories: 0,
			layers: BTreeMap::new(),
			spans: BTreeMap::new(),
			pending: BTreeSet::new(),
			looking: BTreeSet::new(),
			waiting: BTreeSet::new(),
			waited: None,
			reaching: BTreeSet::new(),
			joining,
			groups: Cover::default(),
			missable: Cover::default(),
			covered: None,
			named: BTreeMap::new(),
			unnamed: BTreeSet::new(),
		};
		for range in ranges.values() {
			window.count_budget(range);
		}
		window.ranges = ranges;
		window.place(latest, holdings);
		window
	}

	/// Retire the ranges keyed `retired`, then resolve `added`, leaving every other range
	/// where it resolved. A key added again replaces its range, but a live join added again
	/// with only its `max` changed keeps where it started. Costs the ranges edited, never the
	/// rest.
	pub fn edit(
		&mut self,
		retired: impl IntoIterator<Item = u64>,
		added: &[(u64, Range)],
		latest: Option<u64>,
		holdings: &impl Holdings,
	) {
		self.edit_since(retired, added, |_| Joining::default(), latest, holdings);
	}

	/// [`Self::edit`], each live join's range placed as `joining` of its key says. One already
	/// held is retuned only when it is placed as it was.
	pub fn edit_since(
		&mut self,
		retired: impl IntoIterator<Item = u64>,
		added: &[(u64, Range)],
		joining: impl Fn(u64) -> Joining,
		latest: Option<u64>,
		holdings: &impl Holdings,
	) {
		let placed = |joining: Joining| match joining {
			Joining::At(group) => Some(group),
			Joining::Since(_) => None,
		};
		let retuned: BTreeSet<u64> = (added.iter())
			.filter(|(key, range)| {
				range
					.join()
					.is_some_and(|join| self.ranges.get(key).and_then(Range::join) == Some(join))
					&& self.joining.get(key).copied().and_then(placed) == placed(joining(*key))
			})
			.map(|(key, _)| *key)
			.collect();
		for key in retired {
			if !retuned.contains(&key) {
				self.retire(key);
			}
		}
		for &(key, range) in added {
			if retuned.contains(&key) {
				let old = self.ranges.insert(key, range).expect("a retuned range");
				self.uncount_budget(&old);
				self.count_budget(&range);
				// Its groups take the new budget where it stands.
				if let Some(span) = self.spans.remove(&key) {
					self.uncount(key, &span);
					self.count(
						key,
						Span {
							budget: range.budget(),
							..span
						},
					);
				}
				continue;
			}
			self.retire(key);
			self.ranges.insert(key, range);
			self.count_budget(&range);
			if range.join().is_some() {
				self.joining.insert(key, joining(key));
			}
			if !range.is_empty() {
				self.place_range(key, latest, holdings);
			}
		}
		self.covered = None;
	}

	/// Count a live join's budget among [`Self::budgets`], or a history range among
	/// [`Self::histories`].
	fn count_budget(&mut self, range: &Range) {
		if range.join().is_some() {
			*self.budgets.entry(range.budget()).or_default() += 1;
		} else if !range.is_empty() {
			self.histories += 1;
		}
	}

	/// Take back what [`Self::count_budget`] counted.
	fn uncount_budget(&mut self, range: &Range) {
		if range.join().is_none() {
			if !range.is_empty() {
				self.histories -= 1;
			}
			return;
		}
		let count = self.budgets.get_mut(&range.budget()).expect("a counted budget");
		*count -= 1;
		if *count == 0 {
			self.budgets.remove(&range.budget());
		}
	}

	/// How long each group stays worth delivering once superseded, as these ranges set it.
	pub fn budget(&self) -> Budget {
		let widest = match self.histories {
			0 => (self.budgets.last_key_value()).map_or(Duration::ZERO, |(budget, _)| *budget),
			_ => Duration::MAX,
		};
		let layers = self.layers.iter().map(|(budget, cover)| (*budget, cover.clone()));
		Budget {
			widest,
			..Budget::new(layers.collect(), self.unplaced_budgets(|key| self.ranges.get(&key)))
		}
	}

	/// Each range not yet placed, as `range` of its key says, as the last group it may cover
	/// and its budget: a pending one may cover any up to its end, since every group is in
	/// range meanwhile, and a live join still reaching may start at any.
	fn unplaced_budgets<'a>(&self, range: impl Fn(u64) -> Option<&'a Range>) -> Vec<(u64, Duration)> {
		(self.pending.iter().chain(&self.reaching))
			.filter_map(|key| range(*key))
			.filter(|range| !range.is_empty())
			.map(|range| (range.end.unwrap_or(u64::MAX), range.budget()))
			.collect()
	}

	/// [`Self::budget`] with each range's staleness taken from `ranges`, by key, rather than
	/// from the range it was placed as: a front's reader places a live join where a copy
	/// resolved it, as a time start, and the join's groups still go stale past its `max`.
	pub fn budget_of(&self, ranges: &BTreeMap<u64, Range>) -> Budget {
		let mut layers: BTreeMap<Duration, Cover> = BTreeMap::new();
		for (key, span) in &self.spans {
			if let (Some(run), Some(range)) = (&span.groups, ranges.get(key)) {
				layers.entry(range.budget()).or_default().add(run.clone(), 1);
			}
		}
		Budget {
			widest: widest(ranges.values()),
			..Budget::new(
				layers.into_iter().collect(),
				self.unplaced_budgets(|key| ranges.get(&key)),
			)
		}
	}

	/// Take out the range keyed `key`, if any.
	fn retire(&mut self, key: u64) {
		let Some(range) = self.ranges.remove(&key) else {
			return;
		};
		self.uncount_budget(&range);
		self.pending.remove(&key);
		self.reaching.remove(&key);
		self.joining.remove(&key);
		if let Some(span) = self.spans.remove(&key) {
			self.uncount(key, &span);
		}
	}

	/// Whether nothing is left to resolve as groups arrive.
	pub fn is_settled(&self) -> bool {
		self.pending.is_empty() && self.looking.is_empty() && !self.unplaced()
	}

	/// Whether a live join still waits on a first group.
	pub fn is_pending(&self) -> bool {
		!self.pending.is_empty()
	}

	/// The time starts still waiting on the groups held: what a received copy asks its
	/// upstream to place.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn asks(&self) -> impl Iterator<Item = u64> + '_ {
		let mut last = None;
		self.waiting
			.iter()
			.filter_map(move |&(time, _)| (last.replace(time) != Some(time)).then_some(time))
	}

	/// The live joins still waiting on an answer, with their lag, end, budget, and the join
	/// tick each was added at: what a received copy asks its upstream to place.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn joins(&self) -> impl Iterator<Item = (Option<Duration>, Option<u64>, Duration, u64)> + '_ {
		self.reaching.iter().filter_map(|key| {
			let range = self.ranges.get(key)?;
			let since = match self.joining.get(key) {
				Some(Joining::Since(since)) => *since,
				_ => 0,
			};
			let (min, end) = range.join()?;
			Some((min, join_end(end), range.budget(), since))
		})
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

	/// The times in `times` of the time starts still waiting on the groups held. The group
	/// containing each starts at or before its time, so one at or past a read's floor and
	/// below a group may still land between them.
	pub fn waiting_in<R: ops::RangeBounds<u64> + 'static>(&self, times: R) -> impl Iterator<Item = u64> + '_ {
		let from = match times.start_bound() {
			ops::Bound::Included(&start) => start,
			ops::Bound::Excluded(&start) => start.saturating_add(1),
			ops::Bound::Unbounded => 0,
		};
		self.waiting
			.range((from, 0)..)
			.map(|&(time, _)| time)
			.take_while(move |time| times.contains(time))
	}

	/// Resolve what the groups now held settle: live joins once `latest` exists or an
	/// upstream answers, and time starts once a group arrives. Returns whether the groups in
	/// range moved.
	pub fn settle(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> bool {
		if self.is_pending() {
			if latest.is_none() {
				return false;
			}
			// Every range places again, as when first resolved: the first group arrives once per
			// track.
			self.place(latest, holdings);
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
			let (start, floored) = match holdings.containing(time, self.ranges[&key].budget()) {
				Containing::At(start) => (start, true),
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
		let mut span = self.spans.remove(&key).expect("a waiting span");
		self.uncount(key, &span);
		(span.start, span.floored, span.waiting) = (start, floored, false);
		if let Some(floor) = unresolved(time, start, floored, holdings).filter(|_| span.asks) {
			span.looking = Some(Looking { time, floor });
		}
		self.count(key, span);
	}

	/// The ranges by key, and how each live join among them is placed, as given.
	pub fn ranges(&self) -> (&BTreeMap<u64, Range>, &BTreeMap<u64, Joining>) {
		(&self.ranges, &self.joining)
	}

	/// Whether group `key` is in range. Everything is while a live join waits on a first group.
	pub fn contains(&self, key: u64) -> bool {
		self.is_pending() || self.groups.contains(key)
	}

	/// Whether a range placed so far holds group `key`: unlike [`Self::contains`], not every
	/// group while a live join waits on a first group.
	pub fn holds(&self, key: u64) -> bool {
		self.groups.contains(key)
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
		match self.is_pending() || !self.reaching.is_empty() || !self.unnamed.is_empty() {
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
		if let (Start::Time(start), Some(_)) = (range.start, range.end)
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
	/// on a first group, on the groups held to place a start, or on a lookup. Costs those.
	pub fn unsettled(&self) -> BTreeSet<u64> {
		let mut keys = self.pending.clone();
		keys.extend(self.reaching.iter().copied());
		keys.extend(self.waiting.iter().map(|(_, key)| *key));
		keys.extend(self.looking.iter().map(|(_, key)| *key));
		keys
	}

	/// The keys of the ranges whose start names no group yet, which [`Self::resolved_key`]
	/// leaves unresolved or unfloored: waiting on a first group or a live edge, reaching back
	/// to the oldest group, or waiting on or looking up a time start. Costs those, nothing
	/// once every start names a group.
	pub fn unnamed_keys(&self) -> impl Iterator<Item = u64> + '_ {
		// A span waiting on its time start, or looking one up from the oldest group, is unfloored.
		let looking = (self.looking.iter())
			.map(|(_, key)| *key)
			.filter(|key| self.spans[key].floored);
		(self.pending.iter().chain(&self.reaching).chain(&self.unnamed))
			.copied()
			.chain(looking)
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
	/// them but those starting before a time start that fell before every group held.
	pub fn missable(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		self.from(&self.missable, from)
	}

	fn from<'a>(&'a self, groups: &'a Cover, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + 'a {
		let all = self.is_pending().then_some(from..=u64::MAX);
		let runs = (!self.is_pending()).then(|| groups.runs(from));
		all.into_iter().chain(runs.into_iter().flatten())
	}

	/// The groups in range, `None` while a live join waits on a first group and every group is.
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

	/// The time starts still looked up, each with the oldest key the group containing it may
	/// start at: past every group held below the time, which holds it if the track's history
	/// has one.
	pub fn lookups(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
		self.looking
			.iter()
			.filter_map(|(_, key)| self.spans[key].looking.map(|looking| (looking.time, looking.floor)))
	}

	/// The lookup of time start `time` answered: its spans start where the answer says,
	/// never where the groups held happen to. Left unanswered because no handler is left,
	/// they start where the groups held say, as with no handler at all, unless the cache since
	/// evicted the group their floor was past and a handler is back: then the history may
	/// hold the answer below that floor, so they look it up again from lower down. Each
	/// look-up again needs another eviction, so it ends. Returns whether the groups in range
	/// moved.
	pub fn found(&mut self, time: u64, found: Found, holdings: &impl Holdings) -> bool {
		let keys: Vec<u64> = self
			.looking
			.range((time, 0)..=(time, u64::MAX))
			.map(|(_, key)| *key)
			.collect();
		if keys.is_empty() {
			return false;
		}
		for key in keys {
			let mut span = self.spans.remove(&key).expect("a span looking up its start");
			self.uncount(key, &span);
			let asked = span.looking.take().map(|looking| looking.floor);
			(span.start, span.floored) = match found {
				Found::Group(group) => (group, true),
				// A floor is one past where the groups held placed it, which then holds it; from 0,
				// no group starts at or before it, so the span reads from the time on.
				Found::Dropped { floor } => (floor.checked_sub(1).unwrap_or(time), true),
				Found::Unanswered => match holdings.containing(time, self.ranges[&key].budget()) {
					Containing::At(start) => (start, true),
					_ => (0, false),
				},
			};
			if let Found::Unanswered = found {
				let floor = match span.floored {
					true => unresolved(time, span.start, true, holdings),
					// Nothing held at or before the time any more, maybe nothing at all: the group
					// containing it may start anywhere below the floor asked.
					false => holdings.looks_up().then_some(0),
				};
				span.looking = floor
					.filter(|floor| asked.is_some_and(|asked| *floor < asked))
					.map(|floor| Looking { time, floor });
			}
			self.count(key, span);
		}
		self.covered = None;
		true
	}

	/// Resolve every range again from scratch.
	fn place(&mut self, latest: Option<u64>, holdings: &impl Holdings) {
		// A live join still waiting keeps the answers it may take.
		self.reaching.clear();
		self.spans.clear();
		self.pending.clear();
		self.looking.clear();
		self.waiting.clear();
		self.groups = Cover::default();
		self.missable = Cover::default();
		self.layers.clear();
		self.named.clear();
		self.unnamed.clear();
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
	}

	/// Resolve the non-empty range keyed `key` into a span, or note what it waits on.
	fn place_range(&mut self, key: u64, latest: Option<u64>, holdings: &impl Holdings) {
		let range = self.ranges[&key];
		let budget = range.budget();
		// Every end includes its group.
		let end = range.end.and_then(|end| end.checked_add(1));
		let (anchor, asks) = match range.start {
			Start::Time(time) => (Some(time), true),
			// The oldest group held.
			Start::Oldest => (None, false),
			// Placed already, where an earlier copy resolved it: its lag places nothing.
			Start::Live { .. } if let Some(Joining::At(group)) = self.joining.get(&key) => (Some(*group), false),
			// A hop whose copies place the join waits on them, as one with no group yet does.
			Start::Live { .. } if holdings.defers() => {
				self.pending.insert(key);
				return;
			}
			// Placed already, where a copy resolved it: one left with no start reached back.
			Start::Live { .. } if !holdings.resolves() => (None, false),
			// Only its upstream's answer places it, whatever this hop holds: read none of the
			// groups held until it does.
			Start::Live { min, .. } if holdings.answers_joins() => {
				let since = match self.joining.get(&key) {
					Some(Joining::Since(since)) => *since,
					_ => 0,
				};
				// Nothing to deliver: a start past its end holds nothing.
				let past = range.end.and_then(|last| last.checked_add(1));
				match (holdings.join(min, join_end(range.end), budget, since), past) {
					(Join::Group(group) | Join::Time(group), _) | (Join::Empty, Some(group)) => (Some(group), false),
					(Join::Oldest, _) => (None, false),
					(Join::Empty | Join::Unknown, _) => {
						self.reaching.insert(key);
						return;
					}
				}
			}
			Start::Live { min, .. } => {
				let since = match self.joining.get(&key) {
					Some(Joining::Since(since)) => *since,
					_ => 0,
				};
				let until = join_end(range.end);
				// Its upstream's answer places it, whatever the groups held measure: nothing to
				// deliver starts it past its end, while an unbounded one measures.
				let answered = match holdings.answered_join(min, until, budget, since) {
					Some(Some(group)) => Some(group),
					Some(None) => range.end.and_then(|last| last.checked_add(1)),
					None => None,
				};
				match (answered, latest) {
					(Some(group), _) => (Some(group), false),
					// Nothing to measure the live edge by yet: wait for the first group, reading
					// everything meanwhile.
					(None, None) => {
						self.pending.insert(key);
						return;
					}
					// The latest group holds the live edge. One made before any group then starts at
					// the first, which was the latest when it was placed.
					(None, Some(latest)) if min == Some(Duration::ZERO) => (Some(latest), false),
					(None, Some(_)) => match holdings.join(min, until, budget, since) {
						Join::Time(time) => (Some(time), true),
						Join::Group(group) => (Some(group), false),
						Join::Oldest => (None, false),
						// Nothing to measure the live edge by yet: read none of the groups held until
						// it resolves.
						Join::Empty | Join::Unknown => {
							self.reaching.insert(key);
							return;
						}
					},
				}
			}
		};
		// A group a join measured starts there; a time is placed among the groups held.
		let placed = match (anchor, asks) {
			(Some(group), false) => Placed::At(group, true),
			_ => start(anchor, latest, budget, holdings),
		};
		let (start, floored) = match placed {
			Placed::At(start, floored) => (start, floored),
			Placed::Pending => {
				self.pending.insert(key);
				return;
			}
			// Reads only the groups from its time until the groups held place it; see
			// [`Self::start_waiting`].
			Placed::Waiting => {
				let span = Span::new(anchor, asks, 0, end, false, budget, None);
				self.count(key, Span { waiting: true, ..span });
				return;
			}
		};
		// A time start the groups held can't place reads nothing until its lookup answers. A
		// group a join measured is held, or an upstream reported it, so it looks nothing up.
		let looking = match anchor.filter(|_| asks) {
			Some(time) => unresolved(time, start, floored, holdings).map(|floor| Looking { time, floor }),
			None => None,
		};
		self.count(key, Span::new(anchor, asks, start, end, floored, budget, looking));
	}

	/// Whether a time start or a join's reach back waits on the groups held to place it.
	fn unplaced(&self) -> bool {
		!self.waiting.is_empty() || !self.reaching.is_empty()
	}

	/// Hold `span` under `key`, counting the groups it reads and where it starts.
	fn count(&mut self, key: u64, mut span: Span) {
		(span.groups, span.missable) = span.reads();
		if let Some(run) = &span.groups {
			self.groups.add(run.clone(), 1);
			self.layers.entry(span.budget).or_default().add(run.clone(), 1);
		}
		if let Some(run) = &span.missable {
			self.missable.add(run.clone(), 1);
		}
		match span.floored {
			true => *self.named.entry(span.start).or_default() += 1,
			false => _ = self.unnamed.insert(key),
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
			let layer = self.layers.get_mut(&span.budget).expect("a counted layer");
			layer.add(run.clone(), -1);
			if layer.is_empty() {
				self.layers.remove(&span.budget);
			}
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
			false => _ = self.unnamed.remove(&key),
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
		self.anchor.filter(|_| self.waiting)
	}

	fn new(
		anchor: Option<u64>,
		asks: bool,
		start: u64,
		end: Option<u64>,
		floored: bool,
		budget: Duration,
		looking: Option<Looking>,
	) -> Self {
		Self {
			anchor,
			asks,
			start,
			end,
			floored,
			budget,
			looking,
			waiting: false,
			groups: None,
			missable: None,
		}
	}

	/// The groups it reads, and those of them a miss may ask for: from its start through its
	/// end once it knows where it starts, from its time while it waits on its time start, and
	/// none while it looks its start up.
	fn reads(&self) -> (Option<ops::RangeInclusive<u64>>, Option<ops::RangeInclusive<u64>>) {
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
				Some(time) if time <= last => (Some(time..=last), Some(time..=last)),
				_ => (None, None),
			};
		}
		if last < self.start {
			return (None, None);
		}
		// A time start before every group held reaches back to the group containing it,
		// which is unknown here, so it asks no miss below its time: every key from there
		// on is in range, the key at the time itself being the containing group if it
		// exists.
		let reach = match (self.anchor, self.floored) {
			(Some(time), false) => time,
			_ => self.start,
		};
		(Some(self.start..=last), (reach <= last).then_some(reach..=last))
	}
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

/// Where a range starts.
enum Placed {
	/// At this group, and whether it names that group rather than reaching back to the
	/// oldest one.
	At(u64, bool),
	/// A time start the groups held can't place yet, so it reads only the groups from that
	/// time.
	Waiting,
	/// Waiting on a latest group, reading everything meanwhile.
	Pending,
}

fn start(anchor: Option<u64>, latest: Option<u64>, budget: Duration, holdings: &impl Holdings) -> Placed {
	match anchor {
		// Nothing sits below group 0, so the oldest start needs no latest group.
		None => Placed::At(0, false),
		// No group starts before 0, so time 0 names group 0 whatever is held.
		Some(0) => Placed::At(0, true),
		Some(time) => match holdings.containing(time, budget) {
			Containing::At(start) => Placed::At(start, true),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Placed::At(0, false),
			// Before any group, which a hop that holds none (such as a reader following route
			// copies) leaves to the copies to resolve.
			Containing::Unknown if latest.is_none() && !holdings.resolves() => Placed::Pending,
			// Nothing can arrive any more, so nothing held reaches past it.
			Containing::Unknown if holdings.complete() => Placed::At(0, false),
			// The groups held can't place it, so wait for one that can rather than guess from the
			// latest: a copy can see the live edge before the groups its publisher resolved the
			// time to.
			Containing::Unknown => Placed::Waiting,
		},
	}
}

/// The oldest key a fill of the group containing time start `time`, resolved to `start`,
/// asks from, when the groups held can't tell which group contains it and a handler may: it
/// is before every group held, or a group the cache evicted may start after `start`, where the
/// groups held placed it, or at `start` itself when that is a whole group's End, and still at
/// or before `time`. Keys the producer skipped hold none.
fn unresolved(time: u64, start: u64, floored: bool, holdings: &impl Holdings) -> Option<u64> {
	if !holdings.looks_up() {
		return None;
	}
	if !floored {
		return holdings.next_start(0).map(|_| 0);
	}
	// Past a group held at the start, or one starting at the time, which contains it whatever
	// its End. A start below the time no group holds is a whole group's End, where a group the
	// cache let go may start and end before the time, so the search takes it in.
	let next = match holdings.next_start(start) == Some(start) || start >= time {
		true => start.checked_add(1)?,
		false => start,
	};
	let after = holdings.next_start(next)?;
	let gap = next..after.min(time.saturating_add(1));
	(!holdings.never_produced(gap)).then_some(next)
}

/// The end a live join is asked and answered under. One through the reserved group
/// `u64::MAX` reaches every group, so the wire carries it unbounded and its answer comes back so.
fn join_end(end: Option<u64>) -> Option<u64> {
	end.filter(|end| end.checked_add(1).is_some())
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
		/// Where a live join with a nonzero lag starts, `None` for nowhere yet.
		join: Option<Join>,
		/// The tick of the newest join answer, on a copy that places a live join only from
		/// its upstream's answer: `join` counts only for a join waiting since before it.
		answered: Option<u64>,
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

		fn containing(&self, time: u64, _: Duration) -> Containing {
			match self.held.range(..=time).next_back() {
				Some(key) => Containing::At(*key),
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
				runs: [unknown, Unplaced::NONE, Unplaced::NONE, Unplaced::NONE],
				mark: 0,
			})
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			self.held.range(from..).next().copied()
		}

		fn join(&self, _: Option<Duration>, _: Option<u64>, _: Duration, since: u64) -> Join {
			match self.answered {
				Some(tick) if tick <= since => Join::Unknown,
				_ => self.join.unwrap_or(Join::Unknown),
			}
		}

		fn answers_joins(&self) -> bool {
			self.answered.is_some()
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

		fn containing(&self, time: u64, budget: Duration) -> Containing {
			self.0.containing(time, budget)
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			self.0.next_start(from)
		}

		fn join(&self, min: Option<Duration>, end: Option<u64>, budget: Duration, since: u64) -> Join {
			self.0.join(min, end, budget, since)
		}

		fn answers_joins(&self) -> bool {
			self.0.answers_joins()
		}

		fn answered_join(
			&self,
			min: Option<Duration>,
			end: Option<u64>,
			budget: Duration,
			since: u64,
		) -> Option<Option<u64>> {
			self.0.answered_join(min, end, budget, since)
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
			ranges in proptest::collection::vec((0u8..10, 0u64..40, proptest::option::of(0u64..48)), 1..12),
			ops in proptest::collection::vec((0u8..6, 0u64..48), 0..24),
			looks_up: bool,
			join in proptest::option::of(0u64..48),
		) {
			// Mostly time starts, with some reaching back and some live joins of a small lag or none.
			let to_range = |(kind, start, end): (u8, u64, Option<u64>)| match kind {
				0 => range(None, end),
				1 => live((start % 4 < 3).then(|| Duration::from_millis(start % 4)), end),
				_ => range(time(start), end),
			};
			let ranges: Vec<Range> = ranges.into_iter().map(to_range).collect();
			let mut next_key = ranges.len() as u64;
			let mut keys = Keys {
				looks_up,
				join: join.map(Join::Time),
				..Keys::default()
			};
			let mut fast = Window::new(&ranges, None, &keys);
			let mut full = Window::new(&ranges, None, &Blind(&keys));
			for (op, value) in ops {
				let latest = keys.held.last().copied();
				match op {
					0 => drop(keys.held.insert(value)),
					1 => keys.dropped = keys.dropped.max(value),
					2 => {
						let Some((time, floor)) = fast.lookups().next() else {
							continue;
						};
						let answer = match value % 3 {
							0 => Found::Group(value),
							1 => Found::Dropped { floor },
							_ => Found::Unanswered,
						};
						fast.found(time, answer, &keys);
						full.found(time, answer, &Blind(&keys));
					}
					3 => keys.complete = true,
					// Retire one range and add a time start, placed against what is held now.
					4 => {
						let added = [(next_key, range(time(value), None))];
						next_key += 1;
						let retired = [value % next_key];
						fast.edit(retired, &added, latest, &keys);
						full.edit(retired, &added, latest, &Blind(&keys));
					}
					// The oldest group held is evicted.
					_ => drop(keys.held.pop_first()),
				}
				let latest = keys.held.last().copied();
				fast.settle(latest, &keys);
				full.settle(latest, &Blind(&keys));
				proptest::prop_assert_eq!(seen(&fast), seen(&full));
				proptest::prop_assert_eq!(unnamed_keys(&fast), unnamed_by_scan(&fast));
				proptest::prop_assert_eq!(unnamed_keys(&full), unnamed_by_scan(&full));
			}
		}
	}

	/// [`Window::unnamed_keys`], sorted, checking it lists none twice.
	fn unnamed_keys(window: &Window) -> Vec<u64> {
		let mut keys: Vec<u64> = window.unnamed_keys().collect();
		keys.sort_unstable();
		let len = keys.len();
		keys.dedup();
		assert_eq!(keys.len(), len, "a key listed twice");
		keys
	}

	/// The keys [`Window::unnamed_keys`] lists, found by resolving every range.
	fn unnamed_by_scan(window: &Window) -> Vec<u64> {
		(window.ranges.keys().copied())
			.filter(|key| !window.resolved_key(*key).is_some_and(|resolved| resolved.floored))
			.collect()
	}

	/// A range from time `start`, or reaching back to the oldest group with none.
	fn range(start: Option<u64>, end: impl Into<Option<u64>>) -> Range {
		let start = match start {
			Some(time) => Start::Time(time),
			None => Start::Oldest,
		};
		Range { start, end: end.into() }
	}

	/// A live join `min` behind the live edge, or with no lag limit for `None`.
	fn live(min: impl Into<Option<Duration>>, end: impl Into<Option<u64>>) -> Range {
		Range {
			start: Start::Live {
				min: min.into(),
				max: None,
			},
			end: end.into(),
		}
	}

	fn time(start: u64) -> Option<u64> {
		Some(start)
	}

	fn covered(window: &Window, keys: impl IntoIterator<Item = u64>) -> Vec<u64> {
		keys.into_iter().filter(|key| window.contains(*key)).collect()
	}

	/// Disjoint ranges filter to exactly their groups; the gap between them is out.
	#[test]
	fn ranges_cover_only_their_groups() {
		let keys = Keys::new(0..10);
		let ranges = [range(time(2), time(3)), range(time(7), None)];
		let window = Window::new(&ranges, Some(9), &keys);
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
		let window = Window::new(&[], None, &keys);
		assert_eq!(covered(&window, 0..4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// A time start names the group containing it, the newest starting at or before it, and
	/// a time end includes every group starting at or before it: sparse starts need no walk.
	#[test]
	fn time_anchors_name_groups_by_their_starts() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(&[range(time(50), time(70))], Some(99), &keys);
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
		let window = Window::new(&[range(time(33), time(66))], Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);

		// An end before its start is empty, and so is one ending before every group.
		let window = Window::new(&[range(time(66), time(33))], Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		// Before every start held: reach back to the group containing it, naming no floor.
		let keys = Keys::new([10, 20]);
		let window = Window::new(&[range(time(5), None)], Some(20), &keys);
		assert_eq!(covered(&window, [10, 20]), vec![10, 20]);
		assert_eq!((window.floor(), window.start(), window.named()), (Some(0), None, None));
		assert_eq!(window.missable(0).next(), Some(5..=u64::MAX), "nothing below its time");
	}

	/// A range with no start reaches back to the oldest group, and a live join with no lag
	/// starts at the latest group, waiting for a first group when there is none.
	#[test]
	fn a_live_join_with_no_lag_starts_at_the_latest_group() {
		let keys = Keys::new([0, 33, 66, 99]);
		let oldest = Window::new(&[Range::ALL], Some(99), &keys);
		assert_eq!(covered(&oldest, [0, 33, 66, 99]), vec![0, 33, 66, 99]);
		assert_eq!((oldest.floor(), oldest.start()), (Some(0), None));

		let joined = Window::new(&[live(Duration::ZERO, None)], Some(99), &keys);
		assert_eq!(covered(&joined, [0, 33, 66, 99]), vec![99]);
		assert_eq!(joined.start(), Some(99));
		// An end before where it joins holds nothing.
		let short = Window::new(&[live(Duration::ZERO, time(50))], Some(99), &keys);
		assert_eq!(covered(&short, [0, 33, 66, 99]), Vec::<u64>::new());

		let mut window = Window::new(&[live(Duration::ZERO, None)], None, &Keys::default());
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Keys::default()));
		let keys = Keys::new([5]);
		assert!(window.settle(Some(5), &keys));
		assert_eq!(covered(&window, 0..8), vec![5, 6, 7]);
		assert!(window.is_settled());
	}

	/// A live join with a lag starts where the holdings measure it: a time resolves as a
	/// time start, history lookup included, and a group starts there. Until they measure it
	/// the join reads nothing.
	#[test]
	fn a_live_join_starts_where_the_holdings_place_it() {
		let join = [live(Duration::from_millis(40), None)];
		let mut keys = Keys::new([0, 33, 66, 99]);
		keys.join = Some(Join::Time(59));
		let window = Window::new(&join, Some(99), &keys);
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 33,
				end: None,
				floored: true
			})]
		);
		keys.join = Some(Join::Group(66));
		let window = Window::new(&join, Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![66, 99]);

		// A time the groups held can't place is looked up, as a time start is.
		keys.looks_up = true;
		keys.join = Some(Join::Time(20));
		keys.held.remove(&0);
		let window = Window::new(&join, Some(99), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(20, 0)]);

		keys.looks_up = false;
		keys.join = None;
		let mut window = Window::new(&join, Some(99), &keys);
		assert!(window.is_reaching() && !window.contains(99) && window.resolved() == [None]);
		assert!(!window.settle(Some(99), &keys), "still nothing to measure it by");
		keys.join = Some(Join::Group(66));
		assert!(window.settle(Some(99), &keys));
		assert!(!window.is_reaching());
		assert_eq!(window.start(), Some(66));
	}

	/// On a copy that places a live join only from its upstream's answer, a join reads none
	/// of the groups held, whatever its lag, and takes only an answer recorded after it began
	/// waiting: an answer for an earlier reader proves nothing about the live edge now.
	#[test]
	fn a_copy_places_a_live_join_only_from_a_fresh_answer() {
		let mut keys = Keys::new([0, 33, 66, 99]);
		keys.join = Some(Join::Group(33));
		for min in [Duration::ZERO, Duration::from_millis(40)] {
			keys.answered = Some(3);
			let mut window = Window::keyed_since([(0, live(min, None))], |_| Joining::Since(3), Some(99), &keys);
			assert!(
				window.is_reaching() && !window.contains(99),
				"an answer from before it waited"
			);
			assert_eq!(
				window.joins().collect::<Vec<_>>(),
				[(Some(min), None, Duration::MAX, 3)]
			);
			// One through the reserved last group asks and waits as an unbounded one, as the wire
			// carries it.
			let through = Window::keyed_since([(0, live(min, time(u64::MAX)))], |_| Joining::Since(3), Some(99), &keys);
			assert_eq!(
				through.joins().collect::<Vec<_>>(),
				[(Some(min), None, Duration::MAX, 3)]
			);

			// A newer answer places it, at exactly the group upstream answered.
			keys.answered = Some(4);
			assert!(window.settle(Some(99), &keys));
			assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66, 99]);
			assert_eq!(window.joins().count(), 0);
		}

		// An answer of nothing places a join with an end past it, and keeps one without waiting.
		keys.answered = Some(3);
		keys.join = Some(Join::Empty);
		let join = |end| [(0, live(Duration::from_millis(40), end))];
		let mut bounded = Window::keyed_since(join(time(50)), |_| Joining::Since(3), Some(99), &keys);
		let mut open = Window::keyed_since(join(None), |_| Joining::Since(3), Some(99), &keys);
		keys.answered = Some(4);
		assert!(bounded.settle(Some(99), &keys));
		assert_eq!(covered(&bounded, [0, 33, 66, 99]), Vec::<u64>::new());
		assert!(bounded.is_settled());
		assert!(!open.settle(Some(99), &keys));
		assert!(open.is_reaching());
	}

	/// A live join already placed starts at that group on any hop, asking nothing and waiting
	/// on nothing, and its groups still go stale past its `max`.
	#[test]
	fn a_placed_join_starts_where_it_was_placed() {
		let mut keys = Keys::new([0, 33, 66, 99]);
		keys.join = Some(Join::Group(66));
		keys.answered = Some(3);
		let max = Duration::from_secs(2);
		let join = Range::from(Start::Live {
			min: Some(Duration::from_millis(40)),
			max: Some(max),
		});
		let placed = |_| Joining::At(33);
		for window in [
			Window::keyed_since([(0, join)], placed, Some(99), &keys),
			Window::keyed_since([(0, join)], placed, Some(99), &Blind(&keys)),
			Window::keyed_since([(0, join)], placed, None, &Unheld),
		] {
			assert!(window.is_settled(), "{window:?}");
			assert_eq!(window.joins().count(), 0);
			assert_eq!(window.start(), Some(33));
			let budget = window.budget();
			assert_eq!((budget.at(66), budget.widest()), (max, max));
		}
		let window = Window::keyed_since([(0, join)], placed, Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66, 99]);

		// Placed elsewhere by an edit, it moves there; retuned in place, it stays.
		let mut window = Window::keyed_since([(0, join)], placed, Some(99), &keys);
		window.edit_since([0], &[(0, join)], |_| Joining::At(66), Some(99), &keys);
		assert_eq!(window.start(), Some(66));
		let wider = Range::from(Start::Live {
			min: Some(Duration::from_millis(40)),
			max: None,
		});
		window.edit_since([0], &[(0, wider)], |_| Joining::At(66), Some(99), &keys);
		assert_eq!((window.start(), window.budget().at(66)), (Some(66), Duration::MAX));
	}

	/// A group takes the widest budget among the ranges covering it, not among every range: a
	/// finite live join alone keeps its `max` beside an unlimited one placed later, and a join
	/// still reaching may cover any group up to its end.
	#[test]
	fn a_group_takes_the_widest_budget_covering_it() {
		let keys = Keys {
			answered: Some(0),
			..Keys::new(0..100)
		};
		let (narrow, reach) = (Duration::from_millis(100), Duration::from_millis(500));
		let join = |min, max, end| Range {
			start: Start::Live {
				min: Some(Duration::from_millis(min)),
				max,
			},
			end,
		};
		let ranges: BTreeMap<u64, Range> = [
			(0, join(40, Some(narrow), None)),
			(1, join(20, None, Some(59))),
			(2, range(time(80), time(89))),
			(3, join(60, Some(reach), Some(20))),
		]
		.into();
		let placed = |key| match key {
			0 => Joining::At(10),
			1 => Joining::At(50),
			_ => Joining::default(),
		};
		let window = Window::keyed_since(ranges.clone(), placed, Some(99), &keys);
		assert!(window.is_reaching());
		for budget in [window.budget(), window.budget_of(&ranges)] {
			assert_eq!(
				budget.runs(0..100),
				vec![
					(0..21, reach),
					(21..50, narrow),
					(50..60, Duration::MAX),
					(60..80, narrow),
					(80..90, Duration::MAX),
					(90..100, narrow),
				],
				"{budget:?}"
			);
			assert_eq!(budget.stretch(30), (narrow, Some(50)));
			assert_eq!((budget.widest(), budget.is_uniform()), (Duration::MAX, false));
			// A group none covers gets the widest budget, which is unlimited, so all are judged.
			assert_eq!(budget.floor(Some(70)), 0);
			let clamped = budget.clamped(Some(Duration::from_millis(200)));
			assert_eq!(clamped.at(55), Duration::from_millis(200));
			assert_eq!(clamped.at(30), narrow);
		}
	}

	/// A group no range covers, such as one handed out before the ranges moved off it, takes the
	/// widest budget any range asks: none beside a history range, and the widest `max` beside
	/// live joins alone.
	#[test]
	fn a_group_none_covers_takes_the_widest_budget() {
		let keys = Keys::new(0..10);
		let max = Duration::from_millis(30);
		// Placed at the latest group, 9.
		let join = Range::from(Start::Live {
			min: Some(Duration::ZERO),
			max: Some(max),
		});
		let history = range(time(3), time(3));
		for (ranges, rest) in [
			(vec![history], Duration::MAX),
			(vec![history, join], Duration::MAX),
			(vec![join], max),
		] {
			let window = Window::new(&ranges, Some(9), &keys);
			let keyed: BTreeMap<u64, Range> = (0..).zip(ranges.iter().copied()).collect();
			for budget in [window.budget(), window.budget_of(&keyed)] {
				assert!(window.is_settled() && !window.contains(1), "{window:?}");
				assert_eq!((budget.at(1), budget.widest()), (rest, rest), "{ranges:?}");
			}
		}
	}

	/// A history range a hop holding nothing leaves to its copies keeps every group it may
	/// cover from going stale before they place it, beside a live join judged by its `max`:
	/// a group handed out meanwhile is history's until then.
	#[test]
	fn a_pending_history_range_keeps_what_it_may_cover() {
		let history = Range {
			start: Start::Time(5001),
			end: Some(8999),
		};
		let max = Duration::from_secs(1);
		let join = Range::from(Start::Live {
			min: Some(Duration::ZERO),
			max: Some(max),
		});
		let window = Window::new(&[history, join], None, &Unheld);
		assert!(window.is_pending());
		let ranges: BTreeMap<u64, Range> = [(0, history), (1, join)].into();
		for budget in [window.budget(), window.budget_of(&ranges)] {
			assert_eq!(budget.at(8000), Duration::MAX, "{budget:?}");
			assert_eq!(budget.at(0), Duration::MAX, "the copies may place it at any group");
			assert_eq!(budget.at(9000), max, "past its end");
		}
	}

	/// A hop whose copies place its ranges leaves a live join to them, reading everything
	/// meanwhile, and a range with no start reaches back as it would anywhere.
	#[test]
	fn a_hop_holding_nothing_defers_a_join_to_its_copies() {
		let window = Window::new(&[live(Duration::ZERO, None)], Some(9), &Unheld);
		assert!(window.is_pending() && window.contains(3));
		let window = Window::new(&[Range::ALL], Some(9), &Unheld);
		assert!(!window.is_pending() && window.contains(3));
		assert_eq!(window.start(), None);
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let keys = Keys::new(0..10);
		let joins = live(Duration::ZERO, None);
		let start = |ranges: &[Range], latest| Window::new(ranges, latest, &keys).start();
		assert_eq!(start(&[range(time(4), None), joins], Some(9)), Some(4));
		assert_eq!(start(&[range(time(4), None), joins], Some(2)), Some(2));
		assert_eq!(start(&[range(time(0), None)], None), Some(0));
		assert_eq!(start(&[range(time(4), None), joins], None), None);
		assert_eq!(start(&[range(time(4), None), Range::ALL], Some(9)), None);
	}

	/// A time start with no group held yet reads only the groups from its time, rather than
	/// guess the latest group, and resolves once a group arrives. A track that can receive
	/// nothing more reaches back to the oldest group.
	#[test]
	fn a_time_start_waits_for_a_group() {
		let mut keys = Keys::default();
		// The other joins at the latest group, 9, and ends there.
		let ranges = [range(time(2500), None), live(Duration::ZERO, time(9))];
		let mut window = Window::new(&ranges, Some(9), &keys);
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
		let window = Window::new(&ranges, Some(3000), &keys);
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
		let window = Window::new(&ranges, Some(9), &keys);
		assert_eq!(
			window.resolved()[0],
			Some(Resolved {
				start: 0,
				end: None,
				floored: false
			})
		);
	}

	/// A coverage check resumes where the last stopped, but not below its floor or past its
	/// cap: a lower floor or a wider cap reaches groups no check has seen.
	#[test]
	fn coverage_resumes_within_what_was_checked() {
		let keys = Keys::new(0..6);
		let ranges = [range(time(0), time(1)), range(time(4), time(5))];
		let mut window = Window::new(&ranges, None, &keys);
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
		let mut window = Window::new(&[range(time(0), time(7))], None, &Keys::new(0..8));
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
		let mut window = Window::new(&ranges, None, &Keys::new(0..6));
		let mut accounted = rangemap::RangeMap::new();
		accounted.insert(0..2, 'a');
		accounted.insert(4..5, 'b');
		assert!(!window.covered_by(0, None, &accounted), "group 5 is owed");
		accounted.insert(5..6, 'a');
		assert!(window.covered_by(0, None, &accounted));
	}

	/// With a handler serving the history, a time start the groups held can't place asks a
	/// fill for its group from past the newest held below it, and reads nothing meanwhile:
	/// before every start, or with a group the cache evicted possibly starting between the
	/// newest held at or before it and the time. The answer places it: a drop from 0 says no
	/// group starts at or before it, so it reads from the time on.
	#[test]
	fn a_time_start_the_groups_cant_place_is_looked_up() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(8500), None)], Some(9000), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(8500, 4001)]);
		assert!(!window.is_settled() && !window.contains(9000) && window.resolved() == [None]);
		keys.held.insert(8000);
		assert!(window.found(8500, Found::Group(8000), &keys));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 8000,
				end: None,
				floored: true
			})]
		);
		assert!(window.is_settled());

		let window = Window::new(&[range(time(8500), None)], Some(9000), &keys);
		assert_eq!(window.lookups().count(), 1, "something may start between 8000 and 8500");
		let window = Window::new(&[range(time(8000), None)], Some(9000), &keys);
		assert_eq!(window.lookups().count(), 0, "8000 starts exactly there");

		let mut window = Window::new(&[range(time(500), None)], Some(9000), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(500, 0)]);
		assert!(window.found(500, Found::Dropped { floor: 0 }, &keys));
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 500,
				end: None,
				floored: true
			})]
		);
		assert!(window.contains(4000) && !window.missable(0).any(|run| *run.start() < 500));
		// A group below the time held since is none of the answer's.
		keys.held.insert(200);
		assert!(!window.contains(200));
	}

	/// A dropped lookup starts at the group its floor was past, which then holds the time,
	/// whatever the cache holds by the answer: that group evicted, or another arrived between.
	#[test]
	fn a_dropped_lookup_starts_below_its_floor() {
		let history = |held: &[u64]| Keys {
			looks_up: true,
			..Keys::new(held.iter().copied())
		};
		// 4000 evicted while the lookup was out, or 8000 arrived between, as another reader's
		// fill brings it.
		for held in [&[9000][..], &[4000, 8000, 9000]] {
			let mut window = Window::new(&[range(time(8500), None)], Some(9000), &history(&[4000, 9000]));
			assert_eq!(window.lookups().collect::<Vec<_>>(), [(8500, 4001)]);
			assert!(window.found(8500, Found::Dropped { floor: 4001 }, &history(held)));
			assert_eq!(window.lookups().count(), 0, "answered, not looked up again");
			assert_eq!(
				window.resolved(),
				[Some(Resolved {
					start: 4000,
					end: None,
					floored: true
				})]
			);
		}
	}

	/// The group a lookup's floor was past evicted while it was out, a lookup left unanswered
	/// looks the time up again from below once a handler is back, where the history may hold
	/// the group containing it, rather than starting where the groups still held say.
	#[test]
	fn an_eviction_during_a_lookup_looks_it_up_again() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(8500), None)], Some(9000), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(8500, 4001)]);
		keys.held.remove(&4000);
		assert!(window.found(8500, Found::Unanswered, &keys));
		assert_eq!(
			window.lookups().collect::<Vec<_>>(),
			[(8500, 0)],
			"looked up from below"
		);
		assert!(!window.contains(9000), "reads nothing meanwhile");
		keys.held.insert(4000);
		assert!(window.found(8500, Found::Group(4000), &keys));
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(4000));

		// Even with the cache emptied.
		let mut window = Window::new(&[range(time(8500), None)], Some(9000), &keys);
		let all = std::mem::take(&mut keys.held);
		assert!(window.found(8500, Found::Unanswered, &keys));
		assert_eq!(
			window.lookups().collect::<Vec<_>>(),
			[(8500, 0)],
			"looked up from below"
		);
		keys.held = all;

		// With nothing evicted, it starts where the groups held say.
		let mut window = Window::new(&[range(time(8500), None)], Some(9000), &keys);
		assert!(window.found(8500, Found::Unanswered, &keys));
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(4000));
	}

	/// Keys the producer skipped hold no group, even in the history, so a start in a skipped
	/// span between two held groups is placed without a lookup.
	#[test]
	fn a_skipped_gap_needs_no_lookup() {
		let mut keys = Keys::new([0, 33, 66]);
		keys.looks_up = true;
		keys.skipped = vec![1..33, 34..66];
		let window = Window::new(&[range(time(50), None)], Some(66), &keys);
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(33));
	}

	/// A time end on a start the history couldn't place ends where it says, since time ends
	/// name their groups by start.
	#[test]
	fn an_unanswered_lookup_settles_with_its_time_end() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(500), time(3000))], Some(9000), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(500, 0)]);
		assert!(window.found(500, Found::Unanswered, &keys));
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
		let window = Window::new(&[range(time(8500), None), range(time(500), None)], Some(9000), &keys);
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

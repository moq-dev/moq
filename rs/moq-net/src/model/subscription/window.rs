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

use super::{Cover, Delay, Range};

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
	/// Whether this hop resolves ranges against groups of its own, so a Min Age places a range
	/// with no start here. A hop whose copies resolve them leaves that to them.
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

	/// Where `time` falls among the groups held, for a reader whose [`Delay::max`] is `budget`:
	/// a received copy proves some placements only for a reader within the budget its upstream
	/// answered under.
	///
	/// [`Delay::max`]: crate::track::Delay::max
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

	/// Where a live join holding a nonzero `min` behind the live edge starts, for a reader
	/// within `budget`; see [`Join`].
	fn join(&self, min: Duration, budget: Duration) -> Join {
		let _ = (min, budget);
		Join::Unknown
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
	/// The group containing this time, the live edge less the Min Age, resolved as a range
	/// starting there is, history lookup included.
	Time(u64),
	/// The group starting here, which this hop measured by arrival, or its upstream reported.
	Group(u64),
	/// A group starting at or past this key, which an upstream answering a bounded join empty
	/// proved: a range ending before it holds nothing, and any other waits as for
	/// [`Self::Unknown`].
	Past(u64),
	/// Nothing measures it yet: a copy waits for its upstream to answer, and any hop for a
	/// group to measure the live edge by.
	Unknown,
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

/// What the track's history answered a time start's lookup with; see [`Window::found`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
	/// The group containing the time.
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

/// A subscriber's [`Range`]s resolved to the group starts it reads, each by the key its
/// caller gave it, so an edit resolves only the ranges it adds and leaves every other where
/// it resolved.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, by key, in the order given.
	ranges: BTreeMap<u64, Range>,
	/// The subscriber's Min Age, which reaches a live join back from the live edge.
	min: Option<Duration>,
	/// The subscriber's [`Delay::max`](crate::track::Delay::max) as a budget, which a received
	/// copy places some time starts only within.
	budget: Duration,
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
	/// The time it starts from: its range's start, or where a live join's Min Age reached
	/// it back to. `None` reaches back to the oldest group.
	anchor: Option<u64>,
	/// Whether the start is a time the track's history may be asked to place, rather than a
	/// group this hop already measured.
	asks: bool,
	start: u64,
	/// The exclusive end, `None` past every group.
	end: Option<u64>,
	/// Whether the start names a group, rather than reaching back to the oldest one.
	floored: bool,
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
	/// Resolve `ranges`, keyed by position, for a subscriber with `delay`, `latest` being the
	/// latest group: a range with no start reaches back by [`Delay::min`] from the live edge.
	pub fn new(ranges: &[Range], delay: Delay, latest: Option<u64>, holdings: &impl Holdings) -> Self {
		Self::keyed((0..).zip(ranges.iter().copied()), delay, latest, holdings)
	}

	/// [`Self::new`] for ranges the caller keys itself, in key order.
	pub fn keyed(
		ranges: impl IntoIterator<Item = (u64, Range)>,
		delay: Delay,
		latest: Option<u64>,
		holdings: &impl Holdings,
	) -> Self {
		let mut window = Self {
			ranges: ranges.into_iter().collect(),
			min: delay.min,
			budget: delay.budget(),
			spans: BTreeMap::new(),
			pending: BTreeSet::new(),
			looking: BTreeSet::new(),
			waiting: BTreeSet::new(),
			waited: None,
			reaching: BTreeSet::new(),
			groups: Cover::default(),
			missable: Cover::default(),
			covered: None,
			named: BTreeMap::new(),
			unnamed: 0,
		};
		window.place(latest, holdings);
		window
	}

	/// Retire the ranges keyed `retired`, then resolve `added`, leaving every other range
	/// where it resolved. A key added again replaces its range. Costs the ranges edited, never
	/// the rest.
	pub fn edit(
		&mut self,
		retired: impl IntoIterator<Item = u64>,
		added: &[(u64, Range)],
		latest: Option<u64>,
		holdings: &impl Holdings,
	) {
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
	}

	/// Take out the range keyed `key`, if any.
	fn retire(&mut self, key: u64) {
		if self.ranges.remove(&key).is_none() {
			return;
		}
		self.pending.remove(&key);
		self.reaching.remove(&key);
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

	/// Take the subscriber's current [`Delay`]. Only a range still to place reads it, so one
	/// already started stays where it started.
	pub fn set_delay(&mut self, delay: Delay) {
		self.min = delay.min;
		self.budget = delay.budget();
	}

	/// The time starts still waiting on the groups held, with the budget they wait under: what
	/// a received copy asks its upstream to place.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn asks(&self) -> impl Iterator<Item = u64> + '_ {
		let mut last = None;
		self.waiting.iter().filter_map(move |&(time, _)| (last.replace(time) != Some(time)).then_some(time))
	}

	/// The budget [`Self::asks`] wait under.
	pub fn budget(&self) -> Duration {
		self.budget
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
			let (start, floored) = match holdings.containing(time, self.budget) {
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
		let mut span = self.spans.remove(&key).expect("a waiting span");
		self.uncount(key, &span);
		(span.start, span.floored, span.waiting) = (start, floored, false);
		if let Some(floor) = unresolved(time, start, floored, holdings).filter(|_| span.asks) {
			span.looking = Some(Looking { time, floor });
		}
		self.count(key, span);
	}

	/// Whether group `key` is in range. Everything is while a live join waits on a first group.
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
		match self.is_pending() || !self.reaching.is_empty() || self.unnamed > 0 {
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
		if let (Some(start), Some(_)) = (range.start, range.end)
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
				// A floor is one past the group held below the time, which then holds it; from 0,
				// no group starts at or before it, so the span reads from the time on.
				Found::Dropped { floor } => (floor.checked_sub(1).unwrap_or(time), true),
				Found::Unanswered => match holdings.containing(time, self.budget) {
					Containing::Group(group) => (group, true),
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
		self.spans.clear();
		self.pending.clear();
		self.looking.clear();
		self.waiting.clear();
		self.reaching.clear();
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
	}

	/// Resolve the non-empty range keyed `key` into a span, or note what it waits on.
	fn place_range(&mut self, key: u64, latest: Option<u64>, holdings: &impl Holdings) {
		let range = self.ranges[&key];
		// Every end includes its group.
		let end = range.end.and_then(|end| end.checked_add(1));
		let (anchor, asks) = match (range.start, self.min) {
			(Some(time), _) => (Some(time), true),
			// Unstated, it reaches back as far as the budget allows.
			(None, None) => (None, false),
			// A hop whose copies place the join waits on them, as one with no group yet does.
			(None, Some(_)) if holdings.defers() => {
				self.pending.insert(key);
				return;
			}
			// Placed already, where a copy resolved it: one left with no start reached back.
			(None, Some(_)) if !holdings.resolves() => (None, false),
			(None, Some(min)) => match latest {
				// Nothing to measure the live edge by yet: wait for the first group, reading
				// everything meanwhile.
				None => {
					self.pending.insert(key);
					return;
				}
				// The latest group holds the live edge. One made before any group then starts at
				// the first, which was the latest when it was placed.
				Some(latest) if min.is_zero() => (Some(latest), false),
				Some(_) => match holdings.join(min, self.budget) {
					Join::Time(time) => (Some(time), true),
					Join::Group(group) => (Some(group), false),
					// Starting past its end, the range holds nothing.
					Join::Past(past) if range.end.is_some_and(|last| last < past) => (Some(past), false),
					// Nothing to measure the live edge by yet, or a copy's upstream still to answer:
					// read none of the groups held until it resolves.
					Join::Past(_) | Join::Unknown => {
						self.reaching.insert(key);
						return;
					}
				},
			},
		};
		// A group a join measured starts there; a time is placed among the groups held.
		let placed = match (anchor, asks) {
			(Some(group), false) => Start::At(group, true),
			_ => start(anchor, latest, self.budget, holdings),
		};
		let (start, floored) = match placed {
			Start::At(start, floored) => (start, floored),
			Start::Pending => {
				self.pending.insert(key);
				return;
			}
			// Reads only the groups from its time until the groups held place it; see
			// [`Self::start_waiting`].
			Start::Waiting => {
				let span = Span::new(anchor, asks, 0, end, false, None);
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
		self.count(key, Span::new(anchor, asks, start, end, floored, looking));
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
		self.anchor.filter(|_| self.waiting)
	}

	fn new(
		anchor: Option<u64>,
		asks: bool,
		start: u64,
		end: Option<u64>,
		floored: bool,
		looking: Option<Looking>,
	) -> Self {
		Self {
			anchor,
			asks,
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

fn start(anchor: Option<u64>, latest: Option<u64>, budget: Duration, holdings: &impl Holdings) -> Start {
	match anchor {
		// Nothing sits below group 0, so the oldest start needs no latest group.
		None => Start::At(0, false),
		// No group starts before 0, so time 0 names group 0 whatever is held.
		Some(0) => Start::At(0, true),
		Some(time) => match holdings.containing(time, budget) {
			Containing::Group(group) => Start::At(group, true),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Start::At(0, false),
			// Before any group, which a hop that holds none (such as a reader following route
			// copies) leaves to the copies to resolve.
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

/// The oldest key a fill of the group containing time start `time`, resolved to `start`,
/// asks from, when the groups held can't tell which group contains it and a handler may: it
/// is before every group held, or a group the cache evicted may start after `start`, the
/// newest held at or before it, and still at or before `time`. Keys the producer skipped
/// hold none.
fn unresolved(time: u64, start: u64, floored: bool, holdings: &impl Holdings) -> Option<u64> {
	if !holdings.looks_up() {
		return None;
	}
	if !floored {
		return holdings.next_start(0).map(|_| 0);
	}
	let next = start.checked_add(1)?;
	let after = holdings.next_start(next)?;
	let gap = next..after.min(time.saturating_add(1));
	(!holdings.never_produced(gap)).then_some(next)
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeSet;

	/// A subscriber's delay stating `min`, with no budget.
	fn min_delay(min: Option<Duration>) -> Delay {
		Delay::default().with_max(None).with_min(min)
	}

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
		/// Where a live join with a nonzero Min Age starts, `None` for nowhere yet.
		join: Option<Join>,
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
				runs: [unknown, Unplaced::NONE, Unplaced::NONE, Unplaced::NONE],
				mark: 0,
			})
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			self.held.range(from..).next().copied()
		}

		fn join(&self, _: Duration, _: Duration) -> Join {
			self.join.unwrap_or(Join::Unknown)
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

		fn join(&self, min: Duration, budget: Duration) -> Join {
			self.0.join(min, budget)
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
			min in proptest::option::of(0u64..3),
			join in proptest::option::of(0u64..48),
		) {
			let to_range = |(joins, start, end): (bool, u64, Option<u64>)| {
				let start = match joins {
					true => None,
					false => time(start),
				};
				range(start, end)
			};
			let ranges: Vec<Range> = ranges.into_iter().map(to_range).collect();
			let mut next_key = ranges.len() as u64;
			let mut keys = Keys {
				looks_up,
				join: join.map(Join::Time),
				..Keys::default()
			};
			let min = min.map(Duration::from_millis);
			let mut fast = Window::new(&ranges, min_delay(min), None, &keys);
			let mut full = Window::new(&ranges, min_delay(min), None, &Blind(&keys));
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
			}
		}
	}

	fn range(start: Option<u64>, end: impl Into<Option<u64>>) -> Range {
		Range { start, end: end.into() }
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
		let window = Window::new(&ranges, min_delay(None), Some(9), &keys);
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
		let window = Window::new(&[], min_delay(None), None, &keys);
		assert_eq!(covered(&window, 0..4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// A time start names the group containing it, the newest starting at or before it, and
	/// a time end includes every group starting at or before it: sparse starts need no walk.
	#[test]
	fn time_anchors_name_groups_by_their_starts() {
		let keys = Keys::new([0, 33, 66, 99]);
		let window = Window::new(&[range(time(50), time(70))], min_delay(None), Some(99), &keys);
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
		let window = Window::new(&[range(time(33), time(66))], min_delay(None), Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![33, 66]);

		// An end before its start is empty, and so is one ending before every group.
		let window = Window::new(&[range(time(66), time(33))], min_delay(None), Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), Vec::<u64>::new());

		// Before every start held: reach back to the group containing it, naming no floor.
		let keys = Keys::new([10, 20]);
		let window = Window::new(&[range(time(5), None)], min_delay(None), Some(20), &keys);
		assert_eq!(covered(&window, [10, 20]), vec![10, 20]);
		assert_eq!((window.floor(), window.start(), window.named()), (Some(0), None, None));
		assert_eq!(window.missable(0).next(), Some(5..=u64::MAX), "nothing below its time");
	}

	/// A range with no start reaches back to the oldest group while the Min Age is unstated,
	/// and joins at the latest group with a Min Age of zero, waiting for a first group when
	/// there is none.
	#[test]
	fn a_range_with_no_start_joins_by_its_min_age() {
		let keys = Keys::new([0, 33, 66, 99]);
		let oldest = Window::new(&[Range::default()], min_delay(None), Some(99), &keys);
		assert_eq!(covered(&oldest, [0, 33, 66, 99]), vec![0, 33, 66, 99]);
		assert_eq!((oldest.floor(), oldest.start()), (Some(0), None));

		let live = Window::new(&[Range::default()], min_delay(Some(Duration::ZERO)), Some(99), &keys);
		assert_eq!(covered(&live, [0, 33, 66, 99]), vec![99]);
		assert_eq!(live.start(), Some(99));
		// An end before where it joins holds nothing.
		let short = Window::new(&[range(None, time(50))], min_delay(Some(Duration::ZERO)), Some(99), &keys);
		assert_eq!(covered(&short, [0, 33, 66, 99]), Vec::<u64>::new());
		// A range with a start ignores the Min Age.
		let timed = Window::new(&[range(time(40), None)], min_delay(Some(Duration::ZERO)), Some(99), &keys);
		assert_eq!(timed.start(), Some(33));

		let mut window = Window::new(&[Range::default()], min_delay(Some(Duration::ZERO)), None, &Keys::default());
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Keys::default()));
		let keys = Keys::new([5]);
		assert!(window.settle(Some(5), &keys));
		assert_eq!(covered(&window, 0..8), vec![5, 6, 7]);
		assert!(window.is_settled());
	}

	/// A nonzero Min Age places the join where the holdings measure it: a time resolves as a
	/// time start, history lookup included, and a group starts there. Until they measure it
	/// the join reads nothing, and it resolves once: a later Min Age moves nothing.
	#[test]
	fn a_live_join_starts_where_the_holdings_place_it() {
		let min = Some(Duration::from_millis(40));
		let mut keys = Keys::new([0, 33, 66, 99]);
		keys.join = Some(Join::Time(59));
		let window = Window::new(&[Range::default()], min_delay(min), Some(99), &keys);
		assert_eq!(
			window.resolved(),
			[Some(Resolved {
				start: 33,
				end: None,
				floored: true
			})]
		);
		keys.join = Some(Join::Group(66));
		let window = Window::new(&[Range::default()], min_delay(min), Some(99), &keys);
		assert_eq!(covered(&window, [0, 33, 66, 99]), vec![66, 99]);

		// A time the groups held can't place is looked up, as a time start is.
		keys.looks_up = true;
		keys.join = Some(Join::Time(20));
		keys.held.remove(&0);
		let window = Window::new(&[Range::default()], min_delay(min), Some(99), &keys);
		assert_eq!(window.lookups().collect::<Vec<_>>(), [(20, 0)]);

		keys.looks_up = false;
		keys.join = None;
		let mut window = Window::new(&[Range::default()], min_delay(min), Some(99), &keys);
		assert!(window.is_reaching() && !window.contains(99) && window.resolved() == [None]);
		assert!(!window.settle(Some(99), &keys), "still nothing to measure it by");
		keys.join = Some(Join::Group(66));
		assert!(window.settle(Some(99), &keys));
		assert!(!window.is_reaching());
		assert_eq!(window.start(), Some(66));
		keys.join = Some(Join::Group(99));
		window.set_delay(min_delay(Some(Duration::from_millis(1))));
		assert!(!window.settle(Some(99), &keys), "resolved already");
		assert_eq!(window.start(), Some(66));
	}

	/// A hop whose copies place its ranges leaves a live join to them, reading everything
	/// meanwhile, and one with an unstated Min Age reaches back as it would anywhere.
	#[test]
	fn a_hop_holding_nothing_defers_a_join_to_its_copies() {
		let window = Window::new(&[Range::default()], min_delay(Some(Duration::ZERO)), Some(9), &Unheld);
		assert!(window.is_pending() && window.contains(3));
		let window = Window::new(&[Range::default()], min_delay(None), Some(9), &Unheld);
		assert!(!window.is_pending() && window.contains(3));
		assert_eq!(window.start(), None);
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let keys = Keys::new(0..10);
		let live = Some(Duration::ZERO);
		let start = |ranges: &[Range], min, latest| Window::new(ranges, min_delay(min), latest, &keys).start();
		assert_eq!(start(&[range(time(4), None), Range::default()], live, Some(9)), Some(4));
		assert_eq!(start(&[range(time(4), None), Range::default()], live, Some(2)), Some(2));
		assert_eq!(start(&[range(time(0), None)], None, None), Some(0));
		assert_eq!(start(&[range(time(4), None), Range::default()], live, None), None);
		assert_eq!(start(&[range(time(4), None), Range::default()], None, Some(9)), None);
	}

	/// A time start with no group held yet reads only the groups from its time, rather than
	/// guess the latest group, and resolves once a group arrives. A track that can receive
	/// nothing more reaches back to the oldest group.
	#[test]
	fn a_time_start_waits_for_a_group() {
		let mut keys = Keys::default();
		// The other joins at the latest group, 9, and ends there.
		let ranges = [range(time(2500), None), range(None, time(9))];
		let live = Some(Duration::ZERO);
		let mut window = Window::new(&ranges, min_delay(live), Some(9), &keys);
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
		let window = Window::new(&ranges, min_delay(live), Some(3000), &keys);
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
		let window = Window::new(&ranges, min_delay(live), Some(9), &keys);
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
		let mut window = Window::new(&ranges, min_delay(None), None, &keys);
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
		let mut window = Window::new(&[range(time(0), time(7))], min_delay(None), None, &Keys::new(0..8));
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
		let mut window = Window::new(&ranges, min_delay(None), None, &Keys::new(0..6));
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
		let mut window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &keys);
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

		let window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &keys);
		assert_eq!(window.lookups().count(), 1, "something may start between 8000 and 8500");
		let window = Window::new(&[range(time(8000), None)], min_delay(None), Some(9000), &keys);
		assert_eq!(window.lookups().count(), 0, "8000 starts exactly there");

		let mut window = Window::new(&[range(time(500), None)], min_delay(None), Some(9000), &keys);
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
			let mut window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &history(&[4000, 9000]));
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
		let mut window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &keys);
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
		let mut window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &keys);
		let all = std::mem::take(&mut keys.held);
		assert!(window.found(8500, Found::Unanswered, &keys));
		assert_eq!(
			window.lookups().collect::<Vec<_>>(),
			[(8500, 0)],
			"looked up from below"
		);
		keys.held = all;

		// With nothing evicted, it starts where the groups held say.
		let mut window = Window::new(&[range(time(8500), None)], min_delay(None), Some(9000), &keys);
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
		let window = Window::new(&[range(time(50), None)], min_delay(None), Some(66), &keys);
		assert_eq!(window.lookups().count(), 0);
		assert_eq!(window.resolved()[0].map(|resolved| resolved.start), Some(33));
	}

	/// A time end on a start the history couldn't place ends where it says, since time ends
	/// name their groups by start.
	#[test]
	fn an_unanswered_lookup_settles_with_its_time_end() {
		let mut keys = Keys::new([4000, 9000]);
		keys.looks_up = true;
		let mut window = Window::new(&[range(time(500), time(3000))], min_delay(None), Some(9000), &keys);
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
		let window = Window::new(
			&[range(time(8500), None), range(time(500), None)],
			min_delay(None),
			Some(9000),
			&keys,
		);
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

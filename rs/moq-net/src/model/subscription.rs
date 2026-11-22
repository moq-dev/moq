use std::{
	ops::{Bound, RangeBounds},
	time::Duration,
};

mod cover;
mod window;
pub(super) use cover::{Cover, minus};
pub(crate) use window::Accounted;
pub(crate) use window::Resolved;
pub(super) use window::{Budget, Containing, Found, Holdings, Join, Unheld, Unplaced, Window};

/// Subscriber-side preferences for receiving a track.
///
/// Each subscriber holds its own [`Subscription`]; the publisher observes an
/// aggregate across all live subscribers via [`crate::track::Producer::subscription`].
/// A subscriber can change its preferences after the fact with
/// [`crate::track::Subscriber::update`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Subscription {
	/// Delivery priority, higher first, defaulting to 0. Higher values preempt lower ones
	/// when bandwidth is constrained.
	pub priority: i8,
	/// The spans of the track to deliver; see [`Range`].
	///
	/// The default is one range from [`Start::LATEST`]: the latest group, skipping each the
	/// moment a newer one arrives. An empty list asks for nothing.
	///
	/// A request to the publisher, aggregated across every live subscriber as the union of
	/// their ranges, and this subscriber's read filter: it reads only the groups its own
	/// ranges cover, whatever another subscriber widens the union to.
	/// [`crate::track::Subscriber::with_range`] narrows the reads further. See [Local cursor
	/// vs wire preference](crate::track::Subscriber#local-cursor-vs-wire-preference).
	pub ranges: Vec<Range>,
	/// Which pending group the publisher sends first; see [`Order`].
	///
	/// Aggregated across every live subscriber: any [`Order::Descending`] subscriber makes
	/// the aggregate descending, so a relay fanning one upstream request out keeps its live
	/// subscribers fresh. Each subscriber's own publisher (a relay's downstream session)
	/// still serves it in its own order.
	pub order: Order,
	/// Whether this reader needs the source's live feed, not only its history: a wire
	/// publisher owes its peer an answer naming the live edge, which only the upstream's own
	/// answer gives. A session holding no upstream subscription for its copy subscribes again
	/// for such a reader, whatever its ranges. Aggregated as any: one such reader is enough.
	pub(crate) feed: bool,
	/// The frames this reader already holds of the group a range starts at, so that range asks
	/// for it from the frame after them rather than whole: a route switch resuming on a new
	/// copy. Only a range whose start is exactly that group's start uses
	/// it. Aggregated only when every reader resumes: the lowest position then.
	pub(crate) resume: Option<Position>,
	/// The time starts a received copy's readers wait on: the copy can't prove where the
	/// publisher would start them, so its session asks its upstream with a range of its own
	/// from each. Only an aggregate holds any.
	pub(crate) asks: Vec<u64>,
	/// The live joins a received copy's readers wait on: the copy can't prove where the
	/// publisher would start them, so its session asks its upstream with a range of its own
	/// for each that no unanswered range answers. Only an aggregate holds any.
	pub(crate) joins: Vec<JoinAsk>,
}

/// A live join a received copy's readers wait on, by its [`Start::Live`] lag and its end, with
/// the widest staleness budget one waits with and the newest answer any of them saw before it
/// began waiting: only an answer recorded after that is fresh for every one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JoinAsk {
	pub min: Duration,
	pub end: Option<u64>,
	/// The widest [`Start::Live`] `max` among the readers waiting, as a budget.
	pub budget: Duration,
	pub since: u64,
}

impl JoinAsk {
	/// The widest [`Start::Live`] `max` among the readers waiting.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn max(&self) -> Option<Duration> {
		max_of(self.budget)
	}
}

/// A [`Start::Live`] `max` as a budget: `None` waits as long as anything could.
pub(crate) fn budget(max: Option<Duration>) -> Duration {
	max.unwrap_or(Duration::MAX)
}

/// A budget as a [`Start::Live`] `max`: `Duration::MAX` is no limit.
pub(crate) fn max_of(budget: Duration) -> Option<Duration> {
	Some(budget).filter(|budget| *budget != Duration::MAX)
}

/// Which of a subscription's pending groups the publisher sends first.
///
/// A transmission order only: it never decides *whether* a group is delivered. A history
/// range ([`Start::Time`] or [`Start::Oldest`]) delivers every group it covers, and a
/// [`Start::Live`] range skips what its `max` judges stale, so an ascending subscriber that
/// wants a backlog drains it from a history range or a `max` that covers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Order {
	/// Oldest first, draining a backlog in key order.
	Ascending,
	/// Newest first, so congestion sheds the backlog rather than the live edge.
	#[default]
	Descending,
}

/// A span of a track to deliver: from where [`Self::start`] places it through the group
/// containing `end`, a time in the track's timescale (a bare start on a scale-less track).
///
/// Every range names whole groups: a subscription always starts at a group's first frame,
/// since a group is the unit of decodability, and an end includes its group. A range from
/// [`Start::Time`] 2 through 6 delivers every group from the one containing time 2 through the
/// one containing time 6. An end before a time start is empty.
///
/// Only a group reached through a [`Start::Live`] range alone can go stale. A group several
/// ranges cover takes the largest `max` among them, and never goes stale while any history
/// range covers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Range {
	/// Where delivery starts; see [`Start`].
	pub start: Start,
	/// Where delivery stops, including the group containing this time, or `None` to keep
	/// following the live edge.
	pub end: Option<u64>,
}

/// Where a [`Range`] starts.
///
/// This is the `Start` Anchor on the moq-lite wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Start {
	/// The oldest group the publisher still holds: a floor for history, whose groups are
	/// delivered in full, bounded only by how long the publisher keeps them
	/// ([`Info::max_age`](crate::track::Info::max_age)).
	Oldest,
	/// The group containing this time: the newest one starting at or before it. A floor for
	/// history, like [`Self::Oldest`]: its groups never go stale.
	///
	/// A time resolves at the hop that serves it, from the starts of the groups it holds. When
	/// none does, a hop whose [`Requests`](crate::track::Requests) handler serves its history
	/// asks it for the group containing the time (see
	/// [`FillGroups::floor`](crate::track::FillGroups::floor)), reading nothing until it
	/// answers; otherwise, or when the handler can't tell, it reaches back to the oldest group.
	/// While no group is held at all it waits for one, reading nothing of its range meanwhile.
	/// A relay's copy that can't tell which group contains a time asks its upstream and waits
	/// for the answer.
	Time(u64),
	/// A live join `min` behind the live edge, whose groups go stale `max` behind it.
	///
	/// Carried in whole milliseconds, rounded up. moq-lite before 07 and moq-transport cannot
	/// carry it, so it goes out as [`Self::Oldest`] there, under the subscription-wide max age
	/// those wires carry instead.
	Live {
		/// How far behind the live edge to join: the group containing the live edge minus this
		/// lag, so a player's buffer arrives full instead of filling in real time, and zero
		/// starts at the latest group.
		///
		/// The hop serving it measures the live edge in media time, from the newest frame it
		/// holds, rounded up to whole ticks, and on a scale-less track by arrival on its own
		/// clock. It resolves that time as a [`Self::Time`] start would, so a publisher looks it
		/// up in its history; `max` does not move it. A relay's copy forwards each join to its
		/// upstream as its own range and starts it where that range's answer does. A range
		/// resolves once, so a live edge that moves later moves nothing already started.
		min: Duration,
		/// How long a superseded group stays worth delivering, measured from the live edge, or
		/// `None` for as long as the publisher keeps it.
		///
		/// `Some(Duration::ZERO)` skips immediately: group 8 arriving means group 7 is
		/// abandoned, timed or not. A larger budget tolerates that much reordering before
		/// giving up. This never *adds* delay, since the bound is only reached once newer data
		/// is already that far ahead. Read in order, `None` waits at a missing group until it
		/// resolves; see [`track::Ordered`](crate::track::Ordered) for where that can block.
		/// A `max` below `min` skips the join's own oldest groups.
		///
		/// Changing only `max` on a range already live, with
		/// [`Subscriber::update`](crate::track::Subscriber::update), keeps where it joined,
		/// both here and upstream. Encoded as milliseconds in a QUIC varint, so a duration of
		/// `2^62` milliseconds or more goes out as no limit. Clamped to the publisher's
		/// [`Info::max_age`](crate::track::Info::max_age), since waiting for a group longer
		/// than it is kept around cannot produce it.
		///
		/// # Where it is enforced
		///
		/// At both ends, and neither alone is enough. The publisher skips a group that has
		/// aged out instead of putting it on the wire, which bounds a backlog before it
		/// costs bandwidth. But what reaches the publisher is the aggregate across every
		/// subscriber, resolved in favor of the most tolerant one, so that gate is only ever
		/// as tight as the most patient viewer. The subscriber applies its own budget again
		/// as it reads, where only its own ranges are in play.
		///
		/// This bounds a *subscription*.
		/// [`track::Consumer::fetch_group`](crate::track::Consumer::fetch_group) is exempt:
		/// it names one old group explicitly, so there is no live edge to be late against.
		///
		/// # How staleness is measured
		///
		/// By two clocks, and a group is stale once either reaches the budget. Both start at
		/// its immediate servable successor, so the newest group is never stale.
		///
		/// In presentation time, a group is measured by its *reach*, where its successor
		/// begins, against the newest frame of the latest group: it cannot present past its
		/// successor, so once everything it could still hold falls outside the budget it is
		/// provably useless. The candidate needs no timestamp of its own, so an empty or
		/// stalled group is bounded by its stamped successor the same way.
		///
		/// In wall-clock time, a group is measured by how long ago its successor arrived at
		/// this hop. That bounds what media time can't: a peer whose timestamps stop
		/// advancing, a successor that never presents a frame, and an untimed track
		/// (pre-Lite05 moq-lite, or moq-transport without TIMESCALE), which is judged by the
		/// wall clock alone. A read waiting in a superseded group wakes at that deadline only
		/// when something collects the track's cache pool: an origin's driver does, and a
		/// standalone track needs [`Pool::poll`](crate::cache::Pool::poll) called, as its idle
		/// expiry already does. Otherwise it is judged only when something else wakes the read.
		max: Option<Duration>,
	},
}

impl Start {
	/// The latest group, skipping each the moment a newer one arrives: a live viewer holding
	/// no buffer.
	pub const LATEST: Self = Self::Live {
		min: Duration::ZERO,
		max: Some(Duration::ZERO),
	};

	/// Whether this start is a floor for history rather than a live join, so the groups its
	/// range covers never go stale.
	pub(crate) fn is_history(&self) -> bool {
		!matches!(self, Self::Live { .. })
	}
}

/// Every group from where the start places it, following the live edge.
impl From<Start> for Range {
	fn from(start: Start) -> Self {
		Self { start, end: None }
	}
}

impl Range {
	/// Whether this range certainly delivers nothing, wherever the latest group lands.
	pub(crate) fn is_empty(&self) -> bool {
		matches!((self.start, self.end), (Start::Time(start), Some(end)) if end < start)
	}

	/// The time this range starts from, `None` when it doesn't name one.
	pub(crate) fn time(&self) -> Option<u64> {
		match self.start {
			Start::Time(time) => Some(time),
			Start::Oldest | Start::Live { .. } => None,
		}
	}

	/// The staleness budget of the groups this range covers: its `max` for a live join, and
	/// none for history.
	pub(crate) fn budget(&self) -> Duration {
		match self.start {
			Start::Live { max, .. } => budget(max),
			Start::Oldest | Start::Time(_) => Duration::MAX,
		}
	}

	/// This range as a live join of the same lag and end, which a change of `max` alone
	/// keeps: the key a join is placed and answered under.
	pub(crate) fn join(&self) -> Option<(Duration, Option<u64>)> {
		match self.start {
			Start::Live { min, .. } => Some((min, self.end)),
			Start::Oldest | Start::Time(_) => None,
		}
	}

	/// Whether this range follows the live edge from no time: a live join, or one reaching
	/// back from the oldest group, with no end.
	pub(crate) fn is_open(&self) -> bool {
		self.time().is_none() && self.end.is_none()
	}

	/// A range holding nothing: nothing sorts below group 0, so an exclusive end there holds
	/// nothing, whatever the start.
	pub(crate) const EMPTY: Self = Self {
		start: Start::Time(1),
		end: Some(0),
	};

	/// Every group the publisher holds, following the live edge.
	pub const ALL: Self = Self {
		start: Start::Oldest,
		end: None,
	};

	/// The range from group `start` up to the exclusive group `end`, or onward for `None`:
	/// what a legacy wire's bounds become. `None` when it holds nothing, which an exclusive
	/// end of 0 says whatever the start.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn span(start: Option<u64>, end: Option<u64>) -> Option<Self> {
		let end = match end {
			Some(end) => Some(end.checked_sub(1)?),
			None => None,
		};
		let start = start.map_or(Start::Oldest, Start::Time);
		let range = Self { start, end };
		(!range.is_empty()).then_some(range)
	}
}

/// A live join's lag in whole milliseconds, rounded up, as the wire carries it, so a start is
/// never newer than the lag allows.
pub(crate) fn live_millis(min: Duration) -> u64 {
	u64::try_from(min.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// The union of `ranges` in a canonical order, without empty or redundant ranges: the
/// reference for the track's incremental union.
///
/// Time spans merge where they overlap or touch, as the group starts they cover. The rest
/// can't be placed without the live edge, so only an open range drops what it certainly
/// covers: the open time span drops ranges starting inside it, and a range from the oldest
/// group following the live edge covers everything. A live join reaches no further back than
/// its lag, so it covers nothing else, and live joins of the same lag and end are one range
/// under the largest `max` among them: where a join lands doesn't depend on it.
#[cfg(test)]
pub(super) fn union(ranges: impl IntoIterator<Item = Range>) -> Vec<Range> {
	let ranges: Vec<Range> = ranges.into_iter().filter(|range| !range.is_empty()).collect();
	if ranges.contains(&Range::ALL) {
		return vec![Range::ALL];
	}
	let mut joins = std::collections::BTreeMap::<(Duration, Option<u64>), Duration>::new();
	for range in &ranges {
		if let Some(join) = range.join() {
			let widest = joins.entry(join).or_default();
			*widest = (*widest).max(range.budget());
		}
	}

	// An end at the last start covers every group, as an open one does.
	let mut spans: Vec<(u64, Option<u64>)> = ranges
		.iter()
		.filter_map(|range| Some((range.time()?, range.end.and_then(|end| end.checked_add(1)))))
		.collect();
	spans.sort_unstable();
	let mut runs: Vec<(u64, Option<u64>)> = Vec::new();
	for (start, end) in spans {
		match runs.last_mut() {
			Some(last) if last.1.is_none_or(|last| start <= last) => {
				last.1 = last.1.zip(end).map(|(last, end)| last.max(end));
			}
			_ => runs.push((start, end)),
		}
	}

	let mut union: Vec<Range> = runs
		.into_iter()
		.map(|(start, end)| Range {
			start: Start::Time(start),
			end: end.map(|end| end - 1),
		})
		.collect();
	// A range with no time start can't be placed against the time spans.
	union.extend(ranges.iter().filter(|range| range.start == Start::Oldest));
	union.extend(joins.into_iter().map(|((min, end), budget)| Range {
		start: Start::Live { min, max: max_of(budget) },
		end,
	}));
	union.sort_unstable();
	union.dedup();
	union
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ranges: vec![Range::from(Start::LATEST)],
			order: Order::Descending,
			feed: false,
			resume: None,
			asks: Vec::new(),
			joins: Vec::new(),
		}
	}
}

impl Subscription {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// The one `Subscriber Max Age` a wire before lite-07 carries for this subscription: no
	/// limit while a history range is asked for, and otherwise the largest live join's `max`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn max_age(&self) -> Option<Duration> {
		max_of(self.widest())
	}

	/// The widest staleness budget any range gives a group: none while a history range is
	/// asked for, and otherwise the largest live join's `max`.
	pub(crate) fn widest(&self) -> Duration {
		(self.ranges.iter())
			.filter(|range| !range.is_empty())
			.map(Range::budget)
			.max()
			.unwrap_or(Duration::ZERO)
	}

	/// Replace [`Self::ranges`], returning `self` for chaining.
	pub fn with_ranges(mut self, ranges: impl IntoIterator<Item = Range>) -> Self {
		self.ranges = ranges.into_iter().collect();
		self
	}

	/// Set which pending group the publisher sends first, returning `self` for chaining.
	pub fn with_order(mut self, order: Order) -> Self {
		self.order = order;
		self
	}

	/// The lowest start any range names, or `None` when one has no start (or none is asked
	/// for).
	///
	/// The form a wire without ranges carries: a floor below which nothing is requested.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn start(&self) -> Option<u64> {
		let mut floor: Option<u64> = None;
		for range in self.ranges.iter().filter(|range| !range.is_empty()) {
			let start = range.time()?;
			floor = Some(floor.map_or(start, |floor| floor.min(start)));
		}
		floor
	}

	/// The exclusive end past every range, or `None` when one is open. An empty list, or one
	/// of empty ranges, ends at group 0.
	///
	/// The form a wire without ranges carries, alongside [`Self::start`].
	#[cfg(any(feature = "async", test))]
	pub(crate) fn end(&self) -> Option<u64> {
		let mut end = Some(0);
		for range in self.ranges.iter().filter(|range| !range.is_empty()) {
			end = end.max(Some(range.end?.checked_add(1)?));
		}
		end
	}

	/// Replace the ranges with one from `start` (or no start) up to [`Self::end`], returning
	/// `self` for chaining: the form a wire without ranges carries.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn with_start(self, start: impl Into<Option<u64>>) -> Self {
		let end = self.end();
		self.with_span(start.into(), end)
	}

	/// Replace the ranges with one from [`Self::start`] up to the exclusive `end`, or onward
	/// for `None`, returning `self` for chaining: the form a wire without ranges carries.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn with_end(self, end: impl Into<Option<u64>>) -> Self {
		let start = self.start();
		self.with_span(start, end.into())
	}

	#[cfg(any(feature = "async", test))]
	fn with_span(self, start: Option<u64>, end: Option<u64>) -> Self {
		self.with_ranges(Range::span(start, end))
	}
}

impl Range {
	/// This range raised to start no earlier than `floor` and stop before `end`, the
	/// exclusive end, without ever widening it.
	///
	/// `resolved` names where it resolved, so a start becomes its own group, raised to `floor`,
	/// and an end capped at `end`. A start not resolved keeps its time, since the floor may lie
	/// before it, so the caller drops what it resolves to below `floor`; an end not resolved
	/// becomes the bound.
	pub(super) fn clamped(&self, floor: Option<u64>, end: Option<u64>, resolved: Option<Resolved>) -> Self {
		let resolved = resolved.filter(|_| floor.is_some() || end.is_some());
		let start = match resolved {
			Some(resolved) => Start::Time(resolved.start),
			None => self.start,
		};
		let start = match (start, floor) {
			(Start::Time(start), Some(floor)) => Start::Time(start.max(floor)),
			(start, _) => start,
		};
		let requested = match (self.end, resolved.and_then(|resolved| resolved.end)) {
			(Some(_), Some(resolved)) => match resolved.checked_sub(1) {
				Some(last) => Some(last),
				None => return Self::EMPTY,
			},
			(requested, _) => requested,
		};
		let end = match (requested, end.map(|end| end.checked_sub(1))) {
			(requested, None) => requested,
			(_, Some(None)) => return Self::EMPTY,
			(Some(requested), Some(Some(last))) => Some(requested.min(last)),
			(_, Some(Some(last))) => Some(last),
		};
		Self { start, end }
	}
}

/// Group spans for unit tests whose groups start at consecutive keys, where a span of
/// starts counts groups.
#[cfg(test)]
impl Range {
	/// The groups starting in `range`.
	pub(crate) fn groups(range: impl RangeBounds<u64>) -> Self {
		let start = match range.start_bound() {
			Bound::Included(start) => Some(*start),
			Bound::Excluded(start) => Some(start.saturating_add(1)),
			Bound::Unbounded => None,
		};
		let end = match range.end_bound() {
			Bound::Included(end) => end.checked_add(1),
			Bound::Excluded(end) => Some(*end),
			Bound::Unbounded => None,
		};
		Self::span(start, end).unwrap_or(Self::EMPTY)
	}
}

#[cfg(test)]
impl Subscription {
	/// These preferences reading only the groups starting in `range`.
	pub(crate) fn with_range(self, range: impl RangeBounds<u64>) -> Self {
		self.with_ranges([Range::groups(range)])
	}

	/// These preferences under a staleness budget the way a subscription-wide max age used to
	/// apply one: [`Start::LATEST`] and [`Start::Oldest`] reach back as far as `max` and go
	/// stale past it, or take every group for `None`; another live join keeps its lag and
	/// takes `max`; a time start is history either way.
	pub(crate) fn with_max(mut self, max: impl Into<Option<Duration>>) -> Self {
		let max = max.into();
		for range in &mut self.ranges {
			range.start = match range.start {
				Start::Oldest | Start::LATEST => match max {
					Some(max) => Start::Live { min: max, max: Some(max) },
					None => Start::Oldest,
				},
				Start::Live { min, .. } => Start::Live { min, max },
				Start::Time(time) => Start::Time(time),
			};
		}
		self
	}
}

/// The most demanding request across subscribers, folded in one at a time: the reference
/// the track's counted aggregate is checked against.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Combined {
	sub: Option<Subscription>,
}

#[cfg(test)]
impl Combined {
	/// Fold in one subscriber's preferences.
	pub(super) fn add(&mut self, sub: &Subscription) {
		let Some(combined) = &mut self.sub else {
			self.sub = Some(sub.clone());
			return;
		};
		combined.priority = combined.priority.max(sub.priority);
		combined.ranges.extend_from_slice(&sub.ranges);
		// Key-first prioritization is enabled only when every subscriber wants it.
		if let Order::Descending = sub.order {
			combined.order = Order::Descending;
		}
		combined.feed |= sub.feed;
		combined.resume = combined.resume.zip(sub.resume).map(|(a, b)| a.min(b));
	}

	/// The aggregate, or `None` without a subscriber.
	pub(super) fn finish(self) -> Option<Subscription> {
		let mut combined = self.sub?;
		combined.ranges = union(std::mem::take(&mut combined.ranges));
		Some(combined)
	}
}

/// A frame-precise point in a track: a group, by its start, and a frame index within it.
///
/// Ordered lexicographically, so comparing positions is the same as comparing groups
/// and only falling back to frames within one. This is the model's counterpart of the
/// wire's (`Group`, `Frame`) pairs on SUBSCRIBE and FETCH.
///
/// Pairing the two is the point: a frame index counts from the start of a group, so it
/// means nothing on its own. Carrying them together makes "frame 5 of nothing"
/// unrepresentable rather than merely undefined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct Position {
	/// Where the group starts, in its track's timescale.
	pub group: u64,
	/// The frame index within the group, numbered from 0 in write order.
	pub frame: u64,
}

impl Position {
	/// Frame `frame` of the group starting at `group`.
	pub fn new(group: u64, frame: u64) -> Self {
		Self { group, frame }
	}

	/// The first frame of `group`.
	///
	/// As an exclusive end this means "everything before `group`"; as a start it means
	/// "`group` from the beginning".
	pub fn group(group: u64) -> Self {
		Self { group, frame: 0 }
	}

	/// The first group this exclusive end excludes whole, `None` past the last group: the
	/// boundary a wire that cannot name a frame carries as the track's end.
	pub(crate) fn final_key(self) -> Option<u64> {
		self.group.checked_add(u64::from(self.frame != 0))
	}
}

/// The first index an end bound withholds, or `None` for no cap: the form cursors store
/// and compare with [`before_end`]. An inclusive bound at `u64::MAX` has nothing above it,
/// so it is no cap at all.
fn exclusive_end(bound: Bound<&u64>) -> Option<u64> {
	match bound {
		Bound::Included(index) => index.checked_add(1),
		Bound::Excluded(&index) => Some(index),
		Bound::Unbounded => None,
	}
}

// Normalize discrete ranges once, including the empty range above the last index.
pub(super) fn key_bounds(range: impl RangeBounds<u64>) -> (u64, Option<u64>) {
	let start = match range.start_bound() {
		Bound::Included(&start) => start,
		Bound::Excluded(&start) => match start.checked_add(1) {
			Some(start) => start,
			None => return (u64::MAX, Some(u64::MAX)),
		},
		Bound::Unbounded => 0,
	};
	(start, exclusive_end(range.end_bound()))
}

/// Whether `key` is strictly below an exclusive cap. `None` is unbounded.
pub(super) fn before_end(key: u64, end: Option<u64>) -> bool {
	end.is_none_or(|end| key < end)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn combine(subscriptions: &[Subscription]) -> Option<Subscription> {
		let mut combined = Combined::default();
		for sub in subscriptions {
			combined.add(sub);
		}
		combined.finish()
	}

	/// Groups from `start` up to the exclusive `end`, or onward for `None`.
	fn groups(start: u64, end: impl Into<Option<u64>>) -> Range {
		Range {
			start: Start::Time(start),
			end: end.into().map(|end| end - 1),
		}
	}

	fn range(start: impl Into<Option<u64>>, end: impl Into<Option<u64>>) -> Range {
		Range {
			start: start.into().map_or(Start::Oldest, Start::Time),
			end: end.into(),
		}
	}

	fn ranges(subscriptions: &[Subscription]) -> Vec<Range> {
		combine(subscriptions).unwrap().ranges
	}

	fn joining(min: Duration) -> Subscription {
		Subscription::default().with_ranges([Range::from(Start::Live { min, max: Some(std::time::Duration::ZERO) })])
	}

	/// A legacy span becomes inclusive times: an exclusive end of 0 holds nothing, whatever
	/// the start, and an end of 1 holds group 0.
	#[test]
	fn legacy_spans_become_inclusive_time_ranges() {
		assert_eq!(Range::span(Some(2), Some(6)), Some(range(2, 5)));
		assert_eq!(Range::span(None, Some(6)), Some(range(None, 5)));
		assert_eq!(Range::span(Some(2), None), Some(range(2, None)));
		assert_eq!(Range::span(None, None), Some(Range::ALL));
		assert_eq!(Range::span(Some(0), Some(0)), None);
		assert_eq!(Range::span(None, Some(0)), None);
		assert_eq!(Range::span(Some(5), Some(5)), None);
		assert_eq!(Range::span(Some(0), Some(1)), Some(range(0, 0)));
		assert!(Range::EMPTY.is_empty());
	}

	#[test]
	fn cursor_bounds_are_total_at_the_extremes() {
		// A cursor cap is the first index it withholds; an inclusive bound at the last
		// index withholds nothing.
		assert_eq!(key_bounds(..0), (0, Some(0)));
		assert_eq!(key_bounds(..=5), (0, Some(6)));
		assert_eq!(key_bounds(..=u64::MAX), (0, None));
		assert_eq!(key_bounds(..), (0, None));
		assert_eq!(key_bounds((Bound::Unbounded, Bound::Included(5))), key_bounds(..6));
	}

	/// The bounds a wire without ranges carries: the lowest start and the highest exclusive
	/// end, either one lost to a range without it.
	#[test]
	fn bounds_project_the_ranges_for_older_wires() {
		let sub = Subscription::default();
		assert_eq!((sub.start(), sub.end()), (None, None));

		let sub = Subscription::default().with_ranges([groups(2, 4), groups(7, 9)]);
		assert_eq!((sub.start(), sub.end()), (Some(2), Some(9)));

		// An explicit start at 0 is a floor, unlike the default.
		let sub = Subscription::default().with_ranges([groups(0, None)]);
		assert_eq!((sub.start(), sub.end()), (Some(0), None));

		let sub = Subscription::default().with_ranges([groups(2, 4), Range::ALL]);
		assert_eq!((sub.start(), sub.end()), (None, None));

		// Nothing asked for is the empty range.
		let sub = Subscription::default().with_ranges([]);
		assert_eq!(sub.end(), Some(0));

		let sub = Subscription::default().with_start(5).with_end(10);
		assert_eq!((sub.start(), sub.end()), (Some(5), Some(10)));
		assert_eq!(sub.with_start(None).with_end(None), Subscription::default());
	}

	/// The aggregate is the union: overlapping and adjacent time ranges merge, an open range
	/// absorbs what starts inside it, and a range with no start absorbs everything only while
	/// it reaches back as far as the budget allows.
	#[test]
	fn combined_ranges_are_the_union() {
		let sub = |ranges: &[Range]| Subscription::default().with_ranges(ranges.iter().copied());

		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), sub(&[groups(4, 8)])]),
			vec![groups(2, 8)]
		);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), sub(&[groups(5, 8)])]),
			vec![groups(2, 8)],
			"adjacent ranges merge"
		);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), sub(&[groups(6, 8)])]),
			vec![groups(2, 5), groups(6, 8)],
			"a gap stays a gap"
		);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5), groups(9, None)]), sub(&[groups(4, None)])]),
			vec![groups(2, None)]
		);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), Subscription::default()]),
			vec![Range::ALL]
		);

		// A live join reaches back only by its lag, so the history beside it stays asked, and
		// each lag is a range of its own.
		let two = Duration::from_secs(2);
		let five = Duration::from_secs(5);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), joining(two), joining(five), joining(two)]),
			vec![
				groups(2, 5),
				Range::from(Start::Live { min: two, max: Some(std::time::Duration::ZERO) }),
				Range::from(Start::Live { min: five, max: Some(std::time::Duration::ZERO) })
			]
		);
		// Unless another reader starts at the oldest group.
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), joining(two), Subscription::default()]),
			vec![Range::ALL]
		);

		// Empty ranges ask for nothing.
		assert_eq!(ranges(&[sub(&[groups(5, 5)])]), Vec::<Range>::new());
		assert_eq!(ranges(&[sub(&[]), sub(&[groups(1, 2)])]), vec![groups(1, 2)]);
	}

	/// Combining is order-independent and stops changing once it covers everyone.
	#[test]
	fn combined_ranges_settle() {
		let a = Subscription::default().with_ranges([groups(2, 5), Range::ALL]);
		let b = Subscription::default().with_ranges([groups(4, 9)]);
		assert_eq!(combine(&[a.clone(), b.clone()]), combine(&[b.clone(), a.clone()]));

		let combined = combine(&[a.clone(), b]);
		assert_eq!(
			combine(&[combined.clone().unwrap(), a]),
			combined,
			"a subset adds nothing"
		);
	}

	#[test]
	fn combined_order_prefers_descending() {
		let ascending = Subscription::default().with_order(Order::Ascending);
		let descending = Subscription::default();

		assert_eq!(
			combine(std::slice::from_ref(&ascending)).unwrap().order,
			Order::Ascending
		);
		assert_eq!(
			combine(&[ascending.clone(), ascending.clone()]).unwrap().order,
			Order::Ascending
		);
		assert_eq!(
			combine(&[ascending.clone(), descending.clone()]).unwrap().order,
			Order::Descending
		);
		assert_eq!(combine(&[descending, ascending]).unwrap().order, Order::Descending);
	}
}

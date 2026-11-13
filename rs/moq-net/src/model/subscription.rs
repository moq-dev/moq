use std::{
	ops::{Bound, RangeBounds},
	time::Duration,
};

mod runs;
mod window;
pub(super) use runs::{Layered, Runs};
pub(crate) use window::Accounted;
pub(crate) use window::Resolved;
pub(super) use window::{Containing, Holdings, Unheld, Window};

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
	/// How far behind the live edge this subscriber plays; see [`Delay`].
	pub delay: Delay,
	/// The spans of the track to deliver; see [`Range`].
	///
	/// The default is [`Range::oldest`]: everything [`Delay::max`] still considers
	/// fresh. [`Range::live`] starts at the latest group instead, and an empty list asks
	/// for nothing.
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
}

/// How far behind the live edge a subscriber plays: where it starts, and how late it may fall.
///
/// Both default to zero, which starts at the newest group and skips a superseded one at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delay {
	/// The target latency, the playout or jitter-buffer delay held behind the live edge, or
	/// `None` when the subscriber did not state one.
	///
	/// Only a peer whose wire cannot carry it leaves it unstated: moq-lite before 07 and
	/// moq-transport. A media consumer reads an unstated minimum as zero. The aggregate
	/// across subscribers is the smallest stated one, so an unstated minimum never pulls it
	/// to zero, and a relay forwards it upstream as is. A publisher may use it, such as to
	/// start a cold join that far back; nothing reads it there yet.
	///
	/// This is the `Subscriber Min Age` on the moq-lite-07 wire, in milliseconds.
	pub min: Option<Duration>,
	/// How long a superseded group stays worth delivering to this subscriber, or `None`
	/// for as long as the publisher keeps it.
	///
	/// `Some(Duration::ZERO)` (the default) skips immediately: group 8 arriving means group
	/// 7 is abandoned, timed or not. A larger budget tolerates that much reordering before
	/// giving up. This never *adds* delay, since the bound is only reached once newer data
	/// is already that far ahead. `None` is the publisher's
	/// [`Info::max_age`](crate::track::Info::max_age) alone, what a reader reaching back
	/// for history wants. Read in order, `None` waits at a missing group until it resolves;
	/// see [`track::Ordered`](crate::track::Ordered) for where that can block.
	///
	/// This is the `Subscriber Max Age` on the wire, and it is stored here verbatim so
	/// what was asked for stays readable. Encoded as milliseconds in a QUIC varint, so
	/// a duration of `2^62` milliseconds or more cannot be put on the wire; `None` goes out
	/// as the largest value every peer can read. Clamped to the publisher's
	/// [`Info::max_age`](crate::track::Info::max_age), since waiting for a group longer
	/// than it is kept around cannot produce it.
	///
	/// It caps every one of [`Subscription::ranges`]: a range names what may be delivered, and this
	/// budget skips what inside it has gone stale.
	///
	/// # Where it is enforced
	///
	/// At both ends, and neither alone is enough. The publisher skips a group that has
	/// aged out instead of putting it on the wire, which bounds a backlog before it
	/// costs bandwidth. But what reaches the publisher is the aggregate across every
	/// subscriber, resolved in favor of the most tolerant one, so that gate is only ever
	/// as tight as the most patient viewer. The subscriber applies the same budget again
	/// as it reads, where its own is the only one in play.
	///
	/// This bounds a *subscription*.
	/// [`track::Consumer::fetch_group`](crate::track::Consumer::fetch_group) is exempt:
	/// it names one old group explicitly, so there is no live edge to be late against.
	///
	/// # How delay is measured
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
	pub max: Option<Duration>,
}

impl Default for Delay {
	fn default() -> Self {
		Self {
			min: Some(Duration::ZERO),
			max: Some(Duration::ZERO),
		}
	}
}

impl Delay {
	/// Set [`Self::min`], returning `self` for chaining.
	pub fn with_min(mut self, min: impl Into<Option<Duration>>) -> Self {
		self.min = min.into();
		self
	}

	/// Set [`Self::max`], returning `self` for chaining.
	pub fn with_max(mut self, max: impl Into<Option<Duration>>) -> Self {
		self.max = max.into();
		self
	}

	/// [`Self::max`] as a budget: `None` waits as long as anything could.
	pub(crate) fn budget(&self) -> Duration {
		self.max.unwrap_or(Duration::MAX)
	}
}

/// Which of a subscription's pending groups the publisher sends first.
///
/// A transmission order only: it never decides *whether* a group is delivered, which is
/// [`Delay::max`]'s job. An ascending subscriber that wants a backlog
/// therefore needs a budget that covers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Order {
	/// Oldest first, draining a backlog in key order.
	Ascending,
	/// Newest first, so congestion sheds the backlog rather than the live edge.
	#[default]
	Descending,
}

/// Where one end of a [`Range`] falls: a count of groups back from the latest one, the
/// oldest group, or a time, which names the group containing it.
///
/// Every anchor names a whole group: a subscription always starts at a group's first frame,
/// since a group is the unit of decodability, and every end includes its group.
///
/// A relative or time anchor resolves at the hop that serves it, when it starts serving
/// and again whenever the subscriber replaces its ranges. Before any group exists a
/// relative anchor resolves at the first one, so [`Range::live`] starts there. A relay's
/// copy waiting on a fresh upstream answer counts from the live feed's latest group once
/// it answers, never from the groups an ended feed left in its cache.
///
/// Over moq-lite-07, a copy resolves relative anchors from the reported live edge. A Rust
/// moq-transport copy resolves a relative anchor from the Largest Object its live join
/// reports. Older moq-lite versions accept time and [`Anchor::Oldest`] starts but refuse
/// relative anchors other than [`Range::live`]'s with
/// [`Error::Unsupported`](crate::Error::Unsupported) rather than guess from a partial copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Anchor {
	/// The group this many before the latest, counting the groups the serving hop holds:
	/// `Relative(0)` is the latest group. One counting back past every group held reaches
	/// back to the oldest as a start, and ends before them all as an end.
	Relative(u64),
	/// The oldest group still around, as far back as [`Delay::max`] allows, naming no floor.
	///
	/// A start only: [`Range::oldest`] is the range starting here. An end here is refused
	/// with [`Error::InvalidRange`](crate::Error::InvalidRange), since no hop can tell
	/// which group is the oldest still around anywhere upstream.
	Oldest,
	/// The group containing this time, in the track's timescale: the newest one starting at
	/// or before it. Groups are keyed by their start, so a group's start names exactly it.
	///
	/// Resolved from the starts of the groups the serving hop holds. When none starts at
	/// or before it, a hop whose [`Requests`](crate::track::Requests) handler serves its
	/// history looks the start up there (see [`Fill::time`](crate::track::Fill::time)),
	/// reading nothing until it answers; otherwise, or when the handler can't tell, a start
	/// reaches back to the oldest group. While no group is held at all it waits for one,
	/// reading nothing of its range meanwhile. An end includes every group starting at or
	/// before it.
	///
	/// On a scale-less track (no [`Info::timescale`](crate::track::Info::timescale)) the
	/// time is a bare start, in the same units as the group starts.
	Time(u64),
}

/// A span of a track to deliver: from `start` through `end`, or onward when `end` is `None`.
///
/// Both ends include their group: `Time(2)..=Time(6)` delivers every group starting from the
/// one containing time 2 through the one containing time 6, and `Oldest..=Relative(0)`
/// delivers through the latest group. A time end before its time start is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Range {
	/// Where delivery starts.
	pub start: Anchor,
	/// Where delivery stops, including its group, or `None` to keep following the live edge.
	pub end: Option<Anchor>,
}

impl Range {
	/// Everything from the latest group onward.
	pub fn live() -> Self {
		Self {
			start: Anchor::Relative(0),
			end: None,
		}
	}

	/// Everything from the oldest group still around onward, the default.
	///
	/// Unlike `Time(0)..` this names no floor: a wire whose publisher resolves the start
	/// from [`Delay::max`] reaches as far back as it allows, and an older wire
	/// starts at the live edge, filling nothing below it.
	pub fn oldest() -> Self {
		Self {
			start: Anchor::Oldest,
			end: None,
		}
	}

	/// Whether a wire without ranges carries this range upstream: time anchors, as group
	/// IDs, and the live and oldest starts it spells with no floor.
	pub(super) fn is_wire(&self) -> bool {
		let start = match self.start {
			Anchor::Oldest | Anchor::Time(_) | Anchor::Relative(0) => true,
			Anchor::Relative(_) => false,
		};
		start && matches!(self.end, None | Some(Anchor::Time(_)))
	}

	/// Whether this range certainly delivers nothing, wherever the latest group lands.
	///
	/// An [`Anchor::Oldest`] end delivers nothing either: every reader holding one is
	/// refused, so it never reaches the demand asked upstream.
	pub(super) fn is_empty(&self) -> bool {
		match (self.start, self.end) {
			(_, Some(Anchor::Oldest)) => true,
			(Anchor::Time(start), Some(Anchor::Time(end))) => end < start,
			_ => false,
		}
	}

	/// A range holding nothing: nothing sorts below group 0, so an exclusive end there holds
	/// nothing, whatever the start.
	pub(crate) const EMPTY: Self = Self {
		start: Anchor::Time(1),
		end: Some(Anchor::Time(0)),
	};

	/// The range from group `start` up to the exclusive group `end`, or onward for `None`:
	/// what a legacy wire's bounds become. `None` when it holds nothing, which an exclusive
	/// end of 0 says whatever the start.
	pub(crate) fn span(start: Option<u64>, end: Option<u64>) -> Option<Self> {
		let end = match end {
			Some(end) => Some(Anchor::Time(end.checked_sub(1)?)),
			None => None,
		};
		let range = Self {
			start: start.map_or(Anchor::Oldest, Anchor::Time),
			end,
		};
		(!range.is_empty()).then_some(range)
	}
}

/// The union of `ranges` in a canonical order, without empty or redundant ranges: the
/// reference for the track's incremental union.
///
/// Time spans merge where they overlap or touch, as the group starts they cover. The rest
/// can't be placed without the latest group, so only an open range drops what it certainly
/// covers: the open time span drops ranges starting inside it, and the farthest open
/// relative range drops relative ranges starting no farther back. [`Range::oldest`] covers
/// everything.
#[cfg(test)]
pub(super) fn union(ranges: impl IntoIterator<Item = Range>) -> Vec<Range> {
	let ranges: Vec<Range> = ranges.into_iter().filter(|range| !range.is_empty()).collect();
	if ranges.contains(&Range::oldest()) {
		return vec![Range::oldest()];
	}

	let mut spans: Vec<(u64, Option<u64>)> = ranges
		.iter()
		.filter_map(|range| match (range.start, range.end) {
			(Anchor::Time(start), None) => Some((start, None)),
			(Anchor::Time(start), Some(Anchor::Time(end))) => Some((start, end.checked_add(1))),
			_ => None,
		})
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
	let tail = runs.last().filter(|run| run.1.is_none()).map(|run| run.0);
	let farthest = ranges
		.iter()
		.filter_map(|range| match (range.start, range.end) {
			(Anchor::Relative(back), None) => Some(back),
			_ => None,
		})
		.max();

	let mut union: Vec<Range> = runs
		.into_iter()
		.map(|(start, end)| Range {
			start: Anchor::Time(start),
			end: end.map(|end| Anchor::Time(end - 1)),
		})
		.collect();
	union.extend(ranges.into_iter().filter(|range| match (range.start, range.end) {
		(Anchor::Time(_), None | Some(Anchor::Time(_))) | (Anchor::Relative(_), None) => false,
		(Anchor::Time(start), _) => tail.is_none_or(|tail| start < tail),
		(Anchor::Relative(back), _) => farthest.is_none_or(|farthest| back > farthest),
		// Nothing but the open oldest range, handled above, covers the oldest group.
		(Anchor::Oldest, _) => true,
	}));
	union.extend(farthest.map(|back| Range {
		start: Anchor::Relative(back),
		end: None,
	}));
	union.sort_unstable();
	union.dedup();
	union
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			delay: Delay::default(),
			ranges: vec![Range::oldest()],
			order: Order::Descending,
			feed: false,
		}
	}
}

impl Subscription {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// Set the [`Delay`], returning `self` for chaining.
	pub fn with_delay(mut self, delay: Delay) -> Self {
		self.delay = delay;
		self
	}

	/// [`Delay::max`] as a budget: `None` waits as long as anything could.
	pub(crate) fn budget(&self) -> Duration {
		self.delay.budget()
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

	/// The lowest group any range names by time, or `None` when one counts back from the
	/// latest group or reaches back to the oldest (or none is asked for).
	///
	/// The form a wire without ranges carries: a floor below which nothing is requested.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn start(&self) -> Option<u64> {
		let mut floor: Option<u64> = None;
		for range in &self.ranges {
			let Anchor::Time(start) = range.start else {
				return None;
			};
			floor = Some(floor.map_or(start, |floor| floor.min(start)));
		}
		floor
	}

	/// The exclusive end past every range, or `None` when one is open or counts back from
	/// the latest group. An empty list is the empty range, ending at group 0.
	///
	/// The form a wire without ranges carries, alongside [`Self::start`].
	#[cfg(any(feature = "async", test))]
	pub(crate) fn end(&self) -> Option<u64> {
		let mut end = Some(0);
		for range in &self.ranges {
			let Some(Anchor::Time(last)) = range.end else {
				return None;
			};
			end = end.max(Some(last.checked_add(1)?));
		}
		end
	}

	/// Replace the ranges with one from `start` (or [`Range::oldest`]'s) up to [`Self::end`],
	/// returning `self` for chaining: the form a wire without ranges carries.
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

	/// These preferences with every range raised to start no earlier than `floor` and
	/// stop before `end`, the exclusive end, without ever widening one.
	///
	/// `resolved` names where each range resolved, by index, so a relative or time anchor
	/// becomes its own group, raised to `floor` or capped at `end`. A start not resolved
	/// keeps its anchor, since the floor may lie before it, so the caller drops what it
	/// resolves to below `floor`; an end not resolved becomes the bound.
	pub(super) fn clamped(&self, floor: Option<u64>, end: Option<u64>, resolved: &[Option<Resolved>]) -> Self {
		let clamping = floor.is_some() || end.is_some();
		let ranges = self
			.ranges
			.iter()
			.enumerate()
			.map(|(index, range)| {
				let resolved = resolved.get(index).copied().flatten().filter(|_| clamping);
				let start = match (range.start, resolved) {
					(Anchor::Oldest | Anchor::Relative(_) | Anchor::Time(_), Some(resolved)) => {
						Anchor::Time(resolved.start)
					}
					(start, _) => start,
				};
				let start = match (start, floor) {
					(Anchor::Time(start), Some(floor)) => Anchor::Time(start.max(floor)),
					(start, _) => start,
				};
				let requested = match (range.end, resolved.and_then(|resolved| resolved.end)) {
					(Some(Anchor::Relative(_) | Anchor::Time(_)), Some(resolved)) => match resolved.checked_sub(1) {
						Some(last) => Some(Anchor::Time(last)),
						None => return Range::EMPTY,
					},
					(requested, _) => requested,
				};
				let end = match (requested, end.map(|end| end.checked_sub(1))) {
					(requested, None) => requested,
					(_, Some(None)) => return Range::EMPTY,
					(Some(Anchor::Time(requested)), Some(Some(last))) => Some(Anchor::Time(requested.min(last))),
					(_, Some(Some(last))) => Some(Anchor::Time(last)),
				};
				Range { start, end }
			})
			.collect();
		Self { ranges, ..self.clone() }
	}

	/// Whether a subscriber joins at the live edge: a range counting back from the latest
	/// group and following it. A fixed start resumes a known group instead, such as a route
	/// switch picking up where the reader stopped.
	pub(super) fn joins_live(&self) -> bool {
		self.ranges
			.iter()
			.any(|range| matches!(range.start, Anchor::Oldest | Anchor::Relative(_)) && range.end.is_none())
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
}

/// The most demanding request across subscribers, folded in one at a time: the reference
/// the track's counted aggregate is checked against.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Combined(Option<Subscription>);

#[cfg(test)]
impl Combined {
	/// Fold in one subscriber's preferences.
	pub(super) fn add(&mut self, sub: &Subscription) {
		let Some(combined) = &mut self.0 else {
			self.0 = Some(sub.clone());
			return;
		};
		combined.priority = combined.priority.max(sub.priority);
		combined.delay.max = match (combined.delay.max, sub.delay.max) {
			(Some(a), Some(b)) => Some(a.max(b)),
			_ => None,
		};
		combined.delay.min = match (combined.delay.min, sub.delay.min) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		};
		combined.ranges.extend_from_slice(&sub.ranges);
		// Key-first prioritization is enabled only when every subscriber wants it.
		if let Order::Descending = sub.order {
			combined.order = Order::Descending;
		}
		combined.feed |= sub.feed;
	}

	/// The aggregate, or `None` without a subscriber.
	pub(super) fn finish(self) -> Option<Subscription> {
		let mut combined = self.0?;
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

	/// The position just past `frame` of `group`: the exclusive end that includes it.
	///
	/// `None` when there is no such position, i.e. the very last frame of the very last
	/// group. That is past everything, which an open [`Range::end`] spells `None` too, so
	/// the two meanings line up.
	pub fn after(group: u64, frame: u64) -> Option<Self> {
		match frame.checked_add(1) {
			Some(frame) => Some(Self { group, frame }),
			// Past the last frame of a group is the head of the next one.
			None => Self::after_group(group),
		}
	}

	/// The position just past every frame of `group`: the exclusive end that includes
	/// the group whole.
	///
	/// `None` past the last group, for the reason given on [`Self::after`].
	pub fn after_group(group: u64) -> Option<Self> {
		Some(Self::group(group.checked_add(1)?))
	}

	/// The bound this exclusive end puts on a group cursor, for
	/// [`crate::track::Subscriber::with_range`].
	///
	/// A head-of-group end excludes its group. A mid-group end includes it, so a frame
	/// cap can apply within that group.
	pub fn group_end(self) -> Bound<u64> {
		if self.frame == 0 {
			Bound::Excluded(self.group)
		} else {
			Bound::Included(self.group)
		}
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

	fn time(start: u64) -> Anchor {
		Anchor::Time(start)
	}

	/// Groups from `start` up to the exclusive `end`, or onward for `None`.
	fn groups(start: u64, end: impl Into<Option<u64>>) -> Range {
		Range {
			start: Anchor::Time(start),
			end: end.into().map(|end| Anchor::Time(end - 1)),
		}
	}

	/// A hop holding every group from 0 through the latest, as contiguous starts do.
	struct Contiguous;

	impl Holdings for Contiguous {
		fn containing(&self, time: u64) -> Containing {
			Containing::Group(time)
		}

		fn next_start(&self, from: u64) -> Option<u64> {
			Some(from)
		}

		fn back(&self, latest: u64, back: u64) -> Option<u64> {
			latest.checked_sub(back)
		}

		fn complete(&self) -> bool {
			false
		}
	}

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn ranges(subscriptions: &[Subscription]) -> Vec<Range> {
		combine(subscriptions).unwrap().ranges
	}

	/// A legacy span becomes inclusive time anchors: an exclusive end of 0 holds nothing,
	/// whatever the start, and an end of 1 holds group 0.
	#[test]
	fn legacy_spans_become_inclusive_time_ranges() {
		assert_eq!(Range::span(Some(2), Some(6)), Some(range(time(2), time(5))));
		assert_eq!(Range::span(None, Some(6)), Some(range(Anchor::Oldest, time(5))));
		assert_eq!(Range::span(Some(2), None), Some(range(time(2), None)));
		assert_eq!(Range::span(None, None), Some(Range::oldest()));
		assert_eq!(Range::span(Some(0), Some(0)), None);
		assert_eq!(Range::span(None, Some(0)), None);
		assert_eq!(Range::span(Some(5), Some(5)), None);
		assert_eq!(Range::span(Some(0), Some(1)), Some(range(time(0), time(0))));
		assert!(Range::EMPTY.is_empty());
	}

	#[test]
	fn positions_are_total_at_the_extremes() {
		// Past the last frame of a group is the head of the next one, not a wider frame
		// in the same group.
		assert_eq!(Position::after(5, u64::MAX), Some(Position::group(6)));

		// Past everything has no position. An open range spells that `None` too, so the
		// meanings coincide and the group is included rather than dropped.
		assert_eq!(Position::after_group(u64::MAX), None);
		assert_eq!(Position::after(u64::MAX, u64::MAX), None);

		// A head-of-group end excludes that group; a mid-group end includes it.
		assert_eq!(Position::group(0).group_end(), Bound::Excluded(0));
		assert_eq!(Position::after_group(5).unwrap().group_end(), Bound::Excluded(6));
		assert_eq!(Position::after(5, 2).unwrap().group_end(), Bound::Included(5));

		// A cursor cap is the first index it withholds; an inclusive bound at the last
		// index withholds nothing.
		assert_eq!(key_bounds(..0), (0, Some(0)));
		assert_eq!(key_bounds(..=5), (0, Some(6)));
		assert_eq!(key_bounds(..=u64::MAX), (0, None));
		assert_eq!(key_bounds(..), (0, None));
		assert_eq!(
			key_bounds((Bound::Unbounded, Bound::Included(5))),
			key_bounds(..6)
		);
	}

	/// A relative start counts back from the latest group and clamps at group 0. Before any
	/// group exists only the farthest offset resolves, since it is group 0 regardless.
	#[test]
	fn relative_starts_resolve_against_the_latest_group() {
		let floor = |sub: &Subscription, latest| Window::new(&sub.ranges, latest, &Contiguous).unwrap().floor();

		let live = Subscription::default().with_ranges([Range::live()]);
		assert_eq!(floor(&live, Some(7)), Some(7));
		assert_eq!(floor(&live, None), None);

		let buffering = Subscription::default().with_ranges([range(Anchor::Relative(3), None)]);
		assert_eq!(floor(&buffering, Some(7)), Some(4));
		assert_eq!(floor(&buffering, Some(1)), Some(0), "an offset past the start clamps");

		assert_eq!(floor(&Subscription::default(), None), Some(0));

		// The lowest range wins, and one unresolved range leaves the whole floor unresolved.
		let mixed = Subscription::default().with_ranges([range(time(4), None), Range::live()]);
		assert_eq!(floor(&mixed, Some(3)), Some(3));
		assert_eq!(floor(&mixed, Some(9)), Some(4));
		assert_eq!(floor(&mixed, None), None);

		// Nothing asked for floors nowhere, and an empty range asks for nothing.
		assert_eq!(floor(&Subscription::default().with_ranges([]), None), Some(0));
		let empty = Subscription::default().with_ranges([groups(5, 5), groups(10, None)]);
		assert_eq!(floor(&empty, None), Some(10));
	}

	/// The bounds a wire without ranges carries: the lowest time start and the highest
	/// exclusive time end, either one lost to a range that counts back from the latest group.
	#[test]
	fn bounds_project_the_ranges_for_older_wires() {
		let sub = Subscription::default();
		assert_eq!((sub.start(), sub.end()), (None, None));

		let sub = Subscription::default().with_ranges([groups(2, 4), groups(7, 9)]);
		assert_eq!((sub.start(), sub.end()), (Some(2), Some(9)));

		// An explicit start at 0 is a floor, unlike the default.
		let sub = Subscription::default().with_ranges([groups(0, None)]);
		assert_eq!((sub.start(), sub.end()), (Some(0), None));

		let sub = Subscription::default().with_ranges([groups(2, 4), Range::live()]);
		assert_eq!((sub.start(), sub.end()), (None, None));

		let sub = Subscription::default().with_ranges([range(time(2), Anchor::Relative(0))]);
		assert_eq!((sub.start(), sub.end()), (Some(2), None));

		// Nothing asked for is the empty range.
		let sub = Subscription::default().with_ranges([]);
		assert_eq!(sub.end(), Some(0));

		let sub = Subscription::default().with_start(5).with_end(10);
		assert_eq!((sub.start(), sub.end()), (Some(5), Some(10)));
		assert_eq!(sub.with_start(None).with_end(None), Subscription::default());
	}

	/// The aggregate is the union: overlapping and adjacent time ranges merge, an open
	/// range absorbs what starts inside it, and the oldest range absorbs everything.
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
			ranges(&[
				sub(&[groups(2, 5), groups(9, None)]),
				sub(&[groups(4, None)])
			]),
			vec![groups(2, None)]
		);
		assert_eq!(
			ranges(&[sub(&[groups(2, 5)]), Subscription::default()]),
			vec![Range::oldest()]
		);

		// Relative ranges count back from the same latest group, so the widest open one covers
		// every narrower one, whatever its end.
		assert_eq!(
			ranges(&[
				sub(&[Range::live()]),
				sub(&[range(Anchor::Relative(3), None)]),
				sub(&[range(Anchor::Relative(1), Anchor::Relative(0))]),
			]),
			vec![range(Anchor::Relative(3), None)]
		);

		// A time floor above the live edge does not hide the live edge: both stay.
		assert_eq!(
			ranges(&[sub(&[groups(4, None)]), sub(&[Range::live()])]),
			vec![groups(4, None), Range::live()]
		);

		// Empty ranges ask for nothing.
		assert_eq!(ranges(&[sub(&[groups(5, 5)])]), Vec::<Range>::new());
		assert_eq!(
			ranges(&[sub(&[]), sub(&[groups(1, 2)])]),
			vec![groups(1, 2)]
		);
	}

	/// Combining is order-independent and stops changing once it covers everyone.
	#[test]
	fn combined_ranges_settle() {
		let a = Subscription::default().with_ranges([groups(2, 5), Range::live()]);
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

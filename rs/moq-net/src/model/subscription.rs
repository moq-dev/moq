use std::{
	ops::{Bound, RangeBounds},
	time::Duration,
};

use crate::Timestamp;

mod window;
pub(super) use window::{Containing, Held, Holdings, Resolved, Unheld, Window, later};

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
	/// How long a superseded group stays worth delivering to this subscriber, or `None`
	/// for as long as the publisher keeps it.
	///
	/// `Some(Duration::ZERO)` (the default) skips immediately: group 8 arriving means group
	/// 7 is abandoned, timed or not. A larger budget tolerates that much reordering before
	/// giving up. This never *adds* delay, since the bound is only reached once newer data
	/// is already that far ahead. `None` is the publisher's
	/// [`Info::max_age`](crate::track::Info::max_age) alone, what a reader reaching back
	/// for history wants.
	///
	/// This is the `Subscriber Max Age` on the wire, and it is stored here verbatim so
	/// what was asked for stays readable. Encoded as milliseconds in a QUIC varint, so
	/// a duration of `2^62` milliseconds or more cannot be put on the wire; `None` goes out
	/// as the largest value every peer can read. Clamped to the publisher's
	/// [`Info::max_age`](crate::track::Info::max_age), since waiting for a group longer
	/// than it is kept around cannot produce it.
	///
	/// It caps every one of [`Self::ranges`]: a range names what may be delivered, and this
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
	pub max_delay: Option<Duration>,
	/// The spans of the track to deliver; see [`Range`].
	///
	/// The default is [`Range::oldest`]: everything [`Self::max_delay`] still considers
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
}

/// Which of a subscription's pending groups the publisher sends first.
///
/// A transmission order only: it never decides *whether* a group is delivered, which is
/// [`Subscription::max_delay`]'s job. An ascending subscriber that wants a backlog
/// therefore needs a budget that covers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Order {
	/// Oldest first, draining a backlog in sequence order.
	Ascending,
	/// Newest first, so congestion sheds the backlog rather than the live edge.
	#[default]
	Descending,
}

/// Where one end of a [`Range`] falls: a fixed group, a count of groups back from the
/// latest one, or a presentation time.
///
/// Every anchor names a whole group: a subscription always starts at a group's first frame,
/// since a group is the unit of decodability.
///
/// A relative or time anchor resolves at the hop that serves it, when it starts serving
/// and again whenever the subscriber replaces its ranges. Before any group exists a
/// relative anchor resolves at the first one, so [`Range::live`] starts there. A relay's
/// copy waiting on a fresh upstream answer counts from the live feed's latest group once
/// it answers, never from the groups an ended feed left in its cache.
///
/// Over moq-lite-07, a copy resolves them from the reported live edge and media time.
/// A Rust moq-transport copy resolves a relative anchor from the Largest Object its live
/// join reports, and accepts time anchors its cache can already resolve, translating both
/// to group bounds upstream; any other time anchor is refused. Older moq-lite versions
/// accept absolute anchors but refuse time anchors and relative anchors other
/// than [`Range::live`] and [`Range::oldest`] with [`Error::Unsupported`](crate::Error::Unsupported)
/// rather than guess from a partial copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Anchor {
	/// The group with this sequence.
	Absolute(u64),
	/// The group this many before the latest, clamped to group 0: `Relative(0)` is the latest group.
	Relative(u64),
	/// The group containing this presentation time: the newest one starting at or before it.
	///
	/// Resolved from the starts of the groups the serving hop holds. When none starts at
	/// or before it, a start reaches back to the oldest group, and while no group has
	/// started at all it waits like `Relative(0)`. An end settles once a group starting
	/// after it is known and every sequence below that one is accounted for (started,
	/// dropped, or never to start). Until then reads stop at the first held group that
	/// may still start, so at the live edge at most that one group past the end is read
	/// before its first frame shows where it starts. A datagram skips all of that: it is
	/// judged on its own, its timestamp against the range's times and its sequence against
	/// the range's absolute groups and the relative ones resolved so far. An untimed track
	/// refuses it with [`Error::TimestampMismatch`](crate::Error::TimestampMismatch).
	Time(Timestamp),
}

/// A span of a track to deliver: from `start` up to `end`, or onward when `end` is `None`.
///
/// An absolute end is exclusive, like the end of a [`std::ops::Range`], so `Absolute(2)..
/// Absolute(6)` delivers groups 2 through 5. Relative and time ends include their group, so
/// `Absolute(0)..Relative(0)` delivers through the latest group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Range {
	/// Where delivery starts.
	pub start: Anchor,
	/// Where delivery stops, or `None` to keep following the live edge.
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
	/// Unlike `Absolute(0)..` this names no floor: a wire whose publisher resolves the start
	/// from [`Subscription::max_delay`] reaches as far back as it allows, and an older wire
	/// starts at the live edge, filling nothing below it.
	pub fn oldest() -> Self {
		Self {
			start: Anchor::Relative(u64::MAX),
			end: None,
		}
	}

	/// The whole groups in `groups`, such as `2..=5`, `2..`, or `..6`.
	///
	/// An omitted start is [`Self::oldest`]'s, and an inclusive end past the last group is
	/// no end.
	pub fn groups(groups: impl RangeBounds<u64>) -> Self {
		let unbounded_start = matches!(groups.start_bound(), Bound::Unbounded);
		let (start, end) = sequence_bounds(groups);
		Self {
			start: match unbounded_start {
				true => Self::oldest().start,
				false => Anchor::Absolute(start),
			},
			end: end.map(Anchor::Absolute),
		}
	}

	/// Whether a wire without ranges carries this range upstream: absolute anchors, and
	/// the live and oldest starts it spells with no floor.
	pub(super) fn is_wire(&self) -> bool {
		let start = match self.start {
			Anchor::Absolute(_) => true,
			Anchor::Relative(back) => back == 0 || back == u64::MAX,
			Anchor::Time(_) => false,
		};
		start && matches!(self.end, None | Some(Anchor::Absolute(_)))
	}

	/// Whether this range certainly delivers nothing, wherever the latest group lands.
	pub(super) fn is_empty(&self) -> bool {
		match (self.start, self.end) {
			(Anchor::Absolute(start), Some(Anchor::Absolute(end))) => end <= start,
			_ => false,
		}
	}
}

/// The union of `ranges` in a canonical order, without empty or redundant ranges: the
/// reference for the track's incremental union.
///
/// Absolute spans merge where they overlap or touch. The rest can't be placed without the
/// latest group, so only an open range drops what it certainly covers: the open absolute
/// span drops ranges starting inside it, and the farthest open relative range drops
/// relative ranges starting no farther back. [`Range::oldest`] covers everything.
#[cfg(test)]
pub(super) fn union(ranges: impl IntoIterator<Item = Range>) -> Vec<Range> {
	let ranges: Vec<Range> = ranges.into_iter().filter(|range| !range.is_empty()).collect();
	if ranges.contains(&Range::oldest()) {
		return vec![Range::oldest()];
	}

	let mut spans: Vec<(u64, Option<u64>)> = ranges
		.iter()
		.filter_map(|range| match (range.start, range.end) {
			(Anchor::Absolute(start), None) => Some((start, None)),
			(Anchor::Absolute(start), Some(Anchor::Absolute(end))) => Some((start, Some(end))),
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
			start: Anchor::Absolute(start),
			end: end.map(Anchor::Absolute),
		})
		.collect();
	union.extend(ranges.into_iter().filter(|range| match (range.start, range.end) {
		(Anchor::Absolute(_), None | Some(Anchor::Absolute(_))) | (Anchor::Relative(_), None) => false,
		(Anchor::Absolute(start), _) => tail.is_none_or(|tail| start < tail),
		(Anchor::Relative(back), _) => farthest.is_none_or(|farthest| back > farthest),
		// A time can't be placed against the others without the hop's groups.
		(Anchor::Time(_), _) => true,
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
			max_delay: Some(Duration::ZERO),
			ranges: vec![Range::oldest()],
			order: Order::Descending,
		}
	}
}

impl Subscription {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// Set how far a group may fall behind the live edge before it is skipped, `None` for
	/// the publisher's max age alone, returning `self` for chaining.
	pub fn with_max_delay(mut self, max_delay: impl Into<Option<Duration>>) -> Self {
		self.max_delay = max_delay.into();
		self
	}

	/// [`Self::max_delay`] as a budget: `None` waits as long as anything could.
	pub(crate) fn budget(&self) -> Duration {
		self.max_delay.unwrap_or(Duration::MAX)
	}

	/// Replace [`Self::ranges`], returning `self` for chaining.
	pub fn with_ranges(mut self, ranges: impl IntoIterator<Item = Range>) -> Self {
		self.ranges = ranges.into_iter().collect();
		self
	}

	/// Request the whole groups in `groups` as the only range; see [`Range::groups`].
	/// Returns `self` for chaining.
	pub fn with_range(self, groups: impl RangeBounds<u64>) -> Self {
		self.with_ranges([Range::groups(groups)])
	}

	/// Set which pending group the publisher sends first, returning `self` for chaining.
	pub fn with_order(mut self, order: Order) -> Self {
		self.order = order;
		self
	}

	/// The lowest absolute group any range asks for, or `None` when one counts back from
	/// the latest group (or none is asked for).
	///
	/// The form a wire without ranges carries: a floor below which nothing is requested.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn start(&self) -> Option<u64> {
		let mut floor: Option<u64> = None;
		for range in &self.ranges {
			let Anchor::Absolute(start) = range.start else {
				return None;
			};
			floor = Some(floor.map_or(start, |floor| floor.min(start)));
		}
		floor
	}

	/// The exclusive end group past every range, or `None` when one is open or counts back
	/// from the latest group. An empty list is the empty range, ending at group 0.
	///
	/// The form a wire without ranges carries, alongside [`Self::start`].
	#[cfg(any(feature = "async", test))]
	pub(crate) fn end(&self) -> Option<u64> {
		let mut end = Some(0);
		for range in &self.ranges {
			let Some(Anchor::Absolute(next)) = range.end else {
				return None;
			};
			end = end.max(Some(next));
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
		self.with_ranges([Range {
			start: start.map_or(Range::oldest().start, Anchor::Absolute),
			end: end.map(Anchor::Absolute),
		}])
	}

	/// These preferences with every range raised to start no earlier than `floor` and
	/// stop no later than `end`, the exclusive end, without ever widening one.
	///
	/// `resolved` names where each range resolved, by index, so a relative or time anchor
	/// becomes its own group, raised to `floor` or capped at `end`. A start not resolved
	/// keeps its anchor, since the floor may lie before it, so the caller drops what it
	/// resolves to below `floor`; an end not resolved becomes the bound, except a time end,
	/// which stays so the caller enforces `end` apart from it.
	pub(super) fn clamped(&self, floor: Option<u64>, end: Option<u64>, resolved: &[Option<Resolved>]) -> Self {
		let clamping = floor.is_some() || end.is_some();
		let ranges = self
			.ranges
			.iter()
			.enumerate()
			.map(|(index, range)| {
				let resolved = resolved.get(index).copied().flatten().filter(|_| clamping);
				let start = match (range.start, resolved) {
					(Anchor::Relative(_) | Anchor::Time(_), Some(resolved)) => Anchor::Absolute(resolved.start),
					(start, _) => start,
				};
				let requested = match (range.end, resolved.and_then(|resolved| resolved.end)) {
					(Some(Anchor::Relative(_) | Anchor::Time(_)), Some(resolved)) => Some(Anchor::Absolute(resolved)),
					(requested, _) => requested,
				};
				Range {
					start: match (start, floor) {
						(Anchor::Absolute(start), Some(floor)) => Anchor::Absolute(start.max(floor)),
						(start, _) => start,
					},
					end: match (requested, end) {
						(requested, None) => requested,
						(Some(Anchor::Absolute(requested)), Some(end)) => Some(Anchor::Absolute(requested.min(end))),
						// Folding the cap in would end the copy's time filter for good, so the
						// caller enforces the cap itself and the copy keeps the time.
						(Some(Anchor::Time(pts)), Some(_)) => Some(Anchor::Time(pts)),
						(_, Some(end)) => Some(Anchor::Absolute(end)),
					},
				}
			})
			.collect();
		Self { ranges, ..self.clone() }
	}

	/// Whether any range ends at a presentation time, which an untimed track refuses.
	pub(super) fn has_time(&self) -> bool {
		self.ranges
			.iter()
			.any(|range| matches!(range.start, Anchor::Time(_)) || matches!(range.end, Some(Anchor::Time(_))))
	}

	/// Whether a subscriber joins at the live edge: a range counting back from the latest
	/// group and following it. A fixed start resumes a known group instead, such as a route
	/// switch picking up where the reader stopped.
	pub(super) fn joins_live(&self) -> bool {
		self.ranges
			.iter()
			.any(|range| matches!(range.start, Anchor::Relative(_)) && range.end.is_none())
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
		combined.max_delay = match (combined.max_delay, sub.max_delay) {
			(Some(a), Some(b)) => Some(a.max(b)),
			_ => None,
		};
		combined.ranges.extend_from_slice(&sub.ranges);
		// Sequence-first prioritization is enabled only when every subscriber wants it.
		if let Order::Descending = sub.order {
			combined.order = Order::Descending;
		}
	}

	/// The aggregate, or `None` without a subscriber.
	pub(super) fn finish(self) -> Option<Subscription> {
		let mut combined = self.0?;
		combined.ranges = union(std::mem::take(&mut combined.ranges));
		Some(combined)
	}
}

/// A frame-precise point in a track: a group sequence and a frame index within it.
///
/// Ordered lexicographically, so comparing positions is the same as comparing groups
/// and only falling back to frames within one. This is the model's counterpart of the
/// wire's (`Group`, `Frame`) pairs on SUBSCRIBE and FETCH.
///
/// Pairing the two is the point: a frame index counts from the start of a group, so it
/// means nothing on its own. Carrying them together makes "frame 5 of nothing"
/// unrepresentable rather than merely undefined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
	/// The group sequence.
	pub group: u64,
	/// The frame index within the group, numbered from 0 in write order.
	pub frame: u64,
}

impl Position {
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
	pub(crate) fn final_sequence(self) -> Option<u64> {
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
pub(super) fn sequence_bounds(range: impl RangeBounds<u64>) -> (u64, Option<u64>) {
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

/// Whether `sequence` is strictly below an exclusive cap. `None` is unbounded.
pub(super) fn before_end(sequence: u64, end: Option<u64>) -> bool {
	end.is_none_or(|end| sequence < end)
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

	fn absolute(group: u64) -> Anchor {
		Anchor::Absolute(group)
	}

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn ranges(subscriptions: &[Subscription]) -> Vec<Range> {
		combine(subscriptions).unwrap().ranges
	}

	/// A group range spells both ends at once, in whichever form the caller has.
	#[test]
	fn group_ranges_build_absolute_anchors() {
		assert_eq!(Range::groups(2..=5), range(absolute(2), absolute(6)));
		assert_eq!(Range::groups(2..6), range(absolute(2), absolute(6)));
		assert_eq!(Range::groups(..6), range(Range::oldest().start, absolute(6)));
		assert_eq!(Range::groups(2..), range(absolute(2), None));
		assert_eq!(Range::groups(..), Range::oldest());

		// Through the last group is unbounded.
		assert_eq!(Range::groups(..=u64::MAX).end, None);

		let sub = Subscription::default().with_range(2..=5).with_range(..);
		assert_eq!(sub, Subscription::default());
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
		assert_eq!(sequence_bounds(..0), (0, Some(0)));
		assert_eq!(sequence_bounds(..=5), (0, Some(6)));
		assert_eq!(sequence_bounds(..=u64::MAX), (0, None));
		assert_eq!(sequence_bounds(..), (0, None));
		assert_eq!(
			sequence_bounds((Bound::Unbounded, Bound::Included(5))),
			sequence_bounds(..6)
		);
	}

	/// A relative start counts back from the latest group and clamps at group 0. Before any
	/// group exists only the farthest offset resolves, since it is group 0 regardless.
	#[test]
	fn relative_starts_resolve_against_the_latest_group() {
		let floor = |sub: &Subscription, latest| Window::new(&sub.ranges, latest, &Unheld).unwrap().floor();

		let live = Subscription::default().with_ranges([Range::live()]);
		assert_eq!(floor(&live, Some(7)), Some(7));
		assert_eq!(floor(&live, None), None);

		let buffering = Subscription::default().with_ranges([range(Anchor::Relative(3), None)]);
		assert_eq!(floor(&buffering, Some(7)), Some(4));
		assert_eq!(floor(&buffering, Some(1)), Some(0), "an offset past the start clamps");

		assert_eq!(floor(&Subscription::default(), None), Some(0));

		// The lowest range wins, and one unresolved range leaves the whole floor unresolved.
		let mixed = Subscription::default().with_ranges([range(absolute(4), None), Range::live()]);
		assert_eq!(floor(&mixed, Some(3)), Some(3));
		assert_eq!(floor(&mixed, Some(9)), Some(4));
		assert_eq!(floor(&mixed, None), None);

		// Nothing asked for floors nowhere, and an empty range asks for nothing.
		assert_eq!(floor(&Subscription::default().with_ranges([]), None), Some(0));
		let empty = Subscription::default().with_ranges([Range::groups(5..5), Range::groups(10..)]);
		assert_eq!(floor(&empty, None), Some(10));
	}

	/// The bounds a wire without ranges carries: the lowest absolute start and the highest
	/// absolute end, either one lost to a range that counts back from the latest group.
	#[test]
	fn bounds_project_the_ranges_for_older_wires() {
		let sub = Subscription::default();
		assert_eq!((sub.start(), sub.end()), (None, None));

		let sub = Subscription::default().with_ranges([Range::groups(2..4), Range::groups(7..9)]);
		assert_eq!((sub.start(), sub.end()), (Some(2), Some(9)));

		// An explicit group 0 is a floor, unlike the default.
		let sub = Subscription::default().with_range(0..);
		assert_eq!((sub.start(), sub.end()), (Some(0), None));

		let sub = Subscription::default().with_ranges([Range::groups(2..4), Range::live()]);
		assert_eq!((sub.start(), sub.end()), (None, None));

		let sub = Subscription::default().with_ranges([range(absolute(2), Anchor::Relative(0))]);
		assert_eq!((sub.start(), sub.end()), (Some(2), None));

		// Nothing asked for is the empty range.
		let sub = Subscription::default().with_ranges([]);
		assert_eq!(sub.end(), Some(0));

		let sub = Subscription::default().with_start(5).with_end(10);
		assert_eq!((sub.start(), sub.end()), (Some(5), Some(10)));
		assert_eq!(sub.with_start(None).with_end(None), Subscription::default());
	}

	/// The aggregate is the union: overlapping and adjacent absolute ranges merge, an open
	/// range absorbs what starts inside it, and the oldest range absorbs everything.
	#[test]
	fn combined_ranges_are_the_union() {
		let sub = |ranges: &[Range]| Subscription::default().with_ranges(ranges.iter().copied());

		assert_eq!(
			ranges(&[sub(&[Range::groups(2..5)]), sub(&[Range::groups(4..8)])]),
			vec![Range::groups(2..8)]
		);
		assert_eq!(
			ranges(&[sub(&[Range::groups(2..5)]), sub(&[Range::groups(5..8)])]),
			vec![Range::groups(2..8)],
			"adjacent ranges merge"
		);
		assert_eq!(
			ranges(&[sub(&[Range::groups(2..5)]), sub(&[Range::groups(6..8)])]),
			vec![Range::groups(2..5), Range::groups(6..8)],
			"a gap stays a gap"
		);
		assert_eq!(
			ranges(&[
				sub(&[Range::groups(2..5), Range::groups(9..)]),
				sub(&[Range::groups(4..)])
			]),
			vec![Range::groups(2..)]
		);
		assert_eq!(
			ranges(&[sub(&[Range::groups(2..5)]), Subscription::default()]),
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

		// An absolute floor above the live edge does not hide the live edge: both stay.
		assert_eq!(
			ranges(&[sub(&[Range::groups(4..)]), sub(&[Range::live()])]),
			vec![Range::groups(4..), Range::live()]
		);

		// Empty ranges ask for nothing.
		assert_eq!(ranges(&[sub(&[Range::groups(5..5)])]), Vec::<Range>::new());
		assert_eq!(
			ranges(&[sub(&[]), sub(&[Range::groups(1..2)])]),
			vec![Range::groups(1..2)]
		);
	}

	/// Combining is order-independent and stops changing once it covers everyone.
	#[test]
	fn combined_ranges_settle() {
		let a = Subscription::default().with_ranges([Range::groups(2..5), Range::live()]);
		let b = Subscription::default().with_ranges([Range::groups(4..9)]);
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

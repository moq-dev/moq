use std::{task::Poll, time::Duration};

/// Subscriber-side preferences for receiving a track.
///
/// Each subscriber holds its own [`Subscription`]; the publisher observes an
/// aggregate across all live subscribers via [`crate::track::Producer::subscription`].
/// A subscriber can change its preferences after the fact with
/// [`crate::track::Subscriber::update`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Subscription {
	/// Delivery priority. Higher values preempt lower ones when bandwidth is constrained.
	pub priority: u8,
	/// Whether groups are prioritized in sequence order. Groups may always arrive
	/// out-of-order (or not at all) over the network. Defaults to `false`; the
	/// aggregate is ordered only when every live subscriber asks for it.
	pub ordered: bool,
	/// The maximum age of a non-latest group before it is skipped. `Duration::ZERO`
	/// skips immediately (e.g. group 8 arriving means group 7 is skipped); a larger
	/// value tolerates that much reordering before giving up on the older group.
	///
	/// This is the `Subscriber Max Latency` on the wire, enforced by the publisher's
	/// cache. Receivers that buffer (e.g. a jitter buffer) enforce the same budget
	/// locally, and a group is skipped once either measure exceeds it.
	pub latency_max: Duration,
	/// First [`Position`] the publisher should deliver, or `None` to start at the latest
	/// group.
	///
	/// A request, aggregated across every live subscriber (the earliest explicit start
	/// wins), so it says what the publisher sends, not what any one subscriber sees.
	/// [`crate::track::Subscriber::start_at`] is the local read cursor; setting one does
	/// not imply the other. See [Local cursor vs wire
	/// preference](crate::track::Subscriber#local-cursor-vs-wire-preference).
	pub start: Option<Position>,
	/// First [`Position`] the publisher should *not* deliver, or `None` for no end.
	///
	/// Exclusive, like the end of a [`std::ops::Range`], which is what lets one field
	/// carry both "through the end of group 5" ([`Position::after_group(5)`](Position::after_group))
	/// and "up to frame 2 of group 5" ([`Position::after(5, 2)`](Position::after)). An
	/// inclusive end cannot express the first without a sentinel frame, and the ordering
	/// falls out for free: group 6's head sorts above any frame of group 5, so a
	/// whole-group subscriber correctly absorbs a frame-capped one in the aggregate.
	///
	/// The wire agrees: `Group End` and `Frame End` are both encoded as `absolute + 1`.
	///
	/// A request, aggregated across every live subscriber (any unbounded subscriber makes
	/// the aggregate unbounded). [`crate::track::Subscriber::end_at`] is the local read
	/// cursor; setting one does not imply the other.
	pub end: Option<Position>,
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ordered: false,
			latency_max: Duration::ZERO,
			start: None,
			end: None,
		}
	}
}

impl Subscription {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}

	/// Set whether groups are prioritized in sequence order, returning `self` for
	/// chaining. Groups may always arrive out-of-order (or not at all) over the network.
	pub fn with_ordered(mut self, ordered: bool) -> Self {
		self.ordered = ordered;
		self
	}

	/// Set the maximum age of a non-latest group before it is skipped, returning
	/// `self` for chaining.
	pub fn with_latency_max(mut self, latency_max: Duration) -> Self {
		self.latency_max = latency_max;
		self
	}

	/// Start delivery at `start`, or at the latest group when `None`. Returns `self` for
	/// chaining.
	///
	/// [`Position::group`] is the whole-group form.
	pub fn with_start(mut self, start: impl Into<Option<Position>>) -> Self {
		self.start = start.into();
		self
	}

	/// Stop delivery at `end`, or leave the subscription unbounded when `None`. Returns
	/// `self` for chaining.
	///
	/// Exclusive, matching [`Self::end`], so pass the position *after* the last one you
	/// want. [`Position::after`] and [`Position::after_group`] name that conversion so no
	/// call site has to write the `+ 1` itself.
	pub fn with_end(mut self, end: impl Into<Option<Position>>) -> Self {
		self.end = end.into();
		self
	}

	// Fold this subscription into the running aggregate: Ready with the merged
	// result when it demands more than `combined`, Pending when it's a subset
	// (so callers can skip a redundant broadcast of the same aggregate).
	pub(super) fn poll_combined(&self, combined: &Option<Subscription>) -> Poll<Subscription> {
		let Some(combined) = combined else {
			return Poll::Ready(self.clone());
		};

		let merged = Subscription {
			priority: self.priority.max(combined.priority),
			// Sequence-first prioritization is enabled only when every subscriber wants it.
			ordered: self.ordered && combined.ordered,
			latency_max: self.latency_max.max(combined.latency_max),
			// Bounds fold as whole positions. Two subscribers starting in the same group
			// are separated only by their frame, so folding group and frame independently
			// would invent a bound neither asked for.
			start: min_some(self.start, combined.start),
			end: max_unbounded(self.end, combined.end),
		};

		if &merged != combined {
			return Poll::Ready(merged);
		}

		Poll::Pending
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
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
	/// group. That is past everything, which [`Subscription::end`] spells `None` too, so
	/// the two meanings line up and `with_end` can take this directly.
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

	/// The last position strictly below this one, turning an exclusive bound such as
	/// [`Subscription::end`] into the inclusive one an API like
	/// [`crate::track::Subscriber::end_at`] wants.
	///
	/// `None` for the very first position, which is the empty range: nothing sorts below
	/// it, so there is no inclusive bound to convert to. Saturating instead would return
	/// a position *above* the input and quietly include the group it excludes.
	pub fn before(self) -> Option<Self> {
		if let Some(frame) = self.frame.checked_sub(1) {
			return Some(Self {
				group: self.group,
				frame,
			});
		}

		Some(Self {
			group: self.group.checked_sub(1)?,
			frame: u64::MAX,
		})
	}
}

/// The lower of two optional bounds, treating `None` as unbounded.
pub(super) fn min_some<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(Some(a), None) | (None, Some(a)) => Some(a),
		(None, None) => None,
	}
}

/// The higher of two optional bounds, treating `None` as unbounded (and therefore
/// absorbing).
pub(super) fn max_unbounded<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.max(b)),
		(None, _) | (_, None) => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn combine(subscriptions: &[Subscription]) -> Option<Subscription> {
		let mut combined = None;
		for sub in subscriptions {
			if let Poll::Ready(merged) = sub.poll_combined(&combined) {
				combined = Some(merged);
			}
		}
		combined
	}

	#[test]
	fn combined_ordered_stays_ordered_for_multiple_ordered_viewers() {
		let subscription = Subscription::default().with_ordered(true);

		let combined = combine(&[subscription.clone(), subscription.clone(), subscription]).unwrap();

		assert!(combined.ordered);
	}

	#[test]
	fn combined_any_unordered_viewer_disables_ordered() {
		let unordered = Subscription::default().with_ordered(false);
		let ordered = Subscription::default().with_ordered(true);

		let combined = combine(&[unordered, ordered]).unwrap();

		assert!(!combined.ordered);
	}

	/// The exclusive representation runs out at both extremes, and `Option` says so
	/// rather than saturating into a bound that contradicts the request.
	#[test]
	fn positions_are_total_at_the_extremes() {
		// Past the last frame of a group is the head of the next one, not a wider frame
		// in the same group.
		assert_eq!(Position::after(5, u64::MAX), Some(Position::group(6)));

		// Past everything has no position. `Subscription::end` spells that `None` too,
		// so the meanings coincide and the group is included rather than dropped.
		assert_eq!(Position::after_group(u64::MAX), None);
		assert_eq!(Position::after(u64::MAX, u64::MAX), None);
		assert_eq!(
			Subscription::default().with_end(Position::after_group(u64::MAX)).end,
			None
		);

		// Nothing sorts below the first position, so there is no inclusive bound for the
		// empty range. Saturating would return one *above* the input.
		assert_eq!(Position::group(0).before(), None);
		assert_eq!(
			Position::group(1).before(),
			Some(Position {
				group: 0,
				frame: u64::MAX
			})
		);
		assert_eq!(
			Position::after(4, 7).unwrap().before(),
			Some(Position { group: 4, frame: 7 })
		);
	}

	#[test]
	fn combined_group_start_uses_earliest_explicit_start() {
		// No start at all is the live edge, which loses to any explicit one.
		let live = Subscription::default();
		let catchup = Subscription::default().with_start(Position::group(10));
		let older_catchup = Subscription::default().with_start(Position::group(5));

		let combined = combine(&[live, catchup, older_catchup]).unwrap();

		assert_eq!(combined.start, Some(Position::group(5)));
	}

	#[test]
	fn combined_group_end_keeps_live_subscription_unbounded() {
		// No end at all is unbounded, which absorbs any explicit one.
		let live = Subscription::default();
		let bounded = Subscription::default().with_end(Position::after_group(10));

		let combined = combine(&[live, bounded]).unwrap();

		assert_eq!(combined.end, None);
	}

	#[test]
	fn combined_start_folds_the_whole_position() {
		let early_frame = Subscription::default().with_start(Position { group: 5, frame: 2 });
		let late_frame = Subscription::default().with_start(Position { group: 5, frame: 9 });

		// Same group: the earlier frame wins.
		let combined = combine(&[late_frame.clone(), early_frame.clone()]).unwrap();
		assert_eq!(combined.start, Some(Position { group: 5, frame: 2 }));

		// An earlier group wins outright, carrying its own frame rather than the
		// smallest frame across the two.
		let earlier_group = Subscription::default().with_start(Position { group: 4, frame: 7 });
		let combined = combine(&[early_frame, earlier_group]).unwrap();
		assert_eq!(combined.start, Some(Position { group: 4, frame: 7 }));
	}

	#[test]
	fn combined_end_folds_the_whole_position() {
		let short = Subscription::default().with_end(Position::after(5, 2));
		let long = Subscription::default().with_end(Position::after(5, 9));

		// Same group: the later frame wins. Ends are exclusive, so an inclusive frame 9
		// is stored as 10.
		let combined = combine(&[short.clone(), long.clone()]).unwrap();
		assert_eq!(combined.end, Some(Position { group: 5, frame: 10 }));

		// The whole group is the head of the next one, which outsorts every frame of
		// this one, so it absorbs any capped end without a sentinel.
		let whole = Subscription::default().with_end(Position::after_group(5));
		let combined = combine(&[long, whole]).unwrap();
		assert_eq!(combined.end, Some(Position::group(6)));

		// A later group wins outright, carrying its own frame.
		let later_group = Subscription::default().with_end(Position::after(6, 1));
		let combined = combine(&[short, later_group]).unwrap();
		assert_eq!(combined.end, Some(Position { group: 6, frame: 2 }));
	}

	#[test]
	fn combined_group_end_uses_latest_bounded_end() {
		let early = Subscription::default().with_end(Position::after_group(10));
		let late = Subscription::default().with_end(Position::after_group(20));

		let combined = combine(&[early, late]).unwrap();

		assert_eq!(combined.end, Some(Position::group(21)));
	}
}

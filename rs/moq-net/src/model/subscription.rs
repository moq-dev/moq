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
	/// First group the publisher should deliver, or `None` to start at the latest group.
	///
	/// A request, aggregated across every live subscriber (the earliest explicit start
	/// wins), so it says what the publisher sends, not what any one subscriber sees.
	/// [`crate::track::Subscriber::start_at`] is the local read cursor; setting one does
	/// not imply the other. See [Local cursor vs wire
	/// preference](crate::track::Subscriber#local-cursor-vs-wire-preference).
	pub group_start: Option<u64>,
	/// Last group the publisher should deliver (inclusive), or `None` for no end.
	///
	/// A request, aggregated across every live subscriber (any unbounded subscriber makes
	/// the aggregate unbounded). [`crate::track::Subscriber::end_at`] is the local read
	/// cursor; setting one does not imply the other. See [Local cursor vs wire
	/// preference](crate::track::Subscriber#local-cursor-vs-wire-preference).
	pub group_end: Option<u64>,
	/// First frame to deliver within [`Self::group_start`]'s group. `0` (the default)
	/// starts at the beginning of that group.
	///
	/// Frames are numbered per group, so this counts from nothing without an explicit
	/// `group_start` and is ignored when there is none. Set the pair together with
	/// [`Self::with_start`], which is the only way to reach it.
	pub frame_start: u64,
	/// Last frame to deliver (inclusive) within [`Self::group_end`]'s group, or `None`
	/// (the default) for through the end of that group.
	///
	/// Ignored without an explicit `group_end`, mirroring [`Self::frame_start`]. Set the
	/// pair together with [`Self::with_end`].
	pub frame_end: Option<u64>,
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ordered: false,
			latency_max: Duration::ZERO,
			group_start: None,
			group_end: None,
			frame_start: 0,
			frame_end: None,
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

	/// Set the first group to deliver, returning `self` for chaining.
	pub fn with_group_start(mut self, group_start: impl Into<Option<u64>>) -> Self {
		self.group_start = group_start.into();
		self
	}

	/// Set the last group to deliver (inclusive), returning `self` for chaining.
	pub fn with_group_end(mut self, group_end: impl Into<Option<u64>>) -> Self {
		self.group_end = group_end.into();
		self
	}

	/// Set the first position to deliver: frame `frame` of group `group`, returning `self`
	/// for chaining.
	///
	/// The group comes with it because a frame index only means something relative to a
	/// group, so there is no way to name a frame without the group it belongs to.
	/// [`Self::with_group_start`] is the whole-group form, the same as `frame` 0.
	pub fn with_start(mut self, group: u64, frame: u64) -> Self {
		self.group_start = Some(group);
		self.frame_start = frame;
		self
	}

	/// Set the last position to deliver (inclusive): frame `frame` of group `group`, or
	/// all of `group` when `frame` is `None`. Returns `self` for chaining.
	///
	/// Pairs the group with the frame for the same reason as [`Self::with_start`].
	pub fn with_end(mut self, group: u64, frame: impl Into<Option<u64>>) -> Self {
		self.group_end = Some(group);
		self.frame_end = frame.into();
		self
	}

	/// The requested start as an ordered position, or `None` for the live edge.
	pub(crate) fn start(&self) -> Option<Position> {
		self.group_start.map(|group| Position {
			group,
			frame: self.frame_start,
		})
	}

	/// The requested end as an ordered position, or `None` for unbounded. The frame is
	/// `u64::MAX` when the whole end group is wanted, so the ordering matches the
	/// "widest end wins" aggregate.
	pub(crate) fn end(&self) -> Option<Position> {
		self.group_end.map(|group| Position {
			group,
			frame: self.frame_end.unwrap_or(u64::MAX),
		})
	}

	/// Apply an aggregated start position, or the live edge when `None`.
	pub(crate) fn set_start(&mut self, start: Option<Position>) {
		self.group_start = start.map(|start| start.group);
		self.frame_start = start.map_or(0, |start| start.frame);
	}

	/// Apply an aggregated end position, or unbounded when `None`. A `u64::MAX` frame
	/// maps back to "the whole end group".
	pub(crate) fn set_end(&mut self, end: Option<Position>) {
		self.group_end = end.map(|end| end.group);
		self.frame_end = end.and_then(|end| (end.frame != u64::MAX).then_some(end.frame));
	}

	// Fold this subscription into the running aggregate: Ready with the merged
	// result when it demands more than `combined`, Pending when it's a subset
	// (so callers can skip a redundant broadcast of the same aggregate).
	pub(super) fn poll_combined(&self, combined: &Option<Subscription>) -> Poll<Subscription> {
		let Some(combined) = combined else {
			return Poll::Ready(self.clone());
		};

		let mut merged = Subscription {
			priority: self.priority.max(combined.priority),
			// Sequence-first prioritization is enabled only when every subscriber wants it.
			ordered: self.ordered && combined.ordered,
			latency_max: self.latency_max.max(combined.latency_max),
			..Subscription::default()
		};
		// Frame-precise bounds fold as whole positions: two subscribers starting in the
		// same group are separated only by their frame, so folding the group and the
		// frame independently would invent a start neither asked for.
		merged.set_start(min_some(self.start(), combined.start()));
		merged.set_end(max_unbounded(self.end(), combined.end()));

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
/// wire's (`Group`, `Frame`) pairs on SUBSCRIBE / SUBSCRIBE_OK / FETCH.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
	/// The group sequence.
	pub group: u64,
	/// The frame index within the group.
	pub frame: u64,
}

impl Position {
	/// The start of a group.
	pub fn group(group: u64) -> Self {
		Self { group, frame: 0 }
	}

	/// The last position strictly below this one, converting a half-open bound into the
	/// inclusive one a [`Subscription`] carries.
	///
	/// A boundary at the head of a group backs up to all of the group below it, which
	/// saturates at the very first frame. Nothing produces a boundary there: a segment
	/// capped at the start of the track would serve nothing, and such a segment is
	/// replaced outright rather than capped.
	pub fn before(self) -> Self {
		match self.frame.checked_sub(1) {
			Some(frame) => Self {
				group: self.group,
				frame,
			},
			None => Self {
				group: self.group.saturating_sub(1),
				frame: u64::MAX,
			},
		}
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

	#[test]
	fn combined_group_start_uses_earliest_explicit_start() {
		let live = Subscription::default().with_group_start(None);
		let catchup = Subscription::default().with_group_start(10);
		let older_catchup = Subscription::default().with_group_start(5);

		let combined = combine(&[live, catchup, older_catchup]).unwrap();

		assert_eq!(combined.group_start, Some(5));
	}

	#[test]
	fn combined_group_end_keeps_live_subscription_unbounded() {
		let live = Subscription::default().with_group_end(None);
		let bounded = Subscription::default().with_group_end(10);

		let combined = combine(&[live, bounded]).unwrap();

		assert_eq!(combined.group_end, None);
	}

	#[test]
	fn combined_start_folds_the_whole_position() {
		let early_frame = Subscription::default().with_start(5, 2);
		let late_frame = Subscription::default().with_start(5, 9);

		// Same group: the earlier frame wins.
		let combined = combine(&[late_frame.clone(), early_frame.clone()]).unwrap();
		assert_eq!((combined.group_start, combined.frame_start), (Some(5), 2));

		// An earlier group wins outright, carrying its own frame rather than the
		// smallest frame across the two.
		let earlier_group = Subscription::default().with_start(4, 7);
		let combined = combine(&[early_frame, earlier_group]).unwrap();
		assert_eq!((combined.group_start, combined.frame_start), (Some(4), 7));
	}

	#[test]
	fn combined_end_folds_the_whole_position() {
		let short = Subscription::default().with_end(5, 2);
		let long = Subscription::default().with_end(5, 9);

		// Same group: the later frame wins.
		let combined = combine(&[short.clone(), long.clone()]).unwrap();
		assert_eq!((combined.group_end, combined.frame_end), (Some(5), Some(9)));

		// An unbounded frame is the whole group, so it absorbs any capped one.
		let whole = Subscription::default().with_group_end(5);
		let combined = combine(&[long, whole]).unwrap();
		assert_eq!((combined.group_end, combined.frame_end), (Some(5), None));

		// A later group wins outright, carrying its own frame.
		let later_group = Subscription::default().with_end(6, 1);
		let combined = combine(&[short, later_group]).unwrap();
		assert_eq!((combined.group_end, combined.frame_end), (Some(6), Some(1)));
	}

	#[test]
	fn combined_group_end_uses_latest_bounded_end() {
		let early = Subscription::default().with_group_end(10);
		let late = Subscription::default().with_group_end(20);

		let combined = combine(&[early, late]).unwrap();

		assert_eq!(combined.group_end, Some(20));
	}
}

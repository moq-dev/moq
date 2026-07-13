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
	/// How long to wait for a group before skipping it once a newer group has
	/// arrived. `Duration::ZERO` skips immediately (e.g. group 8 arriving means
	/// group 7 is skipped); a larger value tolerates that much reordering before
	/// giving up on the older group.
	pub stale: Duration,
	/// First group to deliver, or `None` to start at the latest group.
	pub group_start: Option<u64>,
	/// Last group to deliver (inclusive), or `None` for no end.
	pub group_end: Option<u64>,
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ordered: false,
			stale: Duration::ZERO,
			group_start: None,
			group_end: None,
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

	/// Set how long to wait for a group before skipping it, returning `self` for chaining.
	pub fn with_stale(mut self, stale: Duration) -> Self {
		self.stale = stale;
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
			stale: self.stale.max(combined.stale),
			group_start: min_some(self.group_start, combined.group_start),
			group_end: max_unbounded(self.group_end, combined.group_end),
		};

		if &merged != combined {
			return Poll::Ready(merged);
		}

		Poll::Pending
	}
}

fn min_some(a: Option<u64>, b: Option<u64>) -> Option<u64> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(Some(a), None) | (None, Some(a)) => Some(a),
		(None, None) => None,
	}
}

fn max_unbounded(a: Option<u64>, b: Option<u64>) -> Option<u64> {
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
	fn combined_group_end_uses_latest_bounded_end() {
		let early = Subscription::default().with_group_end(10);
		let late = Subscription::default().with_group_end(20);

		let combined = combine(&[early, late]).unwrap();

		assert_eq!(combined.group_end, Some(20));
	}
}

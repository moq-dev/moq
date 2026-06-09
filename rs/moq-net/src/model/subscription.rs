use std::time::Duration;

/// Subscriber-side preferences for receiving a track.
///
/// Each subscriber holds its own [`Subscription`]; the publisher observes an
/// aggregate across all live subscribers via [`crate::TrackProducer::subscription`].
/// A subscriber can change its preferences after the fact with
/// [`crate::TrackSubscriber::update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
	/// Delivery priority. Higher values preempt lower ones when bandwidth is constrained.
	pub priority: u8,
	/// Whether groups should be delivered in sequence order.
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
	/// How many downstream consumers this subscription represents. A leaf
	/// subscriber is `1`; a relay reports the sum of its own downstream
	/// subscribers so the count telescopes up the fan-out tree. The publisher
	/// reads the aggregate via [`crate::TrackProducer::subscription`] to learn
	/// the total viewer count across every hop.
	pub downstream: u64,
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ordered: true,
			stale: Duration::ZERO,
			group_start: None,
			group_end: None,
			downstream: 1,
		}
	}
}

impl Subscription {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}

	/// Set whether groups are delivered in sequence order, returning `self` for chaining.
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

	/// Set how many downstream consumers this subscription represents, returning
	/// `self` for chaining. Defaults to `1`; relays override it with their summed
	/// downstream count.
	pub fn with_downstream(mut self, downstream: u64) -> Self {
		self.downstream = downstream;
		self
	}

	/// Combine two subscribers' preferences into the most demanding request. The
	/// operation is commutative and associative, so the producer can fold it across
	/// every live subscriber in any order.
	///
	/// Most fields take the most permissive value (highest priority, longest stale
	/// window, widest group range); `downstream` instead **sums**, so each subscriber
	/// contributes its viewer count and the aggregate is the total across every
	/// fan-out branch.
	pub(super) fn merge(&self, other: &Subscription) -> Subscription {
		Subscription {
			priority: self.priority.max(other.priority),
			ordered: !self.ordered || !other.ordered,
			stale: self.stale.max(other.stale),
			group_start: self.group_start.min(other.group_start),
			group_end: self.group_end.max(other.group_end),
			downstream: self.downstream.saturating_add(other.downstream),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Fold a set of subscriptions the same way [`crate::TrackProducer::subscription`]
	/// does: start from `None` and merge each in turn.
	fn combine(subs: &[Subscription]) -> Option<Subscription> {
		let mut combined: Option<Subscription> = None;
		for sub in subs {
			combined = Some(match combined {
				Some(c) => sub.merge(&c),
				None => sub.clone(),
			});
		}
		combined
	}

	#[test]
	fn downstream_sums_across_subscribers() {
		// Three leaf viewers aggregate to 3.
		let leaves = [
			Subscription::default(),
			Subscription::default(),
			Subscription::default(),
		];
		assert_eq!(combine(&leaves).unwrap().downstream, 3);

		// A relay reporting 50 plus two direct viewers telescopes to 52.
		let mixed = [
			Subscription::default().with_downstream(50),
			Subscription::default(),
			Subscription::default(),
		];
		assert_eq!(combine(&mixed).unwrap().downstream, 52);
	}

	#[test]
	fn downstream_default_is_one() {
		assert_eq!(Subscription::default().downstream, 1);
		assert_eq!(combine(&[Subscription::default()]).unwrap().downstream, 1);
	}
}

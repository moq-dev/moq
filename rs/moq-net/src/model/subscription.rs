use std::{task::Poll, time::Duration};

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
}

impl Default for Subscription {
	fn default() -> Self {
		Self {
			priority: 0,
			ordered: true,
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

	// Returns Ready with the new combined subscription, unless this subscription is a subset.
	// TODO I don't know if we need to return Pending at all? I'm kind of confused.
	pub(super) fn poll_combined(&self, combined: &Option<Subscription>) -> Poll<Subscription> {
		let Some(combined) = combined else {
			return Poll::Ready(self.clone());
		};

		let merged = Subscription {
			priority: self.priority.max(combined.priority),
			ordered: !self.ordered || !combined.ordered,
			stale: self.stale.max(combined.stale),
			group_start: self.group_start.min(combined.group_start),
			group_end: self.group_end.max(combined.group_end),
		};

		if &merged != combined {
			return Poll::Ready(merged);
		}

		Poll::Pending
	}
}

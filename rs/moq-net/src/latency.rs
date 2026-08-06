//! How far a subscriber may drift from the live edge.

use std::time::Duration;

/// How far a subscriber may drift behind the live edge before a group is skipped.
///
/// One budget, enforced in two places: the publisher's cache drops a group that exceeds it
/// (this is `Subscriber Max Latency` on the wire, see
/// [`Subscription::latency`](crate::track::Subscription::latency)), and a receiver that
/// buffers applies the same bound locally while reordering. A group is skipped once either
/// measure exceeds it.
///
/// Only a ceiling today: a stalled group is skipped once newer data is [`max`](Self::max)
/// ahead of it. [`Latency::REAL_TIME`] (the default) skips aggressively, so any group with
/// a newer alternative is dropped.
///
/// This never *adds* latency. The bound is reached only when newer data is already that far
/// ahead, so raising it buys a stalled group more time to arrive rather than padding a
/// buffer.
///
/// Distinct from [`track::Info::latency_max`](crate::track::Info::latency_max), which is a
/// *retention* bound (how long the publisher keeps a group, the inverse of an HTTP
/// `Cache-Control: max-age`) rather than a drift budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Latency {
	/// Ceiling: a group is skipped once newer data is this far ahead of it.
	///
	/// Zero (the default) skips aggressively.
	pub max: Duration,
}

impl Latency {
	/// Minimize latency: any group with a newer alternative is dropped.
	pub const REAL_TIME: Self = Self { max: Duration::ZERO };

	/// Tolerate a stalled group until newer data is `max` ahead of it.
	///
	/// Set it to the playout buffer you can absorb (typically tens to a few hundred
	/// milliseconds) for the best congestion-vs-quality trade-off.
	pub const fn max(max: Duration) -> Self {
		Self { max }
	}

	/// The budget that satisfies both: the more tolerant of the two.
	///
	/// How subscriptions to one track combine, since a group may only be skipped once
	/// every subscriber has given up on it.
	pub fn merge(self, other: Self) -> Self {
		Self::max(self.max.max(other.max))
	}
}

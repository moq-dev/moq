//! How far a subscriber may drift from the live edge.

use std::time::Duration;

/// How far a subscriber may drift behind the live edge before a group is skipped.
///
/// Only a ceiling today: a stalled group is skipped once newer data is [`max`](Self::max)
/// ahead of it. [`Latency::REAL_TIME`] (the default) skips aggressively, so any group with
/// a newer alternative is dropped.
///
/// This never *adds* latency. The bound is reached only when newer data is already that far
/// ahead, so raising it buys a stalled group more time to arrive rather than padding a
/// buffer.
///
/// # Where it is enforced
///
/// At both ends of a subscription, and neither alone is enough. A publisher skips a group
/// that has drifted past the budget instead of putting it on the wire, which is what bounds
/// a backlog before it costs bandwidth. But what reaches a publisher is the aggregate
/// across every subscriber, which [`merge`](Self::merge) resolves in favor of the most
/// tolerant one, so that gate is only ever as tight as the most patient viewer. The
/// subscriber applies the same budget again as it reads, where its own budget is the only
/// one in play and the semantics are exact.
///
/// This bounds a *subscription*. [`track::Consumer::fetch_group`](crate::track::Consumer::fetch_group)
/// is exempt: it names one old group explicitly, so there is no live edge to be late
/// against.
///
/// # How drift is measured
///
/// By both presentation time and wall-clock arrival time, with either age able to expire
/// the group. Presentation time compares a group's first timestamp against the newest one
/// above it, so a backlog delivered as a burst still reads as its true age. Wall-clock time
/// compares when each group entered this hop, which backstops an empty or stalled group that
/// has not supplied a first-frame timestamp.
///
/// Protocols whose wire can't carry a timestamp (pre-Lite05 moq-lite, moq-transport without
/// the Timestamp property) have their frames stamped on receipt instead, which makes the
/// measure burst-blind on the receiving side: thirty seconds of backlog delivered in three
/// reads as three. The publisher's copy is stamped as it produces, so the gate there still
/// holds; it is just the coarser of the two.
///
/// # Not a retention window
///
/// Distinct from [`track::Info::latency_max`](crate::track::Info::latency_max), which is a
/// *retention* bound (how long the publisher keeps a group, the inverse of an HTTP
/// `Cache-Control: max-age`) rather than a drift budget. A budget is clamped to it, since
/// waiting for a group longer than it is kept around cannot produce it.
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

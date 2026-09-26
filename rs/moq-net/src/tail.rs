//! A subscription's tail: the group streams still owed once the publisher has ended it.
//!
//! A publisher ends a subscription only after every group stream it opened has finished,
//! but QUIC does not order streams, so one opened before the end can still reach us after
//! it. The subscriber keeps the subscription routable until each owed group is accounted
//! for, or a grace expires for a group whose stream was reset before its header arrived.
//! A reset that keeps the header (reliable reset) would make the grace unnecessary.

use std::{ops::Range, task::Poll, time::Duration};

use crate::runtime::Deadline;

/// How long a subscriber waits for a group stream it cannot account for once the
/// publisher has ended the subscription.
///
/// Bounds the wait on IETF, and on moq-lite when the subscription has no max age to bound
/// it with. Matches `@moq/net`, so a reader cannot tell which side it is talking to.
pub(crate) const GRACE: Duration = Duration::from_secs(1);

/// The data streams a subscription has received, and the groups they account for.
#[derive(Default, Debug)]
pub(crate) struct Tail {
	// Disjoint, sorted, non-adjacent ranges of accounted sequences, so this grows with the
	// number of gaps rather than the number of groups.
	accounted: Vec<Range<u64>>,
	streams: u64,
}

impl Tail {
	/// Record a group stream whose header arrived.
	pub fn open(&mut self, sequence: u64) {
		self.stream();
		self.account(sequence..sequence.saturating_add(1));
	}

	/// Record a data stream that names no single group, such as a fill.
	pub fn stream(&mut self) {
		self.streams += 1;
	}

	/// Record groups as accounted for without a stream: dropped, or sent as a datagram.
	pub fn account(&mut self, groups: Range<u64>) {
		if groups.is_empty() {
			return;
		}

		// Every range the insert overlaps or touches merges with it into one.
		let first = self.accounted.partition_point(|range| range.end < groups.start);
		let last = self.accounted.partition_point(|range| range.start <= groups.end);
		let merged = match &self.accounted[first..last] {
			[] => groups,
			[head, .., tail] => head.start.min(groups.start)..tail.end.max(groups.end),
			[only] => only.start.min(groups.start)..only.end.max(groups.end),
		};
		self.accounted.splice(first..last, [merged]);
	}

	/// Whether every group in `groups` is accounted for.
	pub fn covers(&self, groups: Range<u64>) -> bool {
		// Ranges merge on insert, so one range covers the span or none does.
		groups.is_empty()
			|| self
				.accounted
				.iter()
				.any(|range| range.start <= groups.start && groups.end <= range.end)
	}

	/// Data streams received, whether they finished or were reset.
	pub fn streams(&self) -> u64 {
		self.streams
	}
}

/// Waits out a subscription's tail: until the owed streams are accounted for, or the grace.
pub(crate) struct Settle {
	tail: kio::Consumer<Tail>,
	grace: Deadline<crate::time::Clock>,
}

impl Settle {
	/// Start waiting on `tail`, giving up on missing streams after `grace`.
	pub fn new(runtime: &crate::time::Clock, tail: kio::Consumer<Tail>, grace: Duration) -> Self {
		Self {
			tail,
			grace: Deadline::after(runtime, grace),
		}
	}

	/// Ready once `complete` holds, the grace expires, or the subscription is gone.
	pub fn poll(&mut self, waiter: &kio::Waiter, mut complete: impl FnMut(&Tail) -> bool) -> Poll<()> {
		if self.grace.poll(waiter).is_ready() {
			return Poll::Ready(());
		}
		self.tail
			.poll(waiter, |tail| match complete(tail) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ranges_merge_across_gaps() {
		let mut tail = Tail::default();
		tail.open(0);
		tail.open(2);
		assert!(!tail.covers(0..3), "group 1 is missing");
		assert_eq!(tail.accounted, vec![0..1, 2..3]);

		tail.account(1..2);
		assert!(tail.covers(0..3) && tail.accounted.len() == 1, "adjacent ranges merge");

		tail.account(5..7);
		tail.account(9..10);
		tail.account(4..9);
		assert_eq!(tail.accounted, vec![0..3, 4..10], "one insert swallows several ranges");
		assert!(tail.covers(4..10));
		assert!(!tail.covers(2..5));
		assert!(tail.covers(7..7), "an empty range is always covered");
		assert_eq!(tail.streams(), 2, "only streams count, not drops");
	}
}

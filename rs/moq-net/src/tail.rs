//! A subscription's tail: the group streams still owed once the publisher has ended it.
//!
//! A publisher ends a subscription only after every group stream it opened has finished,
//! but QUIC does not order streams, so one opened before the end can still reach us after
//! it. The subscriber keeps the subscription routable until each owed group is accounted
//! for, or a grace expires for a group whose stream was reset before its header arrived.
//! A reset that keeps the header (reliable reset) would make the grace unnecessary.
//!
//! A stream whose header arrived is read to its end whatever the grace says: a group ends
//! on its own stream's FIN or reset, never because its track ended. A lost datagram is not
//! owed: it leaves a hole that waits out the grace like a stream reset before its header.

use std::{ops::Range, task::Poll, time::Duration};

use crate::time::{Deadline, Instant};

/// How long a subscriber waits for a group stream it cannot account for once the
/// publisher has ended the subscription.
///
/// Bounds the wait on IETF, and on moq-lite when the subscription has no max delay to bound
/// it with. Matches `@moq/net`, so a reader cannot tell which side it is talking to.
pub(crate) const GRACE: Duration = Duration::from_secs(1);

/// The grace for a subscription with `max_delay`: how long the subscriber was willing to
/// wait for a late group anyway, or [`GRACE`] without one.
pub(crate) fn grace(max_delay: Duration) -> Duration {
	match max_delay.is_zero() {
		true => GRACE,
		false => max_delay,
	}
}

/// A run of accounted sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
	groups: Range<u64>,
	/// When the gap below this run opened, which is when its missing groups became late.
	since: Instant,
}

/// The data streams a subscription has received, and the groups they account for.
#[derive(Debug)]
pub(crate) struct Tail {
	// Disjoint, sorted, non-adjacent runs of accounted sequences. A gap older than the grace
	// can no longer be waited for, so it folds into the runs around it, which bounds this by
	// the gaps opened within the grace rather than every gap in the subscription. A fold is
	// final: a later, longer grace or a lowered floor cannot reopen it.
	runs: Vec<Run>,
	grace: Duration,
	streams: u64,
	// Streams whose header arrived and that are still being read.
	active: u64,
}

impl Default for Tail {
	fn default() -> Self {
		Self {
			runs: Vec::new(),
			grace: GRACE,
			streams: 0,
			active: 0,
		}
	}
}

impl Tail {
	/// A tail that gives up on a missing group after `grace`.
	pub fn new(grace: Duration) -> Self {
		Self {
			grace,
			..Self::default()
		}
	}

	/// Change the grace, for a subscription whose max delay changed.
	pub fn set_grace(&mut self, grace: Duration) {
		self.grace = grace;
	}

	/// Record groups as accounted for without a stream: dropped, or sent as a datagram.
	pub fn account(&mut self, groups: Range<u64>, now: Instant) {
		if groups.is_empty() {
			return;
		}

		// Every run the insert overlaps or touches merges with it into one.
		let first = self.runs.partition_point(|run| run.groups.end < groups.start);
		let last = self.runs.partition_point(|run| run.groups.start <= groups.end);
		let merged = match &self.runs[first..last] {
			[] => Run {
				groups,
				// Splitting a gap leaves both halves as late as it was. A run past every
				// other one opens a new gap.
				since: self.runs.get(first).map_or(now, |above| above.since),
			},
			[head, .., tail] => Run {
				groups: head.groups.start.min(groups.start)..tail.groups.end.max(groups.end),
				since: head.since,
			},
			[only] => Run {
				groups: only.groups.start.min(groups.start)..only.groups.end.max(groups.end),
				since: only.since,
			},
		};
		self.runs.splice(first..last, [merged]);
		self.expire(now);
	}

	/// Restart the age of every gap reaching into `groups`, which the demand newly asks for.
	pub fn demand(&mut self, groups: Range<u64>, now: Instant) {
		if groups.is_empty() {
			return;
		}
		let mut below = 0;
		for run in &mut self.runs {
			if below < groups.end && groups.start < run.groups.start {
				run.since = now;
			}
			below = run.groups.end;
		}
	}

	/// Fold every gap older than the grace into the runs around it.
	pub fn expire(&mut self, now: Instant) {
		let grace = self.grace;
		self.runs.dedup_by(|run, below| {
			let expired = now.duration_since(run.since) >= grace;
			if expired {
				below.groups.end = run.groups.end;
			}
			expired
		});
	}

	/// Whether every group in `groups` is accounted for.
	pub fn covers(&self, groups: Range<u64>) -> bool {
		// Runs merge on insert, so one run covers the span or none does.
		groups.is_empty()
			|| self
				.runs
				.iter()
				.any(|run| run.groups.start <= groups.start && groups.end <= run.groups.end)
	}

	/// Data streams received, whether they finished or were reset.
	pub fn streams(&self) -> u64 {
		self.streams
	}
}

/// A data stream whose header arrived, holding its subscription's end open until dropped.
pub(crate) struct Reading {
	tail: Option<kio::Producer<Tail>>,
	active: bool,
}

impl Reading {
	/// Count a data stream, accounting for `group` when it names one (a fill does not).
	pub fn open(tail: &kio::Producer<Tail>, group: Option<u64>, now: Instant) -> Self {
		let Ok(mut state) = tail.write() else {
			return Self {
				tail: None,
				active: false,
			};
		};
		state.streams += 1;
		state.active += 1;
		if let Some(group) = group {
			state.account(group..group.saturating_add(1), now);
		}
		Self {
			tail: Some(tail.clone()),
			active: true,
		}
	}

	/// Stop holding the end open while the stream waits on another one, which holds it
	/// open itself if its header arrived.
	pub fn park(&mut self) {
		self.set_active(false);
	}

	/// Hold the end open again once the stream resumes reading.
	pub fn resume(&mut self) {
		self.set_active(true);
	}

	fn set_active(&mut self, active: bool) {
		if self.active == active {
			return;
		}
		self.active = active;
		if let Some(tail) = &self.tail
			&& let Ok(mut state) = tail.write()
		{
			match active {
				true => state.active += 1,
				false => state.active -= 1,
			}
		}
	}
}

impl Drop for Reading {
	fn drop(&mut self) {
		self.set_active(false);
	}
}

/// Waits out a subscription's tail: until the owed streams are accounted for, or the grace.
pub(crate) struct Settle {
	tail: kio::Consumer<Tail>,
	grace: Deadline,
}

impl Settle {
	/// Start waiting on `tail`, giving up on missing streams after its grace.
	pub fn new(runtime: &crate::time::Clock, tail: kio::Consumer<Tail>) -> Self {
		let grace = tail.read().grace;
		Self {
			tail,
			grace: Deadline::after(runtime, grace),
		}
	}

	/// Ready once no stream is being read and either `complete` holds or the grace
	/// expired, or once the subscription is gone.
	pub fn poll(&mut self, waiter: &kio::Waiter, mut complete: impl FnMut(&Tail) -> bool) -> Poll<()> {
		let expired = self.grace.poll(waiter).is_ready();
		self.tail
			.poll(waiter, |tail| match tail.active == 0 && (expired || complete(tail)) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn groups(tail: &Tail) -> Vec<Range<u64>> {
		tail.runs.iter().map(|run| run.groups.clone()).collect()
	}

	#[test]
	fn runs_merge_across_gaps() {
		let now = Instant::now();
		let mut tail = Tail::default();
		tail.account(0..1, now);
		tail.account(2..3, now);
		assert!(!tail.covers(0..3), "group 1 is missing");
		assert_eq!(groups(&tail), vec![0..1, 2..3]);

		tail.account(1..2, now);
		assert!(tail.covers(0..3) && tail.runs.len() == 1, "adjacent runs merge");

		tail.account(5..7, now);
		tail.account(9..10, now);
		tail.account(4..9, now);
		assert_eq!(groups(&tail), vec![0..3, 4..10], "one insert swallows several runs");
		assert!(tail.covers(4..10));
		assert!(!tail.covers(2..5));
		assert!(tail.covers(7..7), "an empty range is always covered");
	}

	/// A gap older than the grace folds away, so a lossy track keeps only the gaps it can
	/// still wait for.
	#[test]
	fn a_gap_past_the_grace_folds_away() {
		let start = Instant::now();
		let mut tail = Tail::new(GRACE);
		tail.account(0..1, start);
		tail.account(2..3, start);
		tail.account(4..5, start + GRACE / 2);
		assert_eq!(groups(&tail), vec![0..1, 2..3, 4..5]);

		// Splitting the younger gap keeps it as late as it was.
		tail.account(6..7, start + GRACE / 2);
		tail.account(8..9, start + GRACE);
		assert_eq!(
			groups(&tail),
			vec![0..3, 4..5, 6..7, 8..9],
			"only the oldest gap expired"
		);

		tail.expire(start + GRACE * 2);
		assert_eq!(groups(&tail), vec![0..9]);
		assert!(tail.covers(0..9));
	}

	/// Splitting a gap leaves both halves with its age.
	#[test]
	fn a_split_gap_keeps_its_age() {
		let start = Instant::now();
		let mut tail = Tail::new(GRACE);
		tail.account(0..1, start);
		tail.account(9..10, start);
		tail.account(5..6, start + GRACE / 2);
		assert_eq!(groups(&tail), vec![0..1, 5..6, 9..10]);

		tail.expire(start + GRACE);
		assert_eq!(groups(&tail), vec![0..10], "both halves opened at the start");
	}

	/// A gap the demand newly asks for is late only from then, however old the run above it.
	#[test]
	fn a_lowered_floor_restarts_the_gap_age() {
		let start = Instant::now();
		let mut tail = Tail::new(GRACE);
		tail.account(3..4, start);
		tail.account(9..10, start);

		let lowered = start + GRACE * 2;
		tail.demand(1..3, lowered);
		tail.account(1..2, lowered);
		assert_eq!(
			groups(&tail),
			vec![1..2, 3..10],
			"only the gap the floor reached restarts"
		);
		assert!(!tail.covers(1..4));

		tail.expire(lowered + GRACE);
		assert!(tail.covers(1..10));
	}

	/// The grace gives up on streams that never arrived, never on one still being read:
	/// its group, or the END_OF_TRACK it carries, still belongs to the track.
	#[moq_net_sim::test]
	async fn the_grace_waits_for_a_stream_being_read() {
		let runtime = crate::time::Clock::sim();
		let tail = kio::Producer::new(Tail::default());
		let reading = Reading::open(&tail, Some(0), runtime.now());
		let mut settle = Settle::new(&runtime, tail.consume());
		let mut settled = std::pin::pin!(kio::wait(|waiter| settle.poll(waiter, |_| false)));

		moq_net_sim::sleep(GRACE * 2).await;
		assert!(
			futures::poll!(settled.as_mut()).is_pending(),
			"a stream is still being read"
		);

		drop(reading);
		settled.await;
	}

	/// A stream parked on another one does not hold the end open.
	#[moq_net_sim::test]
	async fn a_parked_stream_does_not_hold_the_end() {
		let runtime = crate::time::Clock::sim();
		let tail = kio::Producer::new(Tail::default());
		let mut reading = Reading::open(&tail, None, runtime.now());
		reading.park();
		let mut settle = Settle::new(&runtime, tail.consume());
		kio::wait(|waiter| settle.poll(waiter, |tail| tail.streams() == 1)).await;

		reading.resume();
		assert_eq!(tail.read().active, 1);
		drop(reading);
		assert_eq!(tail.read().active, 0);
	}
}

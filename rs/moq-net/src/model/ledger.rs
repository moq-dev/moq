//! The sequences a feed has accounted for, and how long each gap between them is waited for.
//!
//! A subscription's tail (the session's `tail::Tail`) and a track's record of live arrivals
//! both keep one, so they give up on a missing group by the same rule.

use std::{ops::Range, time::Duration};

use crate::time::Instant;

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

/// The sequences accounted for, and how late each gap between them is.
#[derive(Debug)]
pub(crate) struct Ledger {
	// Disjoint, sorted, non-adjacent runs of accounted sequences. A gap older than the grace
	// can no longer be waited for, so it folds into the runs around it, which bounds this by
	// the gaps opened within the grace rather than every gap ever seen. A fold is final: a
	// later, longer grace or a lowered floor cannot reopen it.
	runs: Vec<Run>,
	grace: Duration,
}

impl Default for Ledger {
	fn default() -> Self {
		Self::new(GRACE)
	}
}

impl Ledger {
	/// A ledger that gives up on a missing group after `grace`.
	pub fn new(grace: Duration) -> Self {
		Self {
			runs: Vec::new(),
			grace,
		}
	}

	/// Change the grace, for a subscription whose max delay changed.
	pub fn set_grace(&mut self, grace: Duration) {
		self.grace = grace;
	}

	/// Record `groups` as accounted for. Gaps fold only on [`Self::expire`].
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

	/// Whether a gap separates two runs, so [`Self::expire`] has anything to fold.
	pub fn has_gaps(&self) -> bool {
		self.runs.len() > 1
	}

	/// The lowest sequence accounted for.
	pub fn first(&self) -> Option<u64> {
		self.runs.first().map(|run| run.groups.start)
	}

	/// Whether every group in `groups` is accounted for.
	pub fn covers(&self, groups: Range<u64>) -> bool {
		// Runs merge on insert, so one run covers the span or none does.
		groups.is_empty()
			|| self
				.runs
				.get(self.runs.partition_point(|run| run.groups.end <= groups.start))
				.is_some_and(|run| run.groups.start <= groups.start && groups.end <= run.groups.end)
	}
}

// What only a session's tail reads.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
impl Ledger {
	/// How long a gap is waited for once a later group opened it.
	pub fn grace(&self) -> Duration {
		self.grace
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

	/// When the ledger gives up on a missing `group`: once the grace has passed since a later
	/// group opened the gap below it. `None` while it is accounted for, or while no later
	/// group has arrived, since nothing says it is late yet.
	pub fn folds_at(&self, group: u64) -> Option<Instant> {
		let above = self
			.runs
			.get(self.runs.partition_point(|run| run.groups.end <= group))?;
		match above.groups.start <= group {
			true => None,
			false => above.since.checked_add(self.grace),
		}
	}

	/// Gaps whose age bounds a held fetch, including the gap before the first arrival.
	pub fn folds(&self) -> impl Iterator<Item = (Range<u64>, Instant)> + '_ {
		let mut below = 0;
		self.runs.iter().filter_map(move |run| {
			let gap = below..run.groups.start;
			below = run.groups.end;
			if gap.is_empty() {
				return None;
			}
			Some((gap, run.since.checked_add(self.grace)?))
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Account `groups` and fold what aged out, as each arrival does.
	fn arrive(ledger: &mut Ledger, groups: Range<u64>, now: Instant) {
		ledger.account(groups, now);
		ledger.expire(now);
	}

	fn groups(ledger: &Ledger) -> Vec<Range<u64>> {
		ledger.runs.iter().map(|run| run.groups.clone()).collect()
	}

	#[test]
	fn runs_merge_across_gaps() {
		let now = Instant::now();
		let mut ledger = Ledger::default();
		arrive(&mut ledger, 0..1, now);
		arrive(&mut ledger, 2..3, now);
		assert!(!ledger.covers(0..3), "group 1 is missing");
		assert_eq!(groups(&ledger), vec![0..1, 2..3]);

		arrive(&mut ledger, 1..2, now);
		assert!(ledger.covers(0..3) && ledger.runs.len() == 1, "adjacent runs merge");

		arrive(&mut ledger, 5..7, now);
		arrive(&mut ledger, 9..10, now);
		arrive(&mut ledger, 4..9, now);
		assert_eq!(groups(&ledger), vec![0..3, 4..10], "one insert swallows several runs");
		assert!(ledger.covers(4..10));
		assert!(!ledger.covers(2..5));
		assert!(ledger.covers(7..7), "an empty range is always covered");
	}

	/// A gap older than the grace folds away, so a lossy track keeps only the gaps it can
	/// still wait for.
	#[test]
	fn a_gap_past_the_grace_folds_away() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 0..1, start);
		arrive(&mut ledger, 2..3, start);
		arrive(&mut ledger, 4..5, start + GRACE / 2);
		assert_eq!(groups(&ledger), vec![0..1, 2..3, 4..5]);

		// Splitting the younger gap keeps it as late as it was.
		arrive(&mut ledger, 6..7, start + GRACE / 2);
		arrive(&mut ledger, 8..9, start + GRACE);
		assert_eq!(
			groups(&ledger),
			vec![0..3, 4..5, 6..7, 8..9],
			"only the oldest gap expired"
		);

		ledger.expire(start + GRACE * 2);
		assert_eq!(groups(&ledger), vec![0..9]);
		assert!(ledger.covers(0..9));
	}

	/// A missing group is given up on a grace after a later one opened its gap, and not
	/// before anything later has arrived.
	#[test]
	fn a_missing_group_folds_a_grace_after_its_gap_opened() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 0..1, start);
		assert_eq!(ledger.folds_at(0), None, "accounted for");
		assert_eq!(ledger.folds_at(1), None, "nothing later arrived");

		arrive(&mut ledger, 2..3, start + GRACE / 2);
		assert_eq!(ledger.folds_at(1), Some(start + GRACE / 2 + GRACE));
		assert_eq!(ledger.folds_at(3), None);
	}

	/// Splitting a gap leaves both halves with its age.
	#[test]
	fn a_split_gap_keeps_its_age() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 0..1, start);
		arrive(&mut ledger, 9..10, start);
		arrive(&mut ledger, 5..6, start + GRACE / 2);
		assert_eq!(groups(&ledger), vec![0..1, 5..6, 9..10]);

		ledger.expire(start + GRACE);
		assert_eq!(groups(&ledger), vec![0..10], "both halves opened at the start");
	}

	/// A gap the demand newly asks for is late only from then, however old the run above it.
	#[test]
	fn a_lowered_floor_restarts_the_gap_age() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 3..4, start);
		arrive(&mut ledger, 9..10, start);

		let lowered = start + GRACE * 2;
		ledger.demand(1..3, lowered);
		arrive(&mut ledger, 1..2, lowered);
		assert_eq!(
			groups(&ledger),
			vec![1..2, 3..10],
			"only the gap the floor reached restarts"
		);
		assert!(!ledger.covers(1..4));

		ledger.expire(lowered + GRACE);
		assert!(ledger.covers(1..10));
	}
}

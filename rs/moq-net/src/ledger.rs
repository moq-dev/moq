//! The sequences a feed has accounted for, and how long each gap between them is waited for.
//!
//! A subscription's tail (the session's `tail::Tail`) and a track's record of live arrivals
//! both keep one, so they give up on a missing group by the same rule.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::Range,
	time::Duration,
};

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

/// A run of accounted sequences, keyed by its inclusive start.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
	end: u64,
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
	runs: BTreeMap<u64, Run>,
	// Demand can restart a gap below an older one, so sequence order is not age order.
	// Index both: one due gap must not make every arrival scan all the younger ones.
	ages: BTreeSet<(Instant, u64)>,
	grace: Duration,
	changes: Changes,
}

/// Revisions of the current runs, indexed in both orders. Coalescing or removing a run
/// removes its revision too, so an idle reader never retains a history of mutations.
#[derive(Debug, Default)]
pub(crate) struct Changes {
	runs: BTreeMap<u64, (u64, u64)>,
	changed: BTreeMap<u64, u64>,
	revision: u64,
}

impl Changes {
	pub fn revision(&self) -> u64 {
		self.revision
	}

	pub fn remove(&mut self, start: u64) {
		if let Some((_, revision)) = self.runs.remove(&start) {
			self.changed.remove(&revision);
		}
	}

	pub fn insert(&mut self, run: Range<u64>) {
		self.remove(run.start);
		self.revision = self.revision.checked_add(1).expect("run revisions exhausted");
		self.runs.insert(run.start, (run.end, self.revision));
		self.changed.insert(self.revision, run.start);
	}

	pub fn since(&self, revision: u64) -> impl Iterator<Item = Range<u64>> + '_ {
		self.changed
			.range((std::ops::Bound::Excluded(revision), std::ops::Bound::Unbounded))
			.map(|(_, start)| {
				let (end, _) = self.runs.get(start).expect("a changed run stays indexed");
				*start..*end
			})
	}
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
			runs: BTreeMap::new(),
			ages: BTreeSet::new(),
			grace,
			changes: Changes::default(),
		}
	}

	/// Change the grace, for a subscription whose max delay changed.
	pub fn set_grace(&mut self, grace: Duration) {
		self.grace = grace;
	}

	fn remove(&mut self, start: u64) -> Run {
		self.changes.remove(start);
		let run = self.runs.remove(&start).expect("an indexed run");
		self.ages.remove(&(run.since, start));
		run
	}

	/// Record `groups` as accounted for. Gaps fold only on [`Self::expire`].
	pub fn account(&mut self, groups: Range<u64>, now: Instant) {
		if groups.is_empty() {
			return;
		}
		// Keep the lower run in place when extending it: the in-order path changes
		// only its end, without reallocating either index or changing its gap's age.
		let below = self
			.runs
			.range(..=groups.start)
			.next_back()
			.filter(|(_, run)| run.end >= groups.start)
			.map(|(&start, run)| (start, run.end));
		if below.is_some_and(|(_, end)| groups.end <= end) {
			return;
		}
		let first = below.map_or(groups.start, |(start, _)| start);
		// Splitting a gap inherits the upper run's age; only an arrival above every
		// run opens a fresh gap.
		let since = self.runs.range(first..).next().map_or(now, |(_, run)| run.since);
		let mut end = groups.end.max(below.map_or(groups.end, |(_, end)| end));
		let from = match below {
			Some(_) => std::ops::Bound::Excluded(first),
			None => std::ops::Bound::Included(first),
		};
		while let Some((&start, run)) = self.runs.range((from, std::ops::Bound::Included(end))).next() {
			end = end.max(run.end);
			self.remove(start);
		}
		match below {
			Some(_) => self.runs.get_mut(&first).expect("the lower run stayed indexed").end = end,
			None => {
				self.ages.insert((since, first));
				self.runs.insert(first, Run { end, since });
			}
		}
		self.changes.insert(first..end);
	}

	/// Account a live arrival, refreshing a cached incarnation even if its sequence was
	/// accounted before. Its gap ages stay unchanged, but readers must revisit a refusal.
	pub fn arrive(&mut self, sequence: u64, now: Instant) {
		let revision = self.changes.revision();
		self.account(sequence..sequence.saturating_add(1), now);
		if self.changes.revision() == revision
			&& let Some((&start, run)) = self.runs.range(..=sequence).next_back()
			&& sequence < run.end
		{
			self.changes.insert(start..run.end);
		}
	}

	/// Fold every gap older than the grace into the runs around it.
	pub fn expire(&mut self, now: Instant) {
		let Some((&first, _)) = self.runs.first_key_value() else {
			return;
		};
		// There is no lower run to fold the leading gap into. Keep its age for a
		// later lowered floor, but do not let it hide due gaps elsewhere in the index.
		while let Some(&(since, start)) = self.ages.iter().find(|(_, start)| *start != first) {
			if now.duration_since(since) < self.grace {
				break;
			}
			let run = self.remove(start);
			let (&lower, previous) = self.runs.range_mut(..start).next_back().expect("not the first run");
			previous.end = run.end;
			self.changes.insert(lower..run.end);
		}
	}

	/// Whether a gap separates two runs, so [`Self::expire`] has anything to fold.
	pub fn has_gaps(&self) -> bool {
		self.runs.len() > 1
	}

	/// The lowest sequence accounted for.
	pub fn first(&self) -> Option<u64> {
		self.runs.first_key_value().map(|(start, _)| *start)
	}

	/// The exclusive end of the accounted run holding `sequence`, `None` while it is not.
	pub fn run_end(&self, sequence: u64) -> Option<u64> {
		self.runs
			.range(..=sequence)
			.next_back()
			.filter(|(_, run)| sequence < run.end)
			.map(|(_, run)| run.end)
	}

	/// The accounted runs overlapping `groups`, clipped to it, lowest first.
	pub fn accounted(&self, groups: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
		// From the run holding the first group, if one does.
		let lower = self
			.runs
			.range(..=groups.start)
			.next_back()
			.filter(|(_, run)| groups.start < run.end)
			.map_or(groups.start, |(&start, _)| start);
		self.runs
			.range(lower..)
			.take_while(move |(start, _)| **start < groups.end)
			.map(move |(&start, run)| start.max(groups.start)..run.end.min(groups.end))
	}

	pub fn changes(&self) -> &Changes {
		&self.changes
	}

	/// Whether every group in `groups` is accounted for.
	pub fn covers(&self, groups: Range<u64>) -> bool {
		groups.is_empty() || self.run_end(groups.start).is_some_and(|end| groups.end <= end)
	}
}

// Gap timing shared by a subscription tail and its track's live arrivals.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
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
		for (&start, run) in &mut self.runs {
			if below < groups.end && groups.start < start {
				self.ages.remove(&(run.since, start));
				run.since = now;
				self.ages.insert((now, start));
				self.changes.insert(start..run.end);
			}
			below = run.end;
		}
	}

	/// When the ledger gives up on a missing `group`: once the grace has passed since a later
	/// group opened the gap below it. `None` while it is accounted for, or while no later
	/// group has arrived, since nothing says it is late yet.
	pub fn folds_at(&self, group: u64, grace: Duration) -> Option<Instant> {
		if self.run_end(group).is_some() {
			return None;
		}
		self.runs.range(group..).next()?.1.since.checked_add(grace)
	}

	/// The gaps within `groups`, each with when a later arrival opened it: `None` for the
	/// one past the last run, which nothing has opened yet.
	pub fn gaps(&self, groups: Range<u64>) -> impl Iterator<Item = (Range<u64>, Option<Instant>)> + '_ {
		let first = self
			.runs
			.range(..=groups.start)
			.next_back()
			.filter(|(_, run)| run.end > groups.start)
			.map_or(groups.start, |(start, _)| *start);
		let mut below = groups.start;
		self.runs
			.range(first..)
			.map(|(&start, run)| (start..run.end, Some(run.since)))
			.chain(std::iter::once((u64::MAX..u64::MAX, None)))
			.map_while(move |(run, since)| {
				let gap = below..run.start.min(groups.end);
				below = below.max(run.end);
				(gap.start < groups.end).then_some((gap, since))
			})
			.filter(|(gap, _)| !gap.is_empty())
	}

	/// Gaps whose age bounds a held fetch, including the gap before the first arrival.
	pub fn folds(&self, grace: Duration) -> impl Iterator<Item = (Range<u64>, Instant)> + '_ {
		let mut below = 0;
		self.runs.iter().filter_map(move |(&start, run)| {
			let gap = below..start;
			below = run.end;
			if gap.is_empty() {
				return None;
			}
			Some((gap, run.since.checked_add(grace)?))
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn changes_keep_only_current_runs_across_coalescing_and_expiry() {
		let now = Instant::ZERO;
		let mut ledger = Ledger::new(GRACE);
		ledger.account(0..1, now);
		ledger.account(4..5, now);
		ledger.account(8..9, now);
		let revision = ledger.changes().revision();
		ledger.account(2..3, now);
		assert_eq!(ledger.changes().since(revision).collect::<Vec<_>>(), vec![2..3]);
		let revision = ledger.changes().revision();
		ledger.account(1..4, now);
		assert_eq!(ledger.changes().since(revision).collect::<Vec<_>>(), vec![0..5]);
		ledger.expire(now + GRACE);
		assert_eq!(ledger.changes().since(0).collect::<Vec<_>>(), vec![0..9]);
		assert_eq!(ledger.changes.changed.len(), ledger.runs.len());
		let revision = ledger.changes().revision();
		ledger.account(2..7, now + GRACE);
		assert_eq!(ledger.changes().revision(), revision, "already accounted is unchanged");
	}

	/// Account `groups` and fold what aged out, as each arrival does.
	fn arrive(ledger: &mut Ledger, groups: Range<u64>, now: Instant) {
		ledger.account(groups, now);
		ledger.expire(now);
	}

	fn groups(ledger: &Ledger) -> Vec<Range<u64>> {
		assert_eq!(
			ledger.ages,
			ledger.runs.iter().map(|(&start, run)| (run.since, start)).collect()
		);
		ledger.runs.iter().map(|(&start, run)| start..run.end).collect()
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

	/// The gaps within a span, each with the age of the run that opened it, and the one past
	/// every run with none.
	#[test]
	fn gaps_within_a_span() {
		let now = Instant::now();
		let later = now + GRACE;
		let mut ledger = Ledger::default();
		assert_eq!(ledger.gaps(2..5).collect::<Vec<_>>(), vec![(2..5, None)]);
		arrive(&mut ledger, 2..3, now);
		arrive(&mut ledger, 5..6, later);
		assert_eq!(
			ledger.gaps(0..8).collect::<Vec<_>>(),
			vec![(0..2, Some(now)), (3..5, Some(later)), (6..8, None)]
		);
		assert_eq!(ledger.gaps(4..6).collect::<Vec<_>>(), vec![(4..5, Some(later))]);
		assert_eq!(ledger.gaps(5..6).count(), 0, "covered");
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
		assert_eq!(ledger.folds_at(0, GRACE), None, "accounted for");
		assert_eq!(ledger.folds_at(1, GRACE), None, "nothing later arrived");

		arrive(&mut ledger, 2..3, start + GRACE / 2);
		assert_eq!(ledger.folds_at(1, GRACE), Some(start + GRACE / 2 + GRACE));
		assert_eq!(ledger.folds_at(3, GRACE), None);
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
	#[test]
	fn demand_can_move_an_older_gap_after_a_newer_one() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 0..1, start);
		arrive(&mut ledger, 4..5, start);
		arrive(&mut ledger, 8..9, start + GRACE / 4);
		ledger.demand(1..4, start + GRACE / 2);

		ledger.expire(start + GRACE + GRACE / 4);
		assert_eq!(groups(&ledger), vec![0..1, 4..9]);
		assert_eq!(ledger.folds_at(1, GRACE), Some(start + GRACE + GRACE / 2));
		ledger.expire(start + GRACE + GRACE / 2);
		assert_eq!(groups(&ledger), vec![0..9]);
	}

	#[test]
	fn changing_grace_uses_current_gap_ages_without_reopening_folds() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 0..1, start);
		arrive(&mut ledger, 2..3, start);
		ledger.set_grace(GRACE * 2);
		ledger.expire(start + GRACE);
		assert_eq!(groups(&ledger), vec![0..1, 2..3]);

		ledger.set_grace(GRACE / 2);
		ledger.expire(start + GRACE);
		assert_eq!(groups(&ledger), vec![0..3]);
		ledger.set_grace(GRACE * 2);
		arrive(&mut ledger, 4..5, start + GRACE);
		ledger.expire(start + GRACE * 2);
		assert_eq!(groups(&ledger), vec![0..3, 4..5]);
	}

	#[test]
	fn a_former_first_run_keeps_its_age_when_a_lower_run_arrives() {
		let start = Instant::now();
		let mut ledger = Ledger::new(GRACE);
		arrive(&mut ledger, 10..11, start);
		ledger.expire(start + GRACE * 2);
		assert_eq!(groups(&ledger), vec![10..11]);
		arrive(&mut ledger, 0..1, start + GRACE * 2);
		assert_eq!(groups(&ledger), vec![0..11]);
	}
	#[test]
	fn one_gap_expires_per_arrival_without_retaining_older_gaps() {
		let start = Instant::now();
		for retained in [0u64, 1, 4, 16] {
			let mut ledger = Ledger::new(Duration::from_millis(retained));
			for arrival in 0..retained * 4 + 8 {
				arrive(
					&mut ledger,
					arrival * 2..arrival * 2 + 1,
					start + Duration::from_millis(arrival),
				);
				let runs = groups(&ledger);
				assert_eq!(runs.len() as u64, (arrival + 1).min(retained + 1));
				assert_eq!(runs[0], 0..arrival.saturating_sub(retained) * 2 + 1);
			}
		}
	}
}

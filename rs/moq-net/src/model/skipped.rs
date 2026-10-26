//! The cached groups a reader gave up as stale, so a wider budget can offer them again.

use std::ops::RangeInclusive;
use std::time::Duration;

use rangemap::RangeInclusiveMap;

/// The groups a reader skipped as stale and never handed out, with the budget each was
/// judged under, as runs of consecutive sequences.
///
/// Only a widening reverses a stale verdict, so a run comes back once the budget grows
/// past the one it was judged under. A run is kept only while one of its groups is cached:
/// a group gone from the cache cannot be offered again, so its verdict is final.
///
/// Boxed on the first skip: most readers never skip one, and the cursor holding it is hot.
#[derive(Default)]
pub(super) struct Skipped(Option<Box<Runs>>);

#[derive(Default)]
struct Runs {
	runs: RangeInclusiveMap<u64, Duration>,
	/// At most the narrowest budget any run was judged under, so a poll whose budget did
	/// not widen past it skips the scan.
	narrowest: Duration,
}

impl Skipped {
	/// Record `sequence` as skipped under `budget`.
	pub fn insert(&mut self, sequence: u64, budget: Duration) {
		let skipped = self.0.get_or_insert_default();
		if skipped.runs.is_empty() || budget < skipped.narrowest {
			skipped.narrowest = budget;
		}
		skipped.runs.insert(sequence..=sequence, budget);
	}

	/// Forget `sequence`, now handed out.
	pub fn remove(&mut self, sequence: u64) {
		if let Some(skipped) = &mut self.0
			&& !skipped.runs.is_empty()
		{
			skipped.runs.remove(sequence..=sequence);
		}
	}

	/// The runs judged under a budget narrower than `budget`, lowest first, now judged
	/// under `budget`: each stays until its group is handed out or skipped again, so a
	/// newer group handed out first never folds it into the delivered floor.
	pub fn widened(&mut self, budget: Duration) -> Vec<RangeInclusive<u64>> {
		let Some(skipped) = self.0.as_mut().filter(|skipped| !skipped.runs.is_empty()) else {
			return Vec::new();
		};
		if budget <= skipped.narrowest {
			return Vec::new();
		}
		let widened: Vec<_> = skipped
			.runs
			.iter()
			.filter(|(_, judged)| **judged < budget)
			.map(|(run, _)| run.clone())
			.collect();
		for run in &widened {
			skipped.runs.insert(run.clone(), budget);
		}
		// Every run left was judged under `budget` or wider.
		skipped.narrowest = budget;
		widened
	}

	/// Whether no run is recorded.
	pub fn is_empty(&self) -> bool {
		self.0.as_ref().is_none_or(|skipped| skipped.runs.is_empty())
	}

	/// The lowest sequence skipped.
	pub fn lowest(&self) -> Option<u64> {
		let skipped = self.0.as_ref()?;
		skipped.runs.first_range_value().map(|(run, _)| *run.start())
	}

	/// Drop the lowest runs `gone` says no longer hold a cached group, and every such run
	/// once the runs outnumber `cached` twice over, so each kept run holds a distinct cached
	/// group and pruning costs O(1) per skip amortized.
	pub fn prune(&mut self, cached: usize, mut gone: impl FnMut(&RangeInclusive<u64>) -> bool) {
		let Some(skipped) = &mut self.0 else {
			return;
		};
		while let Some((run, _)) = skipped.runs.first_range_value() {
			let run = run.clone();
			if !gone(&run) {
				break;
			}
			skipped.runs.remove(run);
		}
		if skipped.runs.len() > cached.saturating_mul(2) {
			let gone: Vec<_> = skipped
				.runs
				.iter()
				.map(|(run, _)| run.clone())
				.filter(&mut gone)
				.collect();
			for run in gone {
				skipped.runs.remove(run);
			}
		}
	}

	/// How many runs it holds.
	#[cfg(test)]
	pub fn runs(&self) -> usize {
		self.0.as_ref().map_or(0, |skipped| skipped.runs.len())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	const NARROW: Duration = Duration::from_secs(1);
	const WIDE: Duration = Duration::from_secs(5);

	#[test]
	fn a_widening_takes_only_the_runs_judged_narrower() {
		let mut skipped = Skipped::default();
		skipped.insert(1, NARROW);
		skipped.insert(2, NARROW);
		skipped.insert(4, WIDE);
		assert_eq!(skipped.runs(), 2);
		assert_eq!(skipped.lowest(), Some(1));

		assert!(skipped.widened(NARROW).is_empty(), "the same budget reverses nothing");
		assert_eq!(skipped.widened(WIDE), vec![1..=2]);
		assert!(skipped.widened(WIDE).is_empty(), "a run judged at the budget stays");
		assert_eq!(skipped.lowest(), Some(1), "a widened run stays until it is handed out");
		// Rejudged under WIDE, the widened run now merges with the run already judged there.
		assert_eq!(skipped.widened(WIDE * 2), vec![1..=2, 4..=4]);
		skipped.remove(1);
		skipped.remove(2);
		assert_eq!(skipped.lowest(), Some(4));
	}

	#[test]
	fn a_hand_out_splits_a_run() {
		let mut skipped = Skipped::default();
		for sequence in 3..=5 {
			skipped.insert(sequence, NARROW);
		}
		skipped.remove(4);
		assert_eq!(skipped.widened(WIDE), vec![3..=3, 5..=5]);
	}

	#[test]
	fn prune_drops_runs_with_nothing_cached() {
		let mut skipped = Skipped::default();
		for sequence in [1, 3, 5, 7] {
			skipped.insert(sequence, NARROW);
		}
		// The lowest runs go until one still holds a group.
		skipped.prune(4, |run| *run.start() < 3);
		assert_eq!(skipped.lowest(), Some(3));
		assert_eq!(skipped.runs(), 3);

		// More runs than twice the cache prunes every one that holds nothing.
		skipped.prune(0, |run| *run.start() != 5);
		assert_eq!(skipped.widened(WIDE), vec![5..=5]);
	}
}

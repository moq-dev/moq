//! The cached groups a reader gave up as stale, so a wider budget can offer them again.

use std::ops::RangeInclusive;
use std::time::Duration;

use rangemap::{RangeInclusiveMap, RangeInclusiveSet};

use super::subscription::Budget;

/// The groups a reader skipped as stale and never handed out, with the budget each was
/// judged under, as runs of consecutive keys.
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
	/// Keys whose verdict may have become final since [`Skipped::take_released`]: no
	/// longer skipped, or never were. A reader settles only these, so a hand-out costs
	/// what changed rather than every run, which sparse keys never merge.
	released: RangeInclusiveSet<u64>,
}

impl Skipped {
	/// Record `key` as skipped under `budget`.
	pub fn insert(&mut self, key: u64, budget: Duration) {
		let skipped = self.0.get_or_insert_default();
		if skipped.runs.is_empty() || budget < skipped.narrowest {
			skipped.narrowest = budget;
		}
		skipped.runs.insert(key..=key, budget);
	}

	/// Forget `key`, now handed out or final.
	pub fn remove(&mut self, key: u64) {
		if let Some(skipped) = &mut self.0
			&& skipped.runs.contains_key(&key)
		{
			skipped.runs.remove(key..=key);
			skipped.released.insert(key..=key);
		}
	}

	/// Mark `key` as one whose verdict may be final, whether or not it is skipped.
	pub fn release(&mut self, key: u64) {
		self.0.get_or_insert_default().released.insert(key..=key);
	}

	/// The keys released since the last call: removed or pruned, or marked by
	/// [`Self::release`]. Each may still be skipped again since.
	pub fn take_released(&mut self) -> RangeInclusiveSet<u64> {
		match &mut self.0 {
			Some(skipped) => std::mem::take(&mut skipped.released),
			None => RangeInclusiveSet::new(),
		}
	}

	/// The runs judged under a budget narrower than `budget` gives them, lowest first, now
	/// judged under it: each stays until its group is handed out or skipped again, so a
	/// newer group handed out first never folds it into the delivered floor.
	pub fn widened(&mut self, budget: &Budget) -> Vec<RangeInclusive<u64>> {
		let Some(skipped) = self.0.as_mut().filter(|skipped| !skipped.runs.is_empty()) else {
			return Vec::new();
		};
		if budget.widest() <= skipped.narrowest {
			return Vec::new();
		}
		let widened: Vec<_> = (skipped.runs.iter())
			.flat_map(|(run, judged)| {
				let judged = *judged;
				budget
					.runs(*run.start()..run.end().saturating_add(1))
					.into_iter()
					.filter(move |(_, now)| judged < *now)
			})
			.map(|(run, now)| (run.start..=run.end - 1, now))
			.collect();
		for (run, now) in &widened {
			skipped.runs.insert(run.clone(), *now);
		}
		// Every run left was judged under this or wider.
		skipped.narrowest = skipped.runs.iter().map(|(_, judged)| *judged).min().unwrap_or_default();
		widened.into_iter().map(|(run, _)| run).collect()
	}

	/// Whether `key` is skipped under a budget narrower than `budget`, which widening to it
	/// offers again.
	pub fn widens(&self, key: u64, budget: Duration) -> bool {
		self.0
			.as_ref()
			.and_then(|skipped| skipped.runs.get(&key))
			.is_some_and(|judged| *judged < budget)
	}

	/// Whether no run is recorded.
	pub fn is_empty(&self) -> bool {
		self.0.as_ref().is_none_or(|skipped| skipped.runs.is_empty())
	}

	/// The parts of `groups` not skipped, which no wider budget offers again.
	pub fn gaps(&self, groups: RangeInclusive<u64>) -> Vec<RangeInclusive<u64>> {
		match &self.0 {
			Some(skipped) => skipped.runs.gaps(&groups).collect(),
			None => vec![groups],
		}
	}

	/// The lowest key skipped.
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
			skipped.runs.remove(run.clone());
			skipped.released.insert(run);
		}
		if skipped.runs.len() > cached.saturating_mul(2) {
			let gone: Vec<_> = skipped
				.runs
				.iter()
				.map(|(run, _)| run.clone())
				.filter(&mut gone)
				.collect();
			for run in gone {
				skipped.runs.remove(run.clone());
				skipped.released.insert(run);
			}
		}
	}

	/// One past the last key of the run holding `key`, if it was skipped.
	pub fn run_end(&self, key: u64) -> Option<u64> {
		let skipped = self.0.as_ref()?;
		skipped
			.runs
			.get_key_value(&key)
			.map(|(run, _)| run.end().saturating_add(1))
	}

	/// Forget every run below `floor`, which a fold settled.
	pub fn forget_below(&mut self, floor: u64) {
		if let Some(skipped) = &mut self.0
			&& let Some(below) = floor.checked_sub(1)
			&& !skipped.runs.is_empty()
		{
			skipped.runs.remove(0..=below);
		}
	}

	/// How many runs it holds.
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

		assert!(
			skipped.widened(&Budget::uniform(NARROW)).is_empty(),
			"the same budget reverses nothing"
		);
		assert_eq!(skipped.widened(&Budget::uniform(WIDE)), vec![1..=2]);
		assert!(
			skipped.widened(&Budget::uniform(WIDE)).is_empty(),
			"a run judged at the budget stays"
		);
		assert_eq!(skipped.lowest(), Some(1), "a widened run stays until it is handed out");
		// Rejudged under WIDE, the widened run now merges with the run already judged there.
		assert_eq!(skipped.widened(&Budget::uniform(WIDE * 2)), vec![1..=2, 4..=4]);
		skipped.remove(1);
		skipped.remove(2);
		assert_eq!(skipped.lowest(), Some(4));
	}

	#[test]
	fn a_hand_out_splits_a_run() {
		let mut skipped = Skipped::default();
		for key in 3..=5 {
			skipped.insert(key, NARROW);
		}
		skipped.remove(4);
		assert_eq!(skipped.widened(&Budget::uniform(WIDE)), vec![3..=3, 5..=5]);
	}

	#[test]
	fn prune_drops_runs_with_nothing_cached() {
		let mut skipped = Skipped::default();
		for key in [1, 3, 5, 7] {
			skipped.insert(key, NARROW);
		}
		// The lowest runs go until one still holds a group.
		skipped.prune(4, |run| *run.start() < 3);
		assert_eq!(skipped.lowest(), Some(3));
		assert_eq!(skipped.runs(), 3);

		// More runs than twice the cache prunes every one that holds nothing.
		skipped.prune(0, |run| *run.start() != 5);
		assert_eq!(skipped.widened(&Budget::uniform(WIDE)), vec![5..=5]);
	}

	#[test]
	fn released_holds_only_what_left_since_the_last_take() {
		let mut skipped = Skipped::default();
		assert!(skipped.take_released().is_empty());
		for key in [10, 20, 30] {
			skipped.insert(key, NARROW);
		}
		skipped.remove(20);
		skipped.remove(25);
		skipped.prune(3, |run| *run.start() < 20);
		skipped.release(u64::MAX);
		let released: Vec<_> = skipped.take_released().into_iter().collect();
		assert_eq!(
			released,
			[10..=10, 20..=20, u64::MAX..=u64::MAX],
			"never-skipped 25 is not"
		);
		assert!(skipped.take_released().is_empty(), "taken once");
		assert_eq!(skipped.lowest(), Some(30));
	}
}

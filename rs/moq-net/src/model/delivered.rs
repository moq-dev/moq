//! The group keys a reader handed out, so it never hands one out twice.

use std::collections::BTreeMap;

/// The group keys a reader handed out, as runs of consecutive keys.
///
/// A reader stops expecting a group once it is past its budget, and counts everything
/// below it as handed out (see [`Self::forget_below`]). The run starting at 0 is that
/// floor, so only the gaps above it cost memory. It never holds the reserved group
/// key `u64::MAX`, so one past any run fits.
#[derive(Default)]
pub(super) struct Delivered {
	/// First to last key, inclusive, never adjacent to each other.
	runs: BTreeMap<u64, u64>,
}

impl Delivered {
	/// Whether `key` was handed out, or is below the floor.
	pub fn contains(&self, key: u64) -> bool {
		self.runs
			.range(..=key)
			.next_back()
			.is_some_and(|(_, last)| key <= *last)
	}

	/// The first key above the handed-out prefix.
	pub fn floor(&self) -> u64 {
		self.runs.get(&0).map_or(0, |last| last + 1)
	}

	/// Whether every key in `groups` was handed out, or is below the floor.
	pub fn covers(&self, groups: std::ops::Range<u64>) -> bool {
		groups.is_empty()
			|| self
				.runs
				.range(..=groups.start)
				.next_back()
				.is_some_and(|(_, last)| groups.end - 1 <= *last)
	}

	/// Record `key` as handed out; `false` when it already was.
	pub fn insert(&mut self, key: u64) -> bool {
		if self.contains(key) {
			return false;
		}
		let first = key
			.checked_sub(1)
			.and_then(|below| self.runs.range(..=below).next_back())
			.filter(|(_, last)| **last + 1 == key)
			.map_or(key, |(first, _)| *first);
		let last = self.runs.remove(&(key + 1)).unwrap_or(key);
		self.runs.insert(first, last);
		true
	}

	/// Record every key in `groups` as handed out, joining the runs it touches.
	pub fn insert_run(&mut self, groups: std::ops::Range<u64>) {
		let Some(mut last) = groups.end.checked_sub(1).filter(|_| !groups.is_empty()) else {
			return;
		};
		let mut first = groups.start;
		// A run ending at or just below the first key joins it.
		if let Some((&start, &end)) = self.runs.range(..=first).next_back()
			&& end + 1 >= first
		{
			first = start;
			last = last.max(end);
		}
		// So does every run starting inside it or just past it.
		let joined: Vec<u64> = self.runs.range(first..=last + 1).map(|(&start, _)| start).collect();
		for start in joined {
			if let Some(end) = self.runs.remove(&start) {
				last = last.max(end);
			}
		}
		self.runs.insert(first, last);
	}

	/// The parts of `groups` not handed out, in order.
	pub fn gaps(&self, groups: std::ops::Range<u64>) -> Vec<std::ops::Range<u64>> {
		let mut gaps = Vec::new();
		let mut next = groups.start;
		let from = self
			.runs
			.range(..=groups.start)
			.next_back()
			.map_or(next, |(start, _)| *start);
		for (first, last) in self.runs.range(from..) {
			if next >= groups.end {
				return gaps;
			}
			if *first > next {
				gaps.push(next..(*first).min(groups.end));
			}
			next = next.max(last + 1);
		}
		if next < groups.end {
			gaps.push(next..groups.end);
		}
		gaps
	}

	/// The parts of `groups` none of `taken` handed out, in order.
	pub fn gaps_in_all(taken: &[&Self], groups: std::ops::Range<u64>) -> Vec<std::ops::Range<u64>> {
		let mut gaps = Vec::from([groups]);
		for taken in taken {
			gaps = gaps.into_iter().flat_map(|gap| taken.gaps(gap)).collect();
		}
		gaps
	}

	/// Count every key below `floor` as handed out.
	pub fn forget_below(&mut self, floor: u64) {
		let Some(below) = floor.checked_sub(1) else {
			return;
		};
		// Every run starting at or below the floor joins it, including one starting right at it.
		// No run starts at the reserved `u64::MAX`, so a floor there splits nothing off.
		let above = self.runs.split_off(&floor.saturating_add(1));
		let last = self.runs.last_key_value().map_or(below, |(_, last)| below.max(*last));
		self.runs = above;
		self.runs.insert(0, last);
	}

	/// The lowest key handed out above a gap: once it is past the reader's budget,
	/// so is the gap, since a timed track refuses a group whose start rewinds.
	pub fn above_gap(&self) -> Option<u64> {
		let mut runs = self.runs.keys();
		match runs.next()? {
			0 => runs.next().copied(),
			first => Some(*first),
		}
	}

	/// The last run starting below `key`.
	pub fn last_before(&self, key: u64) -> Option<u64> {
		self.runs.range(..key).next_back().map(|(start, _)| *start)
	}

	/// One past the last key of the run holding `key`, if it was handed out.
	pub fn run_end(&self, key: u64) -> Option<u64> {
		self.runs
			.range(..=key)
			.next_back()
			.filter(|(_, last)| key <= **last)
			.map(|(_, last)| last + 1)
	}

	/// How many runs it holds: one per gap, plus the floor.
	pub fn runs(&self) -> usize {
		self.runs.len()
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn runs(delivered: &Delivered) -> Vec<(u64, u64)> {
		delivered.runs.iter().map(|(first, last)| (*first, *last)).collect()
	}

	#[test]
	fn gaps_are_what_no_run_covers() {
		let mut delivered = Delivered::default();
		for key in [2, 4, 5, 9] {
			delivered.insert(key);
		}
		assert_eq!(delivered.gaps(0..12), [0..2, 3..4, 6..9, 10..12]);
		assert_eq!(delivered.gaps(4..6), []);
		assert_eq!(delivered.gaps(5..11), [6..9, 10..11]);
		delivered.insert(u64::MAX - 1);
		let gaps = delivered.gaps(u64::MAX - 2..u64::MAX);
		assert_eq!((gaps.len(), gaps.first()), (1, Some(&(u64::MAX - 2..u64::MAX - 1))));
	}

	#[test]
	fn insert_joins_neighbouring_runs() {
		let mut delivered = Delivered::default();
		assert!(delivered.insert(5));
		assert!(delivered.insert(7));
		assert_eq!(runs(&delivered), [(5, 5), (7, 7)]);
		assert!(delivered.insert(6));
		assert_eq!(runs(&delivered), [(5, 7)]);
		assert!(!delivered.insert(6));
		assert!(delivered.insert(4));
		assert!(delivered.insert(8));
		assert_eq!(runs(&delivered), [(4, 8)]);
		assert!(delivered.contains(4) && delivered.contains(8));
		assert!(!delivered.contains(3) && !delivered.contains(9));
		assert_eq!(delivered.floor(), 0, "a run above a gap is not the prefix");
		assert!(delivered.covers(4..9) && delivered.covers(9..9));
		assert!(!delivered.covers(3..5) && !delivered.covers(8..10));
	}

	#[test]
	fn insert_run_joins_every_run_it_touches() {
		let mut delivered = Delivered::default();
		for key in [2, 5, 6, 9, 14] {
			delivered.insert(key);
		}
		delivered.insert_run(3..5);
		assert_eq!(runs(&delivered), [(2, 6), (9, 9), (14, 14)], "adjacent on both sides");
		delivered.insert_run(8..13);
		assert_eq!(runs(&delivered), [(2, 6), (8, 12), (14, 14)], "a run inside it");
		delivered.insert_run(4..15);
		assert_eq!(runs(&delivered), [(2, 14)], "overlapping several");
		delivered.insert_run(7..7);
		assert_eq!(runs(&delivered), [(2, 14)], "an empty run");
		delivered.insert_run(u64::MAX - 1..u64::MAX);
		assert!(delivered.contains(u64::MAX - 1) && !delivered.contains(u64::MAX));
	}

	#[test]
	fn the_extremes_are_keys_too() {
		let mut delivered = Delivered::default();
		assert!(delivered.insert(0));
		assert!(delivered.insert(u64::MAX - 1));
		assert!(!delivered.insert(u64::MAX - 1));
		assert_eq!(runs(&delivered), [(0, 0), (u64::MAX - 1, u64::MAX - 1)]);
		delivered.forget_below(u64::MAX);
		assert_eq!(runs(&delivered), [(0, u64::MAX - 1)]);
		assert_eq!(delivered.floor(), u64::MAX);
	}

	#[test]
	fn forget_below_closes_the_gaps_under_the_floor() {
		let mut delivered = Delivered::default();
		for key in [3, 5, 6, 9, 12] {
			delivered.insert(key);
		}
		assert_eq!(delivered.above_gap(), Some(3));
		assert_eq!(delivered.last_before(0), None);
		assert_eq!(delivered.last_before(3), None);
		assert_eq!(delivered.last_before(5), Some(3));
		assert_eq!(delivered.last_before(9), Some(5));
		assert_eq!(delivered.last_before(13), Some(12));
		delivered.forget_below(0);
		assert_eq!(delivered.runs(), 4);

		// A floor at a run's start takes the run along.
		delivered.forget_below(5);
		assert_eq!(runs(&delivered), [(0, 6), (9, 9), (12, 12)]);
		assert_eq!(delivered.above_gap(), Some(9));
		assert!(delivered.contains(4), "below the floor counts as handed out");
		assert_eq!(delivered.floor(), 7);

		// A floor inside a gap keeps the run above it.
		delivered.forget_below(11);
		assert_eq!(runs(&delivered), [(0, 10), (12, 12)]);
		delivered.forget_below(13);
		assert_eq!(runs(&delivered), [(0, 12)]);
		assert_eq!(delivered.above_gap(), None);
	}
}

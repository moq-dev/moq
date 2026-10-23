//! The group sequences a reader handed out, so it never hands one out twice.

use std::collections::BTreeMap;

/// The group sequences a reader handed out, as runs of consecutive sequences.
///
/// A reader stops expecting a group once it is past its budget, and counts everything
/// below it as handed out (see [`Self::forget_below`]). The run starting at 0 is that
/// floor, so only the gaps above it cost memory.
#[derive(Default)]
pub(super) struct Delivered {
	/// First to last sequence, inclusive, never adjacent to each other.
	runs: BTreeMap<u64, u64>,
}

impl Delivered {
	/// Whether `sequence` was handed out, or is below the floor.
	pub fn contains(&self, sequence: u64) -> bool {
		self.runs
			.range(..=sequence)
			.next_back()
			.is_some_and(|(_, last)| sequence <= *last)
	}

	/// The first sequence above the handed-out prefix, or none once every sequence is past.
	pub fn floor(&self) -> Option<u64> {
		self.runs.get(&0).map_or(Some(0), |last| last.checked_add(1))
	}

	/// Whether every sequence in `groups` was handed out, or is below the floor.
	pub fn covers(&self, groups: std::ops::Range<u64>) -> bool {
		groups.is_empty()
			|| self
				.runs
				.range(..=groups.start)
				.next_back()
				.is_some_and(|(_, last)| groups.end - 1 <= *last)
	}

	/// Record `sequence` as handed out; `false` when it already was.
	pub fn insert(&mut self, sequence: u64) -> bool {
		if self.contains(sequence) {
			return false;
		}
		let first = sequence
			.checked_sub(1)
			.and_then(|below| self.runs.range(..=below).next_back())
			.filter(|(_, last)| **last + 1 == sequence)
			.map_or(sequence, |(first, _)| *first);
		let last = sequence
			.checked_add(1)
			.and_then(|above| self.runs.remove(&above))
			.unwrap_or(sequence);
		self.runs.insert(first, last);
		true
	}

	/// Count every sequence below `floor` as handed out.
	pub fn forget_below(&mut self, floor: u64) {
		let Some(below) = floor.checked_sub(1) else {
			return;
		};
		// Every run starting at or below the floor joins it, including one starting right at it.
		let above = match floor.checked_add(1) {
			Some(above) => self.runs.split_off(&above),
			None => BTreeMap::new(),
		};
		let last = self.runs.last_key_value().map_or(below, |(_, last)| below.max(*last));
		self.runs = above;
		self.runs.insert(0, last);
	}

	/// The lowest sequence handed out above a gap: once it is past the reader's budget,
	/// so is the gap, since a timed track refuses a group whose start rewinds.
	pub fn above_gap(&self) -> Option<u64> {
		let mut runs = self.runs.keys();
		match runs.next()? {
			0 => runs.next().copied(),
			first => Some(*first),
		}
	}

	/// The last run starting below `sequence`.
	pub fn last_before(&self, sequence: u64) -> Option<u64> {
		self.runs.range(..sequence).next_back().map(|(start, _)| *start)
	}

	/// How many runs it holds: one per gap, plus the floor.
	#[cfg(test)]
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
		assert_eq!(delivered.floor(), Some(0), "a run above a gap is not the prefix");
		assert!(delivered.covers(4..9) && delivered.covers(9..9));
		assert!(!delivered.covers(3..5) && !delivered.covers(8..10));
	}

	#[test]
	fn the_extremes_are_sequences_too() {
		let mut delivered = Delivered::default();
		assert!(delivered.insert(0));
		assert!(delivered.insert(u64::MAX));
		assert!(!delivered.insert(u64::MAX));
		assert_eq!(runs(&delivered), [(0, 0), (u64::MAX, u64::MAX)]);
		delivered.forget_below(u64::MAX);
		assert_eq!(runs(&delivered), [(0, u64::MAX)]);
		assert_eq!(delivered.floor(), None);
	}

	#[test]
	fn forget_below_closes_the_gaps_under_the_floor() {
		let mut delivered = Delivered::default();
		for sequence in [3, 5, 6, 9, 12] {
			delivered.insert(sequence);
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
		assert_eq!(delivered.floor(), Some(7));

		// A floor inside a gap keeps the run above it.
		delivered.forget_below(11);
		assert_eq!(runs(&delivered), [(0, 10), (12, 12)]);
		delivered.forget_below(13);
		assert_eq!(runs(&delivered), [(0, 12)]);
		assert_eq!(delivered.above_gap(), None);
	}
}

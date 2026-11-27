//! The group keys a reader handed out, so it never hands one out twice.

use std::collections::BTreeMap;

use rangemap::RangeSet;

use super::subscription::{Cover, minus};

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
	/// Span covering keys closed into `runs` by budget retirement ([`Self::fold_below`]) or
	/// debt skip compaction ([`Self::compact_skip`]) without having been handed out or reported.
	/// Kept as one bounding range (zero heap overhead) so debt folding never promotes unreported
	/// keys into the report floor.
	unreported: std::ops::Range<u64>,
	/// The keys a fold counted as handed out that no range held then, so no budget judged
	/// them: a range added later owes each it holds (see [`Self::reopen`]). Always within `runs`,
	/// and never more runs than [`Self::fold_below`] leaves.
	unjudged: RangeSet<u64>,
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
		if groups.is_empty() {
			return;
		}
		self.unjudged.remove(groups.clone());
		self.merge_run(groups);
	}

	/// Join `groups` into `runs` without clearing `unjudged` on keys already present.
	fn merge_run(&mut self, groups: std::ops::Range<u64>) {
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

	/// Compact a declared-skip gap `groups` into `runs`, preserving `unjudged` on surrounding
	/// runs and extending `unreported` so debt folding never promotes compacted skips into the
	/// report floor.
	pub fn compact_skip(&mut self, groups: std::ops::Range<u64>) {
		if groups.is_empty() {
			return;
		}
		self.merge_run(groups.clone());
		self.mark_unreported(groups);
	}

	fn mark_unreported(&mut self, span: std::ops::Range<u64>) {
		if span.is_empty() {
			return;
		}
		if self.unreported.is_empty() {
			self.unreported = span;
		} else {
			self.unreported = self.unreported.start.min(span.start)..self.unreported.end.max(span.end);
		}
	}

	/// Forget that any key in `groups` was handed out, splitting the runs it cuts.
	pub fn remove(&mut self, groups: std::ops::Range<u64>) {
		let Some(last) = groups.end.checked_sub(1).filter(|_| !groups.is_empty()) else {
			return;
		};
		self.unjudged.remove(groups.clone());
		// Runs are disjoint and sorted, so the ones reaching into `groups` are the last few
		// starting at or below its last key.
		let cut: Vec<(u64, u64)> = (self.runs.range(..=last).rev())
			.take_while(|(_, end)| **end >= groups.start)
			.map(|(start, end)| (*start, *end))
			.collect();
		for (start, end) in cut {
			self.runs.remove(&start);
			if start < groups.start {
				self.runs.insert(start, groups.start - 1);
			}
			if end > last {
				self.runs.insert(last + 1, end);
			}
		}
		// Shrink both ends of `unreported` past any keys `remove` just vacated in `runs`, so
		// keys later handed out there are not permanently blocked from debt folding.
		if !self.unreported.is_empty() {
			let start = self.next_from(self.unreported.start);
			let end = self
				.runs
				.range(..self.unreported.end)
				.next_back()
				.map(|(_, last)| (*last + 1).min(self.unreported.end));
			match (start, end) {
				(Some(start), Some(end)) if start < end => self.unreported = start..end,
				_ => self.unreported = 0..0,
			}
		}
	}

	/// The parts of `groups` not handed out, in order, found as they are read.
	pub fn gaps(&self, groups: std::ops::Range<u64>) -> impl Iterator<Item = std::ops::Range<u64>> + '_ {
		let mut next = groups.start;
		let from = self
			.runs
			.range(..=groups.start)
			.next_back()
			.map_or(next, |(start, _)| *start);
		let mut runs = self.runs.range(from..);
		std::iter::from_fn(move || {
			while next < groups.end {
				let Some((first, last)) = runs.next() else {
					let gap = next..groups.end;
					next = groups.end;
					return Some(gap);
				};
				let gap = next..(*first).min(groups.end);
				next = next.max(last + 1);
				if !gap.is_empty() {
					return Some(gap);
				}
			}
			None
		})
	}

	/// The parts of `groups` none of `taken` handed out, in order.
	pub fn gaps_in_all(taken: &[&Self], groups: std::ops::Range<u64>) -> Vec<std::ops::Range<u64>> {
		let mut gaps = Vec::from([groups]);
		for taken in taken {
			gaps = gaps.into_iter().flat_map(|gap| taken.gaps(gap)).collect();
		}
		gaps
	}

	/// Count every key below `floor` as handed out, as a fold past the reader's budget does:
	/// each gap it closes that `judged` doesn't hold, or every one while the ranges are still
	/// being placed, a range added later that holds it owes again.
	///
	/// So that ranges moving to disjoint groups leave no more runs than they hold, what lies
	/// between two runs it remembers that `judged` doesn't hold joins them: what ranges added
	/// over it later hand out may then include a group handed out before, which the same
	/// identity makes harmless, as [`Record::retire`](super::record::Record) trades, rather than
	/// miss one never handed out.
	pub fn fold_below(&mut self, floor: u64, judged: Option<&Cover>) {
		let prev = self.floor();
		let gaps: Vec<std::ops::Range<u64>> = self.gaps(prev..floor).collect();
		let added = !gaps.is_empty();
		for gap in gaps {
			match judged {
				Some(judged) => {
					for run in minus(gap.start..=gap.end - 1, judged) {
						self.unjudged.insert(*run.start()..*run.end() + 1);
					}
				}
				None => self.unjudged.insert(gap),
			}
		}
		if prev < floor {
			self.mark_unreported(prev..floor);
		}
		self.forget_below(floor);
		// Placed ranges say what joins; a fold while they're placed is followed by one that is.
		let (Some(judged), true, Some(first), Some(last)) = (
			judged,
			added,
			self.unjudged.iter().next().map(|run| run.start),
			self.unjudged.iter().next_back().map(|run| run.end - 1),
		) else {
			return;
		};
		for run in minus(first..=last, judged) {
			let run = *run.start()..*run.end() + 1;
			self.unjudged.insert(run.clone());
			// Not what a reopen gave back since, which is owed rather than counted handed out.
			let owed: Vec<std::ops::Range<u64>> = self.gaps(run).collect();
			for gap in owed {
				self.unjudged.remove(gap);
			}
		}
	}

	/// Bounding span covering keys closed by budget retirement or skip compaction without
	/// having been reported.
	pub fn unreported(&self) -> std::ops::Range<u64> {
		self.unreported.clone()
	}

	/// One past the contiguous settled run in `delivered` starting at `key`, stopping before
	/// `unreported` so debt folding never promotes unreported keys.
	pub fn settled_run_end(&self, key: u64) -> Option<u64> {
		if self.unreported.contains(&key) {
			return None;
		}
		let end = self.run_end(key)?;
		if !self.unreported.is_empty() && key < self.unreported.start && end > self.unreported.start {
			Some(self.unreported.start)
		} else {
			Some(end)
		}
	}

	/// The lowest key from `key` on that is in `delivered` and outside `unreported`.
	pub fn next_settled_from(&self, key: u64) -> Option<u64> {
		let start = self.next_from(key)?;
		if self.unreported.contains(&start) {
			self.next_from(self.unreported.end)
		} else {
			Some(start)
		}
	}

	/// Whether a fold counted `key` as handed out while no range held it.
	pub fn is_unjudged(&self, key: u64) -> bool {
		self.unjudged.contains(&key)
	}

	/// Take back each key a fold counted as handed out unjudged that `judged` now holds, so it
	/// is owed again.
	pub fn reopen(&mut self, judged: &Cover) {
		let held: Vec<std::ops::Range<u64>> = (self.unjudged.iter())
			.flat_map(|run| judged.within(run.start, run.end - 1))
			.map(|run| *run.start()..*run.end() + 1)
			.collect();
		for run in held {
			self.remove(run);
		}
	}

	/// Count every key below `floor` as handed out for good: none is owed again, whatever
	/// range is added later.
	pub fn settle_below(&mut self, floor: u64) {
		self.forget_below(floor);
		self.unjudged.remove(0..floor);
		if self.unreported.end <= floor {
			self.unreported = 0..0;
		} else if self.unreported.start < floor {
			self.unreported.start = floor;
		}
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

	/// The lowest key from `key` on that was handed out.
	pub fn next_from(&self, key: u64) -> Option<u64> {
		match self.contains(key) {
			true => Some(key),
			false => self.runs.range(key..).next().map(|(first, _)| *first),
		}
	}

	/// One past the highest key handed out.
	pub fn end(&self) -> Option<u64> {
		self.runs.last_key_value().map(|(_, last)| last + 1)
	}

	/// How many runs it holds: one per gap, plus the floor, plus one per run folded unjudged.
	pub fn runs(&self) -> usize {
		self.runs.len() + self.unjudged.len()
	}

	/// Count distinct runs overlapping `groups`, stopping at `limit`.
	pub fn count_runs_in(&self, groups: std::ops::Range<u64>, limit: usize) -> usize {
		if groups.is_empty() || limit == 0 {
			return 0;
		}
		let from = self
			.runs
			.range(..=groups.start)
			.next_back()
			.filter(|(_, last)| **last >= groups.start)
			.map_or(groups.start, |(start, _)| *start);
		self.runs.range(from..groups.end).take(limit).count()
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
		assert_eq!(delivered.gaps(0..12).collect::<Vec<_>>(), [0..2, 3..4, 6..9, 10..12]);
		assert_eq!(delivered.gaps(4..6).next(), None);
		assert_eq!(delivered.gaps(5..11).collect::<Vec<_>>(), [6..9, 10..11]);
		delivered.insert(u64::MAX - 1);
		let gaps: Vec<_> = delivered.gaps(u64::MAX - 2..u64::MAX).collect();
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
	fn remove_splits_the_runs_it_cuts() {
		let mut delivered = Delivered::default();
		delivered.insert_run(2..7);
		for key in [9, 12] {
			delivered.insert(key);
		}
		delivered.remove(4..10);
		assert_eq!(runs(&delivered), [(2, 3), (12, 12)], "the tail of one, all of another");
		delivered.insert_run(2..12);
		delivered.remove(5..6);
		assert_eq!(runs(&delivered), [(2, 4), (6, 12)], "the middle of one");
		delivered.remove(13..20);
		delivered.remove(8..8);
		assert_eq!(
			runs(&delivered),
			[(2, 4), (6, 12)],
			"nothing handed out, or nothing at all"
		);
		delivered.insert(u64::MAX - 1);
		delivered.remove(0..u64::MAX);
		assert_eq!(runs(&delivered), []);
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

	#[test]
	fn count_runs_in_counts_overlapping_runs_up_to_limit() {
		let mut delivered = Delivered::default();
		delivered.insert_run(10..15);
		for key in [20, 22, 24, 26] {
			delivered.insert(key);
		}
		assert_eq!(delivered.count_runs_in(0..10, 4), 0);
		assert_eq!(delivered.count_runs_in(12..14, 4), 1);
		assert_eq!(delivered.count_runs_in(14..21, 4), 2);
		assert_eq!(delivered.count_runs_in(20..30, 2), 2);
		assert_eq!(delivered.count_runs_in(20..30, 10), 4);
		assert_eq!(delivered.count_runs_in(21..22, 4), 0);
		assert_eq!(delivered.count_runs_in(20..20, 4), 0);
		assert_eq!(delivered.count_runs_in(20..30, 0), 0);
	}

	#[test]
	fn a_fold_reopens_only_the_gaps_no_range_judged() {
		let mut delivered = Delivered::default();
		for key in [4, 9] {
			delivered.insert(key);
		}
		// Ranges hold 2..=4 and 6..=7 as the fold closes every gap below 9.
		delivered.fold_below(9, Some(&Cover::new([2..=4, 6..=7])));
		assert_eq!(runs(&delivered), [(0, 9)]);
		assert_eq!(delivered.runs(), 4, "the floor plus three gaps no range held");

		// A range added over 0..=8 owes the gaps folded unjudged, never a hand-out nor what the
		// budget judged.
		delivered.reopen(&Cover::new(std::iter::once(0..=8)));
		assert_eq!(runs(&delivered), [(2, 4), (6, 7), (9, 9)]);
		assert_eq!(delivered.runs(), 3, "nothing left to reopen");

		// Folded while the ranges are still being placed, every gap is unjudged; one handed out or
		// settled since is not reopened.
		delivered.fold_below(10, None);
		delivered.insert_run(0..1);
		delivered.reopen(&Cover::new(std::iter::once(0..=9)));
		assert_eq!(runs(&delivered), [(0, 0), (2, 4), (6, 7), (9, 9)]);

		// Settled for good, nothing below is owed again.
		delivered.fold_below(10, None);
		delivered.settle_below(10);
		delivered.reopen(&Cover::new(std::iter::once(0..=9)));
		assert_eq!(runs(&delivered), [(0, 9)]);
		assert_eq!(delivered.runs(), 1);
	}

	#[test]
	fn a_fold_keeps_no_more_unjudged_runs_than_the_ranges_hold() {
		let mut delivered = Delivered::default();
		// Ranges moving to a fresh group each time, each handed out and folded past by ranges
		// then holding only the live edge, would leave a run no range held between each pair.
		for key in (10..100).step_by(10) {
			delivered.insert(key);
			delivered.fold_below(key + 5, Some(&Cover::new(std::iter::once(key + 5..=1000))));
		}
		assert_eq!(delivered.runs(), 2, "the floor and one run between");
		// A range added over a group handed out before owes it again, and while it holds it,
		// splits the runs around it; one no longer held, as 97 next, joins them.
		delivered.reopen(&Cover::new([40..=40, 95..=1000]));
		assert!(!delivered.contains(40));
		delivered.insert(40);
		delivered.insert(97);
		delivered.fold_below(99, Some(&Cover::new([40..=40, 99..=1000])));
		assert_eq!(delivered.runs(), 3, "the floor and a run each side of 40");
		delivered.reopen(&Cover::new(std::iter::once(0..=98)));
		assert_eq!(runs(&delivered), [(40, 40)]);
	}
}

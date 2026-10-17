//! Group numbering on the catalog clock's one-second grid, for a track whose timestamps every
//! publisher of its epoch shares.
//!
//! Each second of the catalog clock is a cell, numbered by its index. A group opens at the first
//! start in a cell no group has taken yet and takes that cell's number; later starts in the cell
//! join it, and every cell passed with no start gets an empty group, so sequences stay
//! consecutive. What counts as a start is the track's business: a video keyframe, any audio frame,
//! a passthrough group start.
//!
//! Two producers fed one stream therefore publish the same groups wherever each started, provided
//! neither publishes from a cell it may have seen only part of. That is the one it started in, and
//! the one it resumed in after a break: an earlier producer may have opened that cell's group at a
//! start the later one missed.

use std::ops::Range;

/// The spacing of the grid, in microseconds of the catalog clock.
pub(crate) const GRID_MICROS: u64 = 1_000_000;

/// The most empty cells one step of the timeline fills. A longer step between two frames (or two
/// [`advance`](Grid::advance)s) starts a new watch instead, as a break does, so a source that leaps
/// hours ahead cannot open a group for every second it skipped.
const FILL_MAX: u64 = 60;

/// The cell of the grid `timestamp` falls in.
fn cell(timestamp: moq_net::Timestamp) -> u64 {
	u64::try_from(timestamp.as_micros() / u128::from(GRID_MICROS)).unwrap_or(u64::MAX)
}

/// Where a frame lands on the grid.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Place {
	/// Give each sequence in `fill` an empty group, then open the group `sequence`.
	Open { fill: Range<u64>, sequence: u64 },
	/// Join the open group.
	Join,
	/// No group is open, and the frame's cell may hold a start this producer missed.
	Drop,
}

/// Where a track stands on the grid.
#[derive(Debug, Default)]
pub(crate) struct Grid {
	/// The newest cell a frame or [`advance`](Self::advance) reached.
	now: Option<u64>,
	/// The cell the watch began in, the newest one possibly seen only in part: set by the first
	/// frame, advance or [`watch`](Self::watch), and again by the first after a [break](Self::restart).
	origin: Option<u64>,
	/// The highest cell given a group, empty or not.
	last: Option<u64>,
	/// Whether any group has opened.
	opened: bool,
}

impl Grid {
	/// Place a frame at `timestamp`. `start` says the frame may open a group, `open` that one is
	/// open for it to join.
	pub fn place(&mut self, timestamp: moq_net::Timestamp, start: bool, open: bool) -> Place {
		let at = self.reach(timestamp);
		let origin = self.origin.expect("a reached grid has an origin");
		if start && self.last.is_none_or(|last| at > last) && (self.opened || at > origin) {
			let fill = self.empty(at);
			self.last = Some(at);
			self.opened = true;
			return Place::Open { fill, sequence: at };
		}
		match open {
			true => Place::Join,
			false => Place::Drop,
		}
	}

	/// The timeline reached `now`, though no frame of the track did: the empty groups for the cells
	/// it passed whole. The open group stays open, since only a start in a later cell closes it.
	pub fn advance(&mut self, now: moq_net::Timestamp) -> Range<u64> {
		let at = self.reach(now);
		self.empty(at)
	}

	/// The track sees every start only from `since` on, for an importer that can miss starts after
	/// its first frames. Until the first group opens, a later call can only move that point later.
	pub fn watch(&mut self, since: moq_net::Timestamp) {
		if !self.opened {
			let at = cell(since);
			self.origin = Some(self.origin.map_or(at, |origin| origin.max(at)));
		}
	}

	/// A break: the cells it skipped get no empty group, and the watch begins again at the next
	/// frame or advance.
	pub fn restart(&mut self) {
		self.origin = None;
	}

	/// The cell the timeline reached with `timestamp`: its own, or the newest already reached,
	/// since a passed cell never takes a group again.
	fn reach(&mut self, timestamp: moq_net::Timestamp) -> u64 {
		let own = cell(timestamp);
		let at = self.now.map_or(own, |now| now.max(own));
		if self.now.is_some_and(|now| at - now > FILL_MAX) {
			self.origin = None;
		}
		self.now = Some(at);
		self.origin.get_or_insert(at);
		at
	}

	/// Claim every cell before `at` that was watched whole and holds no group yet.
	fn empty(&mut self, at: u64) -> Range<u64> {
		let origin = self.origin.expect("a reached grid has an origin");
		let from = self.last.map_or(origin, |last| last.max(origin)) + 1;
		if from >= at {
			return at..at;
		}
		self.last = Some(at - 1);
		from..at
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(millis: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_millis(millis).unwrap()
	}

	fn opens(sequence: u64, fill: Range<u64>) -> Place {
		Place::Open { fill, sequence }
	}

	#[test]
	fn the_cell_a_producer_starts_in_publishes_nothing() {
		let mut grid = Grid::default();
		assert_eq!(grid.place(at(10_000), true, false), Place::Drop);
		assert_eq!(grid.place(at(10_500), true, false), Place::Drop);
		assert_eq!(grid.place(at(11_200), true, false), opens(11, 11..11));
	}

	#[test]
	fn later_starts_in_a_cell_join_its_group() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		assert_eq!(grid.place(at(11_000), true, false), opens(11, 11..11));
		assert_eq!(grid.place(at(11_500), true, true), Place::Join);
		assert_eq!(grid.place(at(11_999), false, true), Place::Join);
	}

	#[test]
	fn cells_passed_without_a_start_get_empty_groups() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		grid.place(at(11_000), true, false);
		assert_eq!(grid.place(at(12_500), false, true), Place::Join);
		assert_eq!(grid.place(at(14_100), true, true), opens(14, 12..14));
	}

	#[test]
	fn a_frame_behind_the_newest_cell_counts_in_it() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		grid.place(at(12_000), true, false);
		assert_eq!(grid.place(at(11_900), true, true), Place::Join);
	}

	#[test]
	fn a_break_fills_nothing_it_skipped() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		grid.place(at(11_000), true, false);
		grid.restart();
		assert_eq!(grid.place(at(15_000), true, true), opens(15, 15..15));
		assert_eq!(grid.place(at(17_000), true, true), opens(17, 16..17));
	}

	#[test]
	fn a_leap_past_the_fill_bound_acts_as_a_break() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		grid.place(at(11_000), true, false);
		assert_eq!(grid.place(at(3_600_000), true, true), opens(3_600, 3_600..3_600));
	}

	#[test]
	fn advance_fills_cells_as_they_pass() {
		let mut grid = Grid::default();
		assert_eq!(grid.advance(at(10_300)), 10..10);
		assert_eq!(grid.advance(at(11_000)), 11..11);
		assert_eq!(grid.advance(at(13_000)), 11..13);
		assert_eq!(grid.place(at(13_400), true, false), opens(13, 13..13));
	}

	#[test]
	fn watch_moves_the_origin_later_until_a_group_opens() {
		let mut grid = Grid::default();
		grid.place(at(10_000), true, false);
		grid.watch(at(12_000));
		assert_eq!(grid.place(at(12_500), true, false), Place::Drop);
		assert_eq!(grid.place(at(13_000), true, false), opens(13, 13..13));
		grid.watch(at(20_000));
		assert_eq!(grid.place(at(14_000), true, true), opens(14, 14..14));
	}

	#[test]
	fn producers_started_apart_agree_from_the_later_ones_first_group() {
		let starts = [100, 900, 1_400, 2_050, 2_700, 2_800, 5_300, 5_900, 6_000, 6_400, 9_100];
		let run = |from: usize| {
			let mut grid = Grid::default();
			let mut groups = Vec::new();
			for &start in &starts[from..] {
				if let Place::Open { fill, sequence } = grid.place(at(10_000 + start), true, !groups.is_empty()) {
					groups.extend(fill);
					groups.push(sequence);
				}
			}
			groups
		};
		let early = run(0);
		for from in 1..starts.len() {
			let late = run(from);
			assert!(early.ends_with(&late), "from {from}: {early:?} vs {late:?}");
		}
	}
}

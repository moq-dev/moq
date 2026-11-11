//! What one reader did with each group sequence, as runs: handed out, skipped as stale, given
//! up, and what its wire was told.

use rangemap::{RangeMap, RangeSet};

use super::delivered::Delivered;
use super::skipped::Skipped;
use super::track::Hole;

/// One reader's record of every sequence it settled, as runs.
#[derive(Default)]
pub(crate) struct Record {
	/// The groups handed out, so none is handed out twice: not when its sequence is re-created,
	/// nor when another route delivers it too.
	pub(super) delivered: Delivered,
	/// Cached groups skipped as stale, which a wider budget owes again.
	pub(super) skipped: Skipped,
	/// Groups skipped as stale, which account for a bounded window as handed out would, until a
	/// wider budget owes them again. Kept under an open window too, which a reader may narrow to
	/// a bounded one.
	pub(super) written_off: RangeSet<u64>,
	/// Sequences given up and not reported yet, with why: stale skips, cuts, and upstream skips
	/// and drops.
	pub(super) pending: RangeMap<u64, Pending>,
	/// Every sequence already reported, with how.
	pub(super) reported: RangeMap<u64, Report>,
}

/// A run a reader has yet to report, and whether it was copied from the track's runs, which a
/// rescan folds in afresh: one the track forgot, such as a stale run new terms superseded,
/// leaves nothing behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
	pub hole: Hole,
	pub received: bool,
}

impl Pending {
	/// A verdict this reader reached itself.
	pub fn own(hole: Hole) -> Self {
		Self { hole, received: false }
	}
}

/// How a reader reported a run, and what its wire was told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
	/// Served on a Group Stream, or given up on a version whose drops are all final: nothing
	/// goes out over it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Settled,
	/// Dropped for good: no update reopens it, though a skip still goes out over it.
	Dropped,
	/// Dropped as stale (OLD), which an update reopens.
	Stale {
		/// An upstream's stale run decided it, which is the upstream's to judge again then.
		received: bool,
		/// Its Group Stream expired as stale, or it was dropped OLD for good: its publisher
		/// drops it again under each update rather than this reader reopening it.
		expired: bool,
		/// Its group was handed out after the drop, which the group won: no update reopens it.
		taken: bool,
	},
	/// Skipped, which replaces a drop.
	Skip,
}

impl Report {
	/// How `hole` reports, as it came from upstream when `received`.
	pub fn of(hole: &Hole, received: bool) -> Self {
		match hole {
			Hole::Skip => Self::Skip,
			hole if hole.stale_budget().is_some() => Self::Stale {
				received,
				expired: false,
				taken: false,
			},
			_ => Self::Dropped,
		}
	}

	/// Whether this report stops `hole` going out over it: a skip goes out over a drop, and
	/// a final drop over a stale one, since each replaces what it goes out over. An OLD drop
	/// never goes out over a stale one, which already said OLD.
	pub fn blocks(self, hole: &Hole) -> bool {
		match self {
			Self::Settled | Self::Skip => true,
			Self::Dropped | Self::Stale { taken: true, .. } => !matches!(hole, Hole::Skip),
			Self::Stale { .. } => hole.error() == crate::StreamError::Old,
		}
	}

	/// Whether an update reopens it: a stale drop nothing settled since.
	pub fn reopens(self) -> bool {
		matches!(
			self,
			Self::Stale {
				expired: false,
				taken: false,
				..
			}
		)
	}
}

impl Record {
	/// Report the first part of `range` nothing already reported blocks, as `hole` says,
	/// replacing what it goes out over. `None` when every part is blocked.
	pub fn report(&mut self, range: std::ops::Range<u64>, hole: &Hole, received: bool) -> Option<std::ops::Range<u64>> {
		let blocked: RangeSet<u64> = self
			.reported
			.overlapping(&range)
			.filter(|(_, report)| report.blocks(hole))
			.map(|(run, _)| run.clone())
			.collect();
		let gap = blocked.gaps(&range).next()?;
		self.reported.insert(gap.clone(), Report::of(hole, received));
		Some(gap)
	}

	/// The lowest pending run's first part nothing already reported blocks, now reported, with
	/// why; the rest of that run waits its turn.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn next_pending(&mut self) -> Option<(std::ops::Range<u64>, Hole)> {
		while let Some((range, pending)) = self.pending.iter().next().map(|(range, pending)| (range.clone(), pending.clone())) {
			self.pending.remove(range.clone());
			if let Some(gap) = self.report(range.clone(), &pending.hole, pending.received) {
				if gap.end < range.end {
					self.pending.insert(gap.end..range.end, pending.clone());
				}
				return Some((gap, pending.hole));
			}
		}
		None
	}

	/// Mark a stale report on `sequence` settled by its group being handed out.
	pub fn retire(&mut self, sequence: u64) {
		if let Some(end) = sequence.checked_add(1)
			&& let Some(Report::Stale { received, expired, .. }) = self.reported.get(&sequence).copied()
		{
			self.reported.insert(
				sequence..end,
				Report::Stale {
					received,
					expired,
					taken: true,
				},
			);
		}
	}
}

/// What a publisher told the wire about a reader's sequences.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl Record {
	/// Whether the wire was told of `sequence`: served, or given up.
	pub fn told(&self, sequence: u64) -> bool {
		self.reported.contains_key(&sequence)
	}

	/// Whether `sequence` went out as a stale drop, which a group still replaces.
	pub fn told_stale(&self, sequence: u64) -> bool {
		matches!(self.reported.get(&sequence), Some(Report::Stale { .. }))
	}

	/// The parts of `range` the wire was told nothing of.
	pub fn untold(&self, range: std::ops::Range<u64>) -> RangeSet<u64> {
		self.reported.gaps(&range).collect()
	}

	/// Record that the wire was told `report` of `range`. A stale report keeps the parts an
	/// upstream's stale run decided, which stay the upstream's to judge again.
	pub fn tell(&mut self, range: std::ops::Range<u64>, report: Report) {
		if range.is_empty() {
			return;
		}
		let received: Vec<_> = match report {
			Report::Stale { received: false, .. } => self
				.reported
				.overlapping(&range)
				.filter(|(_, report)| matches!(report, Report::Stale { received: true, .. }))
				.map(|(run, _)| run.start.max(range.start)..run.end.min(range.end))
				.collect(),
			_ => Vec::new(),
		};
		self.reported.insert(range, report);
		if let Report::Stale { expired, taken, .. } = report {
			for run in received {
				self.reported.insert(
					run,
					Report::Stale {
						received: true,
						expired,
						taken,
					},
				);
			}
		}
	}

	/// Forget what the wire was told of `range`, which is owed again.
	pub fn untell(&mut self, range: std::ops::Range<u64>) {
		if !range.is_empty() {
			self.reported.remove(range);
		}
	}

	/// Forget the drops of `range` the wire was told, which a verdict or group replaces.
	pub fn replace_drops(&mut self, range: std::ops::Range<u64>) {
		let drops: Vec<_> = self
			.reported
			.overlapping(&range)
			.filter(|(_, report)| matches!(report, Report::Dropped | Report::Stale { .. }))
			.map(|(run, _)| run.start.max(range.start)..run.end.min(range.end))
			.collect();
		for run in drops {
			self.reported.remove(run);
		}
	}

	/// The runs whose Group Stream expired as stale, or that went out OLD for good, which
	/// each update drops again.
	pub fn expired(&self) -> RangeSet<u64> {
		self.reported
			.iter()
			.filter(|(_, report)| matches!(report, Report::Stale { expired: true, .. }))
			.map(|(run, _)| run.clone())
			.collect()
	}
}

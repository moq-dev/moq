//! What one reader did with each group sequence, as runs: handed out, skipped as stale, given
//! up, and what its wire was told.

use rangemap::{RangeMap, RangeSet};

use super::delivered::Delivered;
use super::skipped::Skipped;
use super::track::Hole;

/// One reader's record of every sequence it settled, as runs.
#[derive(Default)]
pub(super) struct Record {
	/// The groups handed out, so none is handed out twice: not when its sequence is re-created,
	/// nor when another route delivers it too.
	pub delivered: Delivered,
	/// Cached groups skipped as stale, which a wider budget owes again.
	pub skipped: Skipped,
	/// Groups skipped as stale, which account for a bounded window as handed out would, until a
	/// wider budget owes them again. Kept under an open window too, which a reader may narrow to
	/// a bounded one.
	pub written_off: RangeSet<u64>,
	/// Sequences given up and not reported yet, with why: stale skips, cuts, and upstream skips
	/// and drops.
	pub pending: RangeMap<u64, Pending>,
	/// Every sequence already reported, with how.
	pub reported: RangeMap<u64, Report>,
}

/// A run a reader has yet to report, and whether it was copied from the track's runs, which a
/// rescan folds in afresh: one the track forgot, such as a stale run new terms superseded,
/// leaves nothing behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pending {
	pub hole: Hole,
	pub received: bool,
}

impl Pending {
	/// A verdict this reader reached itself.
	pub fn own(hole: Hole) -> Self {
		Self { hole, received: false }
	}
}

/// How a reader reported a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Report {
	/// Dropped for good, or stale until its group was handed out: no update reopens it.
	Dropped,
	/// Dropped as stale, which an update reopens. `received` when an upstream's stale run
	/// decided it, which is the upstream's to judge again then.
	Stale { received: bool },
	/// Skipped, which replaces a drop.
	Skip,
}

impl Report {
	/// How `hole` reports, as it came from upstream when `received`.
	pub fn of(hole: &Hole, received: bool) -> Self {
		match hole {
			Hole::Skip => Self::Skip,
			hole if hole.stale_budget().is_some() => Self::Stale { received },
			_ => Self::Dropped,
		}
	}

	/// Whether this report stops `hole` going out over it: a skip goes out over a drop, and
	/// a final drop over a stale one, since each replaces what it goes out over.
	pub fn blocks(self, hole: &Hole) -> bool {
		match hole {
			Hole::Skip => self == Self::Skip,
			Hole::Drop { stale: None, .. } => !matches!(self, Self::Stale { .. }),
			Hole::Drop { .. } => true,
		}
	}
}

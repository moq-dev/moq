//! What one reader did with each group key, as runs: handed out, skipped as stale, given
//! up, and what its wire was told.

use std::ops::Range;
use std::sync::Arc;

use rangemap::{RangeMap, RangeSet};

use super::cache;
use super::delivered::Delivered;
use super::skipped::Skipped;
use super::track::Hole;
use crate::Error;

/// Heap a record keeps per run: its widest entry, doubled for half-full `BTreeMap` nodes as
/// the cache's own overheads are. Billed to the pool, so a peer fragmenting what a reader
/// settles costs what the cache would hold, never more.
pub(super) const RUN_OVERHEAD: u64 = 2 * size_of::<(Range<u64>, Pending)>() as u64;

/// One reader's record of every key it settled, as runs.
#[derive(Default)]
pub(crate) struct Record {
	/// The groups handed out, so none is handed out twice: not when its key is re-created,
	/// nor when another route delivers it too.
	pub(super) delivered: Delivered,
	/// Cached groups skipped as stale, which a wider budget owes again.
	pub(super) skipped: Skipped,
	/// Groups skipped as stale, which account for a bounded window as handed out would, until a
	/// wider budget owes them again. Kept under an open window too, which a reader may narrow to
	/// a bounded one.
	pub(super) written_off: RangeSet<u64>,
	/// Keys given up and not reported yet, with why: stale skips, cuts, and upstream skips
	/// and drops.
	pub(super) pending: RangeMap<u64, Pending>,
	/// Every key already reported, with how.
	pub(super) reported: RangeMap<u64, Report>,
	/// Everything below was settled for good and folded away under pool pressure: handed
	/// out, or reported final, so none is handed out or reported again.
	floor: u64,
	/// What the runs above the floor bill to the track's account, opened on the first run.
	bill: Option<cache::Reader>,
	/// The pool needed a run this reader was promised, so it ended.
	failed: bool,
	/// A fold forgot the runs copied from the track's drop set, which the next drop scan
	/// folds in again in full.
	pub(super) rescan: bool,
}

/// A run a reader has yet to report, and whether it was copied from the track's runs, which a
/// rescan folds in afresh: one the track forgot, such as a stale run a new upstream
/// subscription superseded, leaves nothing behind.
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

/// How a reader reported a run, and what its wire was told. Every report is final: a drop,
/// stale (OLD) or not, settles its keys for good on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
	/// Served on a Group Stream, or given up on a version that has no skip: nothing goes out
	/// over it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Settled,
	/// Dropped, though a skip still goes out over it.
	Dropped,
	/// Skipped, which replaces a drop.
	Skip,
}

impl Report {
	/// How `hole` reports.
	pub fn of(hole: &Hole) -> Self {
		match hole {
			Hole::Skip => Self::Skip,
			Hole::Drop { .. } => Self::Dropped,
		}
	}

	/// Whether this report stops `hole` going out over it: only a skip goes out over a drop.
	pub fn blocks(self, hole: &Hole) -> bool {
		match self {
			Self::Settled | Self::Skip => true,
			Self::Dropped => !matches!(hole, Hole::Skip),
		}
	}
}

impl Record {
	/// Report the first part of `range` nothing already reported blocks, as `hole` says,
	/// replacing what it goes out over. `None` when every part is blocked.
	pub fn report(&mut self, range: Range<u64>, hole: &Hole) -> Option<Range<u64>> {
		let range = range.start.max(self.floor)..range.end;
		if range.is_empty() {
			return None;
		}
		let blocked: RangeSet<u64> = self
			.reported
			.overlapping(&range)
			.filter(|(_, report)| report.blocks(hole))
			.map(|(run, _)| run.clone())
			.collect();
		let gap = blocked.gaps(&range).next()?;
		self.reported.insert(gap.clone(), Report::of(hole));
		Some(gap)
	}

	/// The lowest pending run's first part nothing already reported blocks, now reported, with
	/// why; the rest of that run waits its turn.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn next_pending(&mut self) -> Option<(Range<u64>, Hole)> {
		while let Some((range, pending)) = self
			.pending
			.iter()
			.next()
			.map(|(range, pending)| (range.clone(), pending.clone()))
		{
			self.pending.remove(range.clone());
			if let Some(gap) = self.report(range.clone(), &pending.hole) {
				if gap.end < range.end {
					self.pending.insert(gap.end..range.end, pending.clone());
				}
				return Some((gap, pending.hole));
			}
		}
		None
	}
}

/// What a publisher told the wire about a reader's keys.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl Record {
	/// Whether the wire was told of `key`: served, or given up.
	pub fn told(&self, key: u64) -> bool {
		key < self.floor || self.reported.contains_key(&key)
	}

	/// The parts of `range` the wire was told nothing of.
	pub fn untold(&self, range: Range<u64>) -> RangeSet<u64> {
		let range = range.start.max(self.floor)..range.end;
		match range.is_empty() {
			true => RangeSet::new(),
			false => self.reported.gaps(&range).collect(),
		}
	}

	/// Record that the wire was told `report` of `range`.
	pub fn tell(&mut self, range: Range<u64>, report: Report) {
		if !range.is_empty() {
			self.reported.insert(range, report);
		}
	}

	/// Forget every report of `range`, which the wire was never told: a run given up below
	/// where delivery starts, which a later widening may still serve.
	pub fn untell(&mut self, range: Range<u64>) {
		if !range.is_empty() {
			self.reported.remove(range);
		}
	}
}

/// Where a reader stands, which shapes what its record's fold settles.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Standing {
	/// The lowest key the reader's ranges name now.
	pub from: u64,
	/// The group this read hands out, which its publisher has yet to tell its wire of.
	pub handed: Option<u64>,
}

/// Runs a reader keeps beside its record, billed with it and settled by its fold: the
/// misses it gave up, which it never asks a fill for again.
pub(super) trait Beside {
	/// How many runs it holds.
	fn runs(&self) -> u64;
	/// Forget every run within `folded`, which the record folded as settled.
	fn forget(&mut self, folded: std::ops::Range<u64>);
}

impl Beside for () {
	fn runs(&self) -> u64 {
		0
	}

	fn forget(&mut self, _folded: std::ops::Range<u64>) {}
}

/// What a record bills, and the debt its track left to its readers.
impl Record {
	/// How many runs it holds above its floor, each billed [`RUN_OVERHEAD`]: the handed-out
	/// prefix is one run that bills nothing.
	fn runs(&self) -> u64 {
		(self.delivered.runs().saturating_sub(1)
			+ self.skipped.runs()
			+ self.written_off.len()
			+ self.pending.len()
			+ self.reported.len()) as u64
	}

	/// Whether the pool needed a run this reader was promised, which ended it.
	pub(super) fn failed(&self) -> bool {
		self.failed
	}

	/// Bill this record to `account`, opened on its first run, and pay what it can of the
	/// debt the track left its readers by folding what it settled for good into its floor.
	/// Ends the reader with [`Error::Lagged`] when a debt is left that only a run it was
	/// promised could pay, and its record is one to end (see [`cache::Reader::ends`]).
	///
	/// Call with no track lock held: a growing bill settles the track's debt.
	pub(super) fn settle(
		&mut self,
		account: impl FnOnce() -> Arc<cache::Track>,
		beside: &mut dyn Beside,
		reader: Standing,
	) -> Result<(), Error> {
		if self.failed {
			return Err(Error::Lagged);
		}
		let bytes = (self.runs() + beside.runs()) * RUN_OVERHEAD;
		let bill = match &mut self.bill {
			Some(bill) => bill,
			None if bytes == 0 => return Ok(()),
			None => self.bill.insert(account().reader()),
		};
		bill.set(bytes);
		if bill.debt() == 0 {
			return Ok(());
		}
		for folded in self.fold(reader) {
			beside.forget(folded);
		}
		let bytes = (self.runs() + beside.runs()) * RUN_OVERHEAD;
		let bill = self.bill.as_mut().expect("opened above");
		bill.set(bytes);
		if bill.debt() > 0 && bill.ends() {
			self.fail();
			return Err(Error::Lagged);
		}
		Ok(())
	}

	/// Raise the floor over the final prefix: each run from it up to the lowest one that is
	/// not final. Final are the groups handed out, every report, and this reader's own stale
	/// skips, which a wider budget would offer again only while the pool still held their
	/// groups. Not final are the runs given up and not reported yet, and the keys nothing
	/// settled.
	///
	/// A reader that started above its floor, as at a sparse-key track's live edge or its
	/// oldest group, never settles the keys below its start, so the final stretch from its
	/// start folds too, into one handed-out and reported run: from where its ranges start, or
	/// the lowest key it settled when they name none or less. The keys below stay owed, should
	/// a group arrive there or its ranges widen. Returns the runs it folded, below the floor
	/// and the stretch.
	fn fold(&mut self, reader: Standing) -> Vec<std::ops::Range<u64>> {
		let mut folded = Vec::new();
		// The runs copied from the track's drop set go first: the next scan folds in again
		// whatever the track still holds.
		let received: Vec<_> = self
			.pending
			.iter()
			.filter(|(_, pending)| pending.received)
			.map(|(run, _)| run.clone())
			.collect();
		if !received.is_empty() {
			for run in received {
				self.pending.remove(run);
			}
			self.rescan = true;
		}
		let (floor, _) = self.final_end(self.floor, reader.handed);
		if floor > self.floor {
			self.floor = floor;
			self.delivered.forget_below(floor);
			self.skipped.forget(0..floor);
			self.written_off.remove(0..floor);
			self.pending.remove(0..floor);
			self.reported.remove(0..floor);
			folded.push(0..floor);
		}
		let from = reader.from.max(self.lowest_settled().unwrap_or(0));
		if from > self.floor {
			// Folded, the stretch costs two runs: only one spanning more is worth it.
			let (end, runs) = self.final_end(from, reader.handed);
			if runs > 2 {
				let stretch = from..end;
				self.delivered.insert_run(stretch.clone());
				self.skipped.forget(stretch.clone());
				self.written_off.remove(stretch.clone());
				self.reported.insert(stretch.clone(), Report::Settled);
				folded.push(stretch);
			}
		}
		folded
	}

	/// The lowest key above the floor that anything settled.
	fn lowest_settled(&self) -> Option<u64> {
		[
			self.delivered.above_gap(),
			self.skipped.lowest(),
			self.written_off.iter().next().map(|run| run.start),
			self.reported.iter().next().map(|(run, _)| run.start),
		]
		.into_iter()
		.flatten()
		.filter(|key| *key > self.floor)
		.min()
	}

	/// One past the final stretch starting at `start`, and how many runs it spans: `start`
	/// itself when its key is not final, or is the group this read hands out.
	fn final_end(&self, start: u64, handed: Option<u64>) -> (u64, usize) {
		let mut end = start;
		let mut runs = 0;
		loop {
			let next = match self.reported.get_key_value(&end) {
				Some((run, _)) => run.end,
				None if self.pending.contains_key(&end) => break,
				None => {
					let local = [
						self.delivered.run_end(end),
						self.written_off.get(&end).map(|run| run.end),
						self.skipped.run_end(end),
					];
					let Some(run_end) = local.into_iter().flatten().max() else {
						break;
					};
					// A report or a run given up inside it is judged on its own.
					let span = end..run_end;
					[
						self.reported.overlapping(&span).next().map(|(run, _)| run.start),
						self.pending.overlapping(&span).next().map(|(run, _)| run.start),
					]
					.into_iter()
					.flatten()
					.fold(run_end, |run_end, start| run_end.min(start.max(end)))
				}
			};
			// The group this read hands out stays above: its publisher has yet to tell its wire
			// of it, and would take one below the floor as told already.
			let next = match handed {
				Some(handed) if end <= handed && handed < next => handed,
				_ => next,
			};
			if next <= end {
				break;
			}
			end = next;
			runs += 1;
		}
		(end, runs)
	}

	/// End this reader: forget every run, releasing its bill against the debt.
	fn fail(&mut self) {
		let bill = self.bill.take();
		*self = Self {
			failed: true,
			..Self::default()
		};
		drop(bill);
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::StreamError;

	/// An account on a pool that any bill puts over its capacity, so a debt it is owed stands.
	fn account() -> (cache::Pool, Arc<cache::Track>) {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		(pool, track)
	}

	/// Leave the track's readers `debt` to pay.
	fn owe(track: &cache::Track, mut debt: u64) {
		track.owe_readers(&mut debt);
	}

	/// Each run past the handed-out prefix bills a fixed cost, released when the record goes.
	#[test]
	fn a_record_bills_each_run() {
		let (pool, track) = account();
		let mut record = Record::default();
		for key in 0..4 {
			record.delivered.insert(key);
		}
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(pool.used(), 0, "a lone prefix bills nothing");
		for key in (10..1_000).step_by(2) {
			record.reported.insert(key..key + 1, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(pool.used(), 495 * RUN_OVERHEAD);
		drop(record);
		assert_eq!(pool.used(), 0);
	}

	/// Under a debt its track could not pay, a record folds what is final into its floor:
	/// handed-out groups and reports, however many keys a run spans.
	/// Folded keys count as handed out and reported.
	#[test]
	fn a_debt_folds_the_final_prefix() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(0);
		record.reported.insert(1..u64::MAX / 2, Report::Dropped);
		record.delivered.insert(u64::MAX / 2);
		record.reported.insert(u64::MAX / 2 + 1..u64::MAX / 2 + 2, Report::Skip);
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(pool.used(), 3 * RUN_OVERHEAD);
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(pool.used(), 0, "all of it folded");
		assert!(
			record.told(7) && record.delivered.contains(7),
			"a folded drop stays dropped"
		);
		assert_eq!(record.report(0..u64::MAX / 2 + 2, &Hole::Skip), None);
		let mut untold = RangeSet::new();
		untold.insert(u64::MAX / 2 + 2..u64::MAX / 2 + 3);
		assert_eq!(record.untold(0..u64::MAX / 2 + 3), untold);
	}

	/// A reader that joined above key 0, as at a sparse-key track's live edge or its oldest
	/// group, folds the final stretch from the lowest key it settled, though nothing settled
	/// the keys below it, which stay owed should a group arrive there.
	#[test]
	fn a_debt_folds_from_the_lowest_key_settled() {
		let (pool, track) = account();
		let mut record = Record::default();
		for key in [1_000, 2_000, 3_000] {
			record.delivered.insert(key);
			record.reported.insert(key + 1..key + 1_000, Report::Dropped);
		}
		record
			.pending
			.insert(4_000..4_001, Pending::own(Hole::drop(StreamError::NotFound)));
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(
			pool.used(),
			2 * RUN_OVERHEAD,
			"the stretch, and the run still to report"
		);
		assert!(record.delivered.contains(2_500) && record.told(2_500));
		assert!(
			!record.delivered.contains(999) && !record.told(999),
			"below it stays owed"
		);
		assert!(!record.told(4_000), "the run still to report stays");
	}

	/// A reader whose ranges start above a key nothing settled, which it no longer asks for,
	/// folds the final stretch from where they start.
	#[test]
	fn a_debt_folds_from_where_the_reader_starts() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(1_000);
		for key in [2_000, 3_000] {
			record.delivered.insert(key);
			record.reported.insert(key + 1..key + 1_000, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(
			pool.used(),
			4 * RUN_OVERHEAD,
			"the lowest key settled starts no stretch"
		);

		let started = Standing {
			from: 2_000,
			handed: None,
		};
		// What the reader gave up inside the stretch is settled with it, so it bills nothing.
		let mut forgotten = Forgotten(Vec::new());
		record.settle(|| track.clone(), &mut forgotten, started).unwrap();
		assert_eq!(pool.used(), 2 * RUN_OVERHEAD, "the stretch, and the key below it");
		assert!(record.delivered.contains(3_500) && record.told(3_500));
		assert!(!record.told(1_500), "below its start stays owed");
		assert_eq!(
			forgotten.0,
			std::slice::from_ref(&(2_000..4_000)),
			"the stretch, beside the record too"
		);
	}

	/// What a reader holds beside its record, here only what it was told to forget.
	struct Forgotten(Vec<std::ops::Range<u64>>);

	impl Beside for Forgotten {
		fn runs(&self) -> u64 {
			0
		}

		fn forget(&mut self, folded: std::ops::Range<u64>) {
			self.0.push(folded);
		}
	}

	/// The group a read hands out stays above the fold: its publisher has yet to tell its wire
	/// of it, and takes a key below the floor as told already.
	#[test]
	fn a_debt_folds_short_of_the_handed_group() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(0);
		record.reported.insert(1..3, Report::Dropped);
		record.delivered.insert(3);
		record.reported.insert(5..6, Report::Skip);
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, pool.used());
		record
			.settle(
				|| track.clone(),
				&mut (),
				Standing {
					from: 0,
					handed: Some(3),
				},
			)
			.unwrap();
		assert!(record.told(2), "the drops below it fold");
		assert!(!record.told(3), "the handed group stays to be told");
	}

	/// A debt that needs a run this reader was promised ends it with `TooFarBehind`: one given
	/// up and not reported yet, or a run above one. What is final below it still folds.
	#[test]
	fn a_debt_on_a_promise_ends_the_reader() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(0);
		record.reported.insert(1..2, Report::Dropped);
		record
			.pending
			.insert(2..3, Pending::own(Hole::drop(StreamError::NotFound)));
		// Enough runs above it that ending the reader frees more than evicting a group.
		for key in (5..1_000).step_by(2) {
			record.reported.insert(key..key + 1, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, pool.used());
		let err = record
			.settle(|| track.clone(), &mut (), Standing::default())
			.unwrap_err();
		assert_eq!(StreamError::from(&err), StreamError::TooFarBehind);
		assert_eq!(pool.used(), 0, "its record went with it");
		assert!(record.failed());
		assert!(
			record.settle(|| track.clone(), &mut (), Standing::default()).is_err(),
			"it stays ended"
		);
	}

	/// A run given up and not reported yet, or a key nothing settled, is no fold's to
	/// forget either; a debt the fold pays leaves the reader running.
	#[test]
	fn a_debt_the_fold_pays_leaves_the_reader() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(0);
		record.reported.insert(1..2, Report::Dropped);
		record
			.pending
			.insert(2..3, Pending::own(Hole::drop(StreamError::NotFound)));
		record.skipped.insert(4, Duration::from_secs(1));
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, RUN_OVERHEAD);
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert_eq!(pool.used(), 2 * RUN_OVERHEAD, "the drop folded");
		assert!(!record.told(2), "the run given up is still owed");
	}

	/// A pool held past its capacity by what it pins ends no reader, however much its records
	/// hold, since what ending one frees is room nothing may use, though it still folds what
	/// is final. Room beside the pins ends it.
	#[test]
	fn a_debt_past_the_pins_ends_no_reader() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(0));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let mut edge = track.charge();
		edge.add(1 << 20);
		edge.promote();
		edge.finish(true);
		let mut record = record_with_promise();
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert!(record.told(1), "what is final still folds");
		assert!(!record.failed());

		// Nor does a record holding most of the pool.
		edge.sub(1 << 20);
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert!(!record.failed());

		// Given room beside the pins, the same debt ends it.
		pool.resize(pool.pinned() + 1);
		owe(&track, pool.used());
		assert!(record.settle(|| track.clone(), &mut (), Standing::default()).is_err());
	}

	/// A record holding a promise it cannot fold, and enough reports above it that ending it
	/// frees more than evicting a group would.
	fn record_with_promise() -> Record {
		let mut record = Record::default();
		record.delivered.insert(0);
		record.reported.insert(1..2, Report::Dropped);
		record
			.pending
			.insert(2..3, Pending::own(Hole::drop(StreamError::NotFound)));
		for key in (5..1_000).step_by(2) {
			record.reported.insert(key..key + 1, Report::Dropped);
		}
		record
	}

	/// Of a track's readers, one holding little never ends for one holding much, nor one
	/// holding less than a cached group costs.
	#[test]
	fn a_debt_ends_only_a_reader_holding_much() {
		let (pool, track) = account();
		let promise = Pending::own(Hole::drop(StreamError::NotFound));
		let mut small = Record::default();
		small.pending.insert(1..2, promise.clone());
		let mut large = Record::default();
		for key in (1..1_000).step_by(2) {
			large.pending.insert(key..key + 1, promise.clone());
		}
		small.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		large.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		owe(&track, RUN_OVERHEAD);
		small.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
		assert!(large.settle(|| track.clone(), &mut (), Standing::default()).is_err());
		assert_eq!(pool.used(), RUN_OVERHEAD);
		small.settle(|| track.clone(), &mut (), Standing::default()).unwrap();
	}

	/// A tail's bill pays the readers' debt but is no record to end: it never joins the mean
	/// that picks which record ends, whether it is small or large.
	#[test]
	fn a_tail_bill_never_skews_which_record_ends() {
		let (_pool, track) = account();
		let mut records = [track.reader(), track.reader()];
		records[0].set(1_536);
		records[1].set(2_880);
		let mut tail = track.tail();
		tail.set(RUN_OVERHEAD);
		assert!(
			!records[0].ends(),
			"the mean is the records' 2208 B, not the tail's share"
		);
		assert!(records[1].ends());
		assert!(!tail.ends(), "a tail never ends");
		tail.set(100_000);
		assert!(
			records[1].ends(),
			"a large tail does not lift the mean over every record"
		);
		drop(tail);
		assert!(records[1].ends());
		assert!(!records[0].ends());
	}

	/// Whatever a bill frees pays its readers' debt, however it shrank, and the debt is forgiven
	/// once the pool is back within its capacity: nothing needs the room any more.
	#[test]
	fn a_debt_is_paid_by_any_shrink_and_forgiven_without_pressure() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(2 * RUN_OVERHEAD));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let mut bill = track.reader();
		bill.set(4 * RUN_OVERHEAD);
		owe(&track, 3 * RUN_OVERHEAD);
		bill.set(3 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 2 * RUN_OVERHEAD, "the shrink paid what it freed");
		bill.set(2 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 0, "within its capacity, the pool forgives the rest");
		bill.set(5 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 0, "a later pressure owes nothing of it");
	}
}

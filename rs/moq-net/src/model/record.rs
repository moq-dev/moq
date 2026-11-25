//! What one reader did with each group key, as runs: handed out, skipped as stale, given
//! up, and what its wire was told.

use std::ops::Range;
use std::sync::Arc;

use rangemap::{RangeMap, RangeSet};

use super::cache;
use super::delivered::Delivered;
use super::skipped::Skipped;
use super::subscription::Cover;
use super::track::Hole;

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
	/// The ranges the last retirement judged the gaps against (see [`Self::retire`]).
	retired: Option<Cover>,
	/// The runs, its own and beside it, left by the last fold: the next folds what is final
	/// once they double (see [`Self::settle`]).
	folded_at: u64,
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
#[derive(Clone, Default)]
pub(super) struct Standing {
	/// The lowest key the reader's ranges name now.
	pub from: u64,
	/// The group this read hands out, which its publisher has yet to tell its wire of.
	pub handed: Option<u64>,
	/// The groups the reader's ranges hold, once nothing in them is left to place: `None`
	/// while a live join waits or a start is still being looked up, when no gap can be known
	/// to lie outside them.
	pub ranges: Option<Cover>,
}

/// Runs a reader keeps beside its record, billed with it and settled by its fold: the
/// misses it gave up, which it never asks a fill for again.
pub(super) trait Beside {
	/// How many runs it holds.
	fn runs(&self) -> u64;
	/// One past the run holding `key` that this reader gave up for good, if it did.
	fn given_up_end(&self, key: u64) -> Option<u64>;
	/// The lowest key from `key` on that it gave up, or still asks or awaits a fill for.
	fn next_held(&self, key: u64) -> Option<u64>;
	/// The lowest key from `key` on that it gave up as stale, which a wider budget asks again.
	fn next_stale(&self, _key: u64) -> Option<u64> {
		None
	}
	/// Forget every run within `folded`, which the record folded as settled.
	fn forget(&mut self, folded: std::ops::Range<u64>);
}

impl Beside for () {
	fn runs(&self) -> u64 {
		0
	}

	fn given_up_end(&self, _key: u64) -> Option<u64> {
		None
	}

	fn next_held(&self, _key: u64) -> Option<u64> {
		None
	}

	fn forget(&mut self, _folded: std::ops::Range<u64>) {}
}

/// A final stretch of keys a fold may settle.
struct Stretch {
	/// One past its last key.
	end: u64,
	/// How many runs it spans.
	runs: usize,
	/// Where each gap it passes through that the reader's ranges left behind starts.
	gaps: Vec<u64>,
}

/// What a walk through the gaps the reader's ranges left behind found, and folded.
#[derive(Default)]
struct Walk {
	gaps: Vec<u64>,
	folded: Vec<std::ops::Range<u64>>,
}

/// Which keys a fold may settle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fold {
	/// Under a debt: every final key, a stale skip and what `beside` gave up included, which
	/// costs a wider budget the offer of them again.
	Debt,
	/// After the ranges moved: only groups handed out, reports, and the gaps left outside the
	/// ranges that start below `below`, so a stale skip stays for a wider budget to offer
	/// again.
	Retire { below: u64 },
	/// As the record grows: only what nothing can change any more, which folds at no cost to
	/// what the reader offers or reports later. Skips and settled reports, as a group handed
	/// out and told; never a drop a skip may still replace, a stale skip a wider budget owes
	/// again, or a key not reported yet, even one handed out.
	Final,
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

	/// Whether the track left this reader a debt to pay: what its record holds back, such as
	/// the misses its allowance left unasked, is then its own to settle.
	pub(super) fn owes(&self) -> bool {
		self.bill.as_ref().is_some_and(|bill| bill.debt() > 0)
	}

	/// Bill this record to `account`, opened on its first run. Once the reader's ranges
	/// moved, retire the gaps they left behind (see [`Self::retire`]); under a debt the track
	/// left its readers, pay what it can by folding what it settled for good into its floor.
	/// Without one, fold what is final whenever the record doubled since it last did, so a
	/// live reader of a sparse-key track, which settles a skip between every two groups, keeps
	/// a record no larger than what is still open, at a cost linear in the runs it adds.
	///
	/// A record is its reader's owed gaps, not cached media, so it never ends its reader:
	/// what is left after both is owed for a moment, a drop not reported yet or a miss still
	/// asked, or lies in the ranges the reader asks for now.
	///
	/// Call with no track lock held: a growing bill settles the track's debt.
	pub(super) fn settle(
		&mut self,
		account: impl FnOnce() -> Arc<cache::Track>,
		beside: &mut dyn Beside,
		reader: Standing,
	) {
		let moved = reader.ranges.is_some() && reader.ranges != self.retired;
		if moved {
			self.retired = reader.ranges.clone();
		}
		let runs = self.runs() + beside.runs();
		let bytes = runs * RUN_OVERHEAD;
		let bill = match &mut self.bill {
			Some(bill) => bill,
			None if bytes == 0 => return,
			None => self.bill.insert(account().reader()),
		};
		bill.set(bytes);
		let owed = bill.debt() > 0;
		let grown = runs > 2 * self.folded_at;
		if !owed && !moved && !grown {
			return;
		}
		let mut folded = Vec::new();
		if moved {
			folded.extend(self.retire(&reader, &*beside));
		}
		match owed {
			true => folded.extend(self.fold(&reader, &*beside, Fold::Debt)),
			false if grown => folded.extend(self.fold(&reader, &*beside, Fold::Final)),
			false => {}
		}
		for folded in folded {
			beside.forget(folded);
		}
		let runs = self.runs() + beside.runs();
		if owed || grown {
			self.folded_at = runs;
		}
		self.bill.as_mut().expect("opened above").set(runs * RUN_OVERHEAD);
	}

	/// Retire the gaps the reader's ranges left behind, which nothing settles while they stay
	/// where they are: keys nothing settled, between two that something did, that the ranges
	/// no longer hold, and that nothing owes for a moment (a run given up and not reported
	/// yet, or a miss `beside` still asks). A record keeps as many such gaps as its ranges
	/// hold runs, so a narrowing its reader undoes still offers what it narrowed past, as the
	/// ranges themselves bound; past that, the lowest retire. Each final stretch through them
	/// folds into one handed-out and reported run, as [`Self::fold`]'s does, so a reader whose
	/// ranges keep moving to disjoint groups keeps a record no larger than its ranges' share.
	/// Should the ranges widen over a retired key again, a group there counts as handed out.
	///
	/// Only a stretch through a retired gap folds, and stale skips stop one: without a debt,
	/// the record otherwise stays as it was, and a wider budget still offers a stale skip
	/// again. Returns the runs it folded.
	fn retire(&mut self, reader: &Standing, beside: &dyn Beside) -> Vec<std::ops::Range<u64>> {
		let Some(ranges) = &reader.ranges else {
			return Vec::new();
		};
		let gaps = self.walk(reader, beside, u64::MAX, false).gaps;
		let excess = gaps.len().saturating_sub(ranges.runs(0).count());
		match excess {
			0 => Vec::new(),
			excess => {
				let below = gaps.get(excess).copied().unwrap_or(u64::MAX);
				self.walk(reader, beside, below, true).folded
			}
		}
	}

	/// Walk each final stretch through the gaps the ranges left behind below `below`, from the
	/// floor up, folding those worth it when `fold` says so.
	fn walk(&mut self, reader: &Standing, beside: &dyn Beside, below: u64, fold: bool) -> Walk {
		let mut walk = Walk::default();
		let retire = Fold::Retire { below };
		let floor = self.final_end(self.floor, reader, beside, retire);
		walk.gaps.extend(&floor.gaps);
		if fold && !floor.gaps.is_empty() {
			self.raise(floor.end);
			walk.folded.push(0..floor.end);
		}
		let mut from = floor.end.max(self.floor);
		while let Some(start) = self.next_told(from) {
			let stretch = self.final_end(start, reader, beside, retire);
			walk.gaps.extend(&stretch.gaps);
			if fold && !stretch.gaps.is_empty() && stretch.runs > 2 {
				self.settle_stretch(start..stretch.end);
				walk.folded.push(start..stretch.end);
			}
			from = stretch.end.max(start + 1);
		}
		walk
	}

	/// Raise the floor over the final prefix: each run from it up to the lowest one that is
	/// not final. Final are the groups handed out, every report, and this reader's own
	/// verdicts: its stale skips and what `beside` gave up, which a wider budget would offer
	/// again only while they were held. Not final are the runs given up and not reported yet,
	/// and the keys nothing settled.
	///
	/// A reader that started above its floor, as at a sparse-key track's live edge or its
	/// oldest group, never settles the keys below its start, so each final stretch from its
	/// start on folds too, into one handed-out and reported run: from where its ranges start,
	/// or the lowest key it settled when they name none or less. The keys below stay owed,
	/// should a group arrive there or its ranges widen. Folding past each key still owed, as a
	/// miss its fills have yet to answer, bounds what is left by those keys, which the reader's
	/// ask allowance bounds in turn. Returns the runs it folded, below the floor and above.
	///
	/// [`Fold::Final`] folds the same way through only what nothing can change any more.
	fn fold(&mut self, reader: &Standing, beside: &dyn Beside, mode: Fold) -> Vec<std::ops::Range<u64>> {
		let mut folded = Vec::new();
		// The runs copied from the track's drop set go first: the next scan folds in again
		// whatever the track still holds.
		let received: Vec<_> = self
			.pending
			.iter()
			.filter(|(_, pending)| mode == Fold::Debt && pending.received)
			.map(|(run, _)| run.clone())
			.collect();
		if !received.is_empty() {
			for run in received {
				self.pending.remove(run);
			}
			self.rescan = true;
		}
		let floor = self.final_end(self.floor, reader, beside, mode).end;
		if floor > self.floor {
			self.raise(floor);
			folded.push(0..floor);
		}
		let lowest = match mode {
			// Only a report starts what it folds.
			Fold::Final => self.next_reported(self.floor.saturating_add(1)),
			_ => self.lowest_settled(),
		};
		let mut from = Some(reader.from.max(lowest.unwrap_or(0))).filter(|from| *from > self.floor);
		while let Some(start) = from {
			// Folded, a stretch costs two runs: only one spanning more is worth it.
			let stretch = self.final_end(start, reader, beside, mode);
			if stretch.runs > 2 {
				self.settle_stretch(start..stretch.end);
				folded.push(start..stretch.end);
			}
			from = match mode {
				// A stretch stopped where it started, at a drop, passes the whole report.
				Fold::Final if stretch.end == start => {
					let past = self
						.reported
						.get_key_value(&start)
						.map_or(start + 1, |(run, _)| run.end);
					self.next_reported(past)
				}
				Fold::Final => self.next_reported(stretch.end),
				_ => self.next_told(stretch.end.max(start + 1)),
			};
		}
		folded
	}

	/// Settle everything below `floor` for good.
	fn raise(&mut self, floor: u64) {
		self.floor = floor;
		self.delivered.forget_below(floor);
		self.skipped.forget(0..floor);
		self.written_off.remove(0..floor);
		self.pending.remove(0..floor);
		self.reported.remove(0..floor);
	}

	/// Settle `stretch` as one handed-out and reported run.
	fn settle_stretch(&mut self, stretch: std::ops::Range<u64>) {
		self.delivered.insert_run(stretch.clone());
		self.skipped.forget(stretch.clone());
		self.written_off.remove(stretch.clone());
		self.reported.insert(stretch, Report::Settled);
	}

	/// The lowest key from `key` on that was reported.
	fn next_reported(&self, key: u64) -> Option<u64> {
		self.reported
			.overlapping(&(key..u64::MAX))
			.next()
			.map(|(run, _)| run.start.max(key))
	}

	/// The lowest key from `key` on that was handed out or reported.
	fn next_told(&self, key: u64) -> Option<u64> {
		[self.delivered.next_from(key), self.next_reported(key)]
			.into_iter()
			.flatten()
			.min()
	}

	/// The lowest key from `key` on that anything in the record holds.
	fn next_held(&self, key: u64) -> Option<u64> {
		let span = key..u64::MAX;
		[
			self.next_told(key),
			self.pending
				.overlapping(&span)
				.next()
				.map(|(run, _)| run.start.max(key)),
			self.written_off.overlapping(&span).next().map(|run| run.start.max(key)),
			self.skipped.next_from(key),
		]
		.into_iter()
		.flatten()
		.min()
	}

	/// One past the gap at `key` the reader's ranges left behind, which the walk from `start`
	/// may settle (see [`Self::retire`]), if `key` starts one.
	fn retirable(&self, key: u64, start: u64, reader: &Standing, beside: &dyn Beside, below: u64) -> Option<u64> {
		let ranges = reader.ranges.as_ref()?;
		// Only between settled keys: the walk passed one, or starts on a raised floor.
		if key >= below || key == start && (key != self.floor || self.floor == 0) {
			return None;
		}
		let past = self
			.delivered
			.end()
			.max(self.reported.last_range_value().map(|(run, _)| run.end));
		let end = [
			past,
			ranges.first(key, u64::MAX).map(|run| *run.start()),
			self.next_held(key),
			beside.next_held(key),
		]
		.into_iter()
		.flatten()
		.min()?;
		(end > key).then_some(end)
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

	/// The final stretch starting at `start`, which ends at `start` itself when its key is not
	/// final, or is the group this read hands out. A retirement counts a gap the reader's
	/// ranges left behind as final too (see [`Self::retire`]).
	fn final_end(&self, start: u64, reader: &Standing, beside: &dyn Beside, fold: Fold) -> Stretch {
		let mut end = start;
		let mut runs = 0;
		let mut gaps = Vec::new();
		loop {
			let next = match self.reported.get_key_value(&end) {
				// A skip may still go out over a drop, which folding would stop.
				Some((_, Report::Dropped)) if fold == Fold::Final => break,
				Some((run, _)) => run.end,
				None if self.pending.contains_key(&end) => break,
				None => {
					let local = match fold {
						Fold::Debt => [
							self.delivered.run_end(end),
							self.written_off.get(&end).map(|run| run.end),
							self.skipped.run_end(end),
							beside.given_up_end(end),
						],
						Fold::Retire { .. } => [self.delivered.run_end(end), None, None, None],
						// Only what the record reported is final: a key handed out, or passed in
						// key order, and never reported may still be reported dropped, as a group
						// cut after it went out or one landing late below an ordered read.
						Fold::Final => [None; 4],
					};
					let Some(run_end) = local.into_iter().flatten().max() else {
						let Fold::Retire { below } = fold else {
							break;
						};
						match self.retirable(end, start, reader, beside, below) {
							Some(next) => {
								gaps.push(end);
								end = next;
								runs += 1;
								continue;
							}
							None => break,
						}
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
			// A stale skip stays for a wider budget to offer again.
			let next = match fold {
				Fold::Final => [
					self.skipped.next_from(end),
					self.written_off.overlapping(&(end..next)).next().map(|run| run.start),
					beside.next_stale(end),
				]
				.into_iter()
				.flatten()
				.fold(next, |next, stale| next.min(stale.max(end))),
				_ => next,
			};
			// The group this read hands out stays above: its publisher has yet to tell its wire
			// of it, and would take one below the floor as told already.
			let next = match reader.handed {
				Some(handed) if end <= handed && handed < next => handed,
				_ => next,
			};
			if next <= end {
				break;
			}
			end = next;
			runs += 1;
		}
		Stretch { end, runs, gaps }
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
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 0, "a lone prefix bills nothing");
		for key in (10..1_000).step_by(2) {
			record.reported.insert(key..key + 1, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default());
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
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 3 * RUN_OVERHEAD);
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default());
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
		record.settle(|| track.clone(), &mut (), Standing::default());
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default());
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
	/// folds the final stretches from where they start, and leaves those below as they are.
	#[test]
	fn a_debt_folds_from_where_the_reader_starts() {
		let (pool, track) = account();
		let mut record = Record::default();
		for key in [1_000, 1_100, 2_000, 3_000] {
			record.delivered.insert(key);
			let end = if key < 2_000 { key + 100 } else { key + 1_000 };
			record.reported.insert(key + 1..end, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 7 * RUN_OVERHEAD);
		owe(&track, pool.used());
		let started = Standing {
			from: 1_500,
			..Standing::default()
		};
		// What the reader gave up inside the stretch is settled with it, so it bills nothing.
		let mut forgotten = Forgotten(Vec::new());
		record.settle(|| track.clone(), &mut forgotten, started);
		assert_eq!(pool.used(), 5 * RUN_OVERHEAD, "the stretch from its start folds");
		assert!(record.delivered.contains(3_500) && record.told(3_500));
		assert!(!record.delivered.contains(1_050), "below its start stays as it was");
		assert!(!record.told(1_500), "below its start stays owed");
		assert_eq!(
			forgotten.0,
			std::slice::from_ref(&(2_000..4_000)),
			"the stretch, beside the record too"
		);
	}

	/// Under a debt, every final stretch folds, also those above a key still owed, such as a
	/// miss its fills have yet to answer: what stays is bounded by the keys owed.
	#[test]
	fn a_debt_folds_every_stretch_past_an_owed_key() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(1_000);
		for key in [2_000, 3_000, 5_000, 6_000] {
			record.delivered.insert(key);
			record.reported.insert(key + 1..key + 1_000, Report::Dropped);
		}
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 8 * RUN_OVERHEAD);
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 4 * RUN_OVERHEAD, "both stretches fold");
		assert!(record.told(2_500) && record.told(6_500));
		assert!(!record.told(1_500) && !record.told(4_500), "the keys owed stay owed");
	}

	/// What a reader holds beside its record, here only what it was told to forget.
	struct Forgotten(Vec<std::ops::Range<u64>>);

	impl Beside for Forgotten {
		fn runs(&self) -> u64 {
			0
		}

		fn given_up_end(&self, _key: u64) -> Option<u64> {
			None
		}

		fn next_held(&self, _key: u64) -> Option<u64> {
			None
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
		record.settle(|| track.clone(), &mut (), Standing::default());
		owe(&track, pool.used());
		record.settle(
			|| track.clone(),
			&mut (),
			Standing {
				handed: Some(3),
				..Standing::default()
			},
		);
		assert!(record.told(2), "the drops below it fold");
		assert!(!record.told(3), "the handed group stays to be told");
	}

	/// A run the reader gave up beside its record, such as a drop it took from the track's drop
	/// set, is its own final verdict: a debt folds the floor over it rather than stopping at
	/// the gap, and forgets it below the floor.
	#[test]
	fn a_debt_folds_over_what_the_reader_gave_up_beside_it() {
		struct GivenUp(Option<Range<u64>>);
		impl Beside for GivenUp {
			fn runs(&self) -> u64 {
				u64::from(self.0.is_some())
			}
			fn given_up_end(&self, key: u64) -> Option<u64> {
				self.0.as_ref().filter(|run| run.contains(&key)).map(|run| run.end)
			}
			fn next_held(&self, key: u64) -> Option<u64> {
				self.0
					.as_ref()
					.filter(|run| run.end > key)
					.map(|run| run.start.max(key))
			}
			fn forget(&mut self, folded: Range<u64>) {
				self.0 = self
					.0
					.take()
					.filter(|run| run.start >= folded.end || run.end <= folded.start);
			}
		}
		let (pool, track) = account();
		let mut record = Record::default();
		let mut beside = GivenUp(Some(1..2));
		record.delivered.insert(0);
		record.delivered.insert(2);
		record.reported.insert(4..5, Report::Dropped);
		record.settle(|| track.clone(), &mut beside, Standing::default());
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut beside, Standing::default());
		assert!(record.told(1) && record.told(2), "folded over the gap it gave up");
		assert_eq!(beside.0, None, "and forgot it below the floor");
		assert!(!record.told(3), "a key nothing settled stops the fold");
	}

	/// A debt that needs a run this reader was promised, one given up and not reported yet or
	/// a run above one, never ends it, at any capacity: the run is owed for a moment, and
	/// what is final below it still folds.
	#[test]
	fn a_debt_on_a_promise_leaves_the_reader() {
		for capacity in [0, 1] {
			let pool = cache::Pool::new(cache::Config::default().with_capacity(capacity));
			let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
			let mut record = record_with_promise();
			record.settle(|| track.clone(), &mut (), Standing::default());
			owe(&track, pool.used());
			record.settle(|| track.clone(), &mut (), Standing::default());
			assert!(record.told(1), "capacity {capacity}: the drop below it folded");
			assert!(!record.told(2), "capacity {capacity}: the promise stays owed");
			assert_eq!(
				pool.used(),
				499 * RUN_OVERHEAD,
				"capacity {capacity}: the promise and the reports above it"
			);
		}
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
		record.settle(|| track.clone(), &mut (), Standing::default());
		owe(&track, RUN_OVERHEAD);
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 2 * RUN_OVERHEAD, "the drop folded");
		assert!(!record.told(2), "the run given up is still owed");
	}

	/// A pool over its capacity by cached groups its tracks could not evict yet owes its
	/// readers only what their records are over: none, when they fit.
	#[test]
	fn cached_groups_past_the_capacity_owe_no_reader() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1 << 20));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let mut cached = track.charge();
		cached.add(4 << 20);
		let mut record = record_with_promise();
		record.settle(|| track.clone(), &mut (), Standing::default());
		let excess = pool.excess().expect("the cached group is over");
		owe(&track, excess);
		assert_eq!(
			record.bill.as_ref().map(cache::Reader::debt),
			Some(0),
			"the record fits the capacity"
		);
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

	/// A record on a pool with room, so it owes no debt.
	fn roomy() -> (cache::Pool, Arc<cache::Track>) {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1 << 30));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		(pool, track)
	}

	/// Standing for a reader whose ranges hold each first to last key in `runs`.
	fn ranged(runs: &[(u64, u64)]) -> Standing {
		Standing {
			ranges: Some(Cover::new(runs.iter().map(|(first, last)| *first..=*last))),
			..Standing::default()
		}
	}

	/// Once the ranges move, the gaps they left between settled keys retire without a debt,
	/// past one per run the ranges hold, lowest first: the runs around them fold into one.
	/// The gap kept, and the keys below the lowest settled, stay owed should the ranges widen
	/// over them again.
	#[test]
	fn moved_ranges_retire_the_gaps_they_left() {
		let (_pool, track) = roomy();
		let mut record = Record::default();
		for key in (10..=50).step_by(10) {
			record.delivered.insert(key);
		}
		record.settle(|| track.clone(), &mut (), ranged(&[(50, 50)]));
		assert_eq!(record.delivered.runs(), 2, "10 through 40, then 50");
		assert_eq!(record.reported.len(), 1, "and one report");
		assert!(
			record.delivered.contains(25) && record.told(25),
			"a retired gap is settled"
		);
		assert!(!record.delivered.contains(45), "the highest gap is kept");
		assert!(!record.delivered.contains(5), "below the lowest settled stays owed");

		// Its ranges move above it: the gap kept retires for the one the move left.
		record.delivered.insert(70);
		record.settle(|| track.clone(), &mut (), ranged(&[(70, 70)]));
		assert_eq!(record.delivered.runs(), 2, "10 through 50, then 70");
		assert_eq!(record.reported.len(), 1);
		assert!(record.delivered.contains(45));
		assert!(!record.delivered.contains(60));
	}

	/// A gap the ranges still hold stays owed, as does one owed for a moment: a run given up
	/// and not reported yet, or a miss still asked. So does a stale skip, which a wider budget
	/// offers again while no debt folds it. Each stops the stretch that would retire past it.
	#[test]
	fn moved_ranges_keep_what_is_still_owed() {
		struct Asked(Range<u64>);
		impl Beside for Asked {
			fn runs(&self) -> u64 {
				0
			}
			fn given_up_end(&self, _key: u64) -> Option<u64> {
				None
			}
			fn next_held(&self, key: u64) -> Option<u64> {
				(self.0.end > key).then(|| self.0.start.max(key))
			}
			fn forget(&mut self, _folded: Range<u64>) {}
		}
		let (_pool, track) = roomy();
		let mut record = Record::default();
		for key in (10..=80).step_by(10) {
			record.delivered.insert(key);
		}
		record
			.pending
			.insert(25..26, Pending::own(Hole::drop(StreamError::NotFound)));
		record.skipped.insert(65, Duration::from_secs(1));
		let mut asked = Asked(15..16);
		record.settle(|| track.clone(), &mut asked, ranged(&[(35, 38)]));
		for owed in [15, 25, 36, 65, 75] {
			assert!(!record.delivered.contains(owed), "{owed} stays owed");
		}
		for retired in [45, 55, 62] {
			assert!(record.delivered.contains(retired), "{retired} retired");
		}
	}

	/// A gap retires only once the ranges move: the same ranges again judge nothing anew.
	#[test]
	fn unmoved_ranges_retire_nothing_anew() {
		let (_pool, track) = roomy();
		let mut record = Record::default();
		let reader = ranged(&[(0, 0)]);
		record.delivered.insert(0);
		record.settle(|| track.clone(), &mut (), reader.clone());
		for key in [10, 20, 30] {
			record.delivered.insert(key);
		}
		record.settle(|| track.clone(), &mut (), reader);
		assert_eq!(record.delivered.runs(), 4, "no move, no retirement");
	}

	/// A debt never exceeds what the records are over the capacity, whatever paid it, and is
	/// forgiven once they are back within it: nothing needs the room any more.
	#[test]
	fn a_debt_is_paid_by_any_shrink_and_forgiven_without_pressure() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(2 * RUN_OVERHEAD));
		let track = cache::Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let mut bill = track.reader();
		bill.set(4 * RUN_OVERHEAD);
		owe(&track, 3 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 2 * RUN_OVERHEAD, "only what the record is over");
		bill.set(3 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), RUN_OVERHEAD, "the shrink paid what it freed");
		bill.set(2 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 0, "within its capacity, the pool forgives the rest");
		bill.set(5 * RUN_OVERHEAD);
		assert_eq!(bill.debt(), 0, "a later pressure owes nothing of it");
	}
}

//! What one reader did with each group key, as runs: handed out, skipped as stale, given
//! up, and what its wire was told.

use std::ops::Range;
use std::sync::Arc;

use rangemap::{RangeMap, RangeSet};

use super::cache;
use super::delivered::Delivered;
use super::skipped::Skipped;
use super::subscription::{Accounted, Cover, minus};
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

/// How a reader reported a run, and what its wire was told. Every wire report is final:
/// served, skipped, or dropped (stale `OLD` included), so adjacent final runs coalesce in
/// `reported`. Only an ordered reader's unreturned stale gap stays [`Self::Withdrawable`], so
/// a wider budget can take it back before it is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
	/// Served, skipped, dropped on the wire, or returned as a gap.
	Final,
	/// Unreturned stale gap buffered for a key-order reader, which a wider budget may
	/// withdraw before it is returned.
	Withdrawable,
}

impl Record {
	/// Report the first part of `range` nothing reported already, as `hole` says. `None`
	/// when every part was. `withdrawable` is true only for a key-order reader that may take
	/// back an unreturned stale gap on a budget widening.
	///
	/// A declared skip (`Hole::Skip`) was never produced, so when it strictly bridges two
	/// already-handed-out runs in `delivered` it merges them into one run.
	pub fn report(&mut self, range: Range<u64>, hole: &Hole, withdrawable: bool) -> Option<Range<u64>> {
		let range = range.start.max(self.floor)..range.end;
		if range.is_empty() {
			return None;
		}
		let gap = self.reported.gaps(&range).next()?;
		let report = match withdrawable && hole.stale_budget().is_some() {
			true => Report::Withdrawable,
			false => Report::Final,
		};
		self.reported.insert(gap.clone(), report);
		if *hole == Hole::Skip {
			self.delivered.bridge_skip(gap.clone());
		}
		Some(gap)
	}

	/// The lowest pending run's first part nothing reported already, now reported, with why;
	/// the rest of that run waits its turn.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn next_pending(&mut self) -> Option<(Range<u64>, Hole)> {
		while let Some((range, pending)) = self
			.pending
			.iter()
			.next()
			.map(|(range, pending)| (range.clone(), pending.clone()))
		{
			self.pending.remove(range.clone());
			if let Some(gap) = self.report(range.clone(), &pending.hole, false) {
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

	/// Whether the wire was told of every key in `range`.
	pub fn told_all(&self, range: Range<u64>) -> bool {
		let range = range.start.max(self.floor)..range.end;
		range.is_empty() || self.reported.gaps(&range).next().is_none()
	}

	/// The first untold gap in `range` that `find` accepts, evaluated lazily without
	/// materializing the remaining gaps.
	pub fn find_untold_gap<T>(&self, range: Range<u64>, mut find: impl FnMut(Range<u64>) -> Option<T>) -> Option<T> {
		let range = range.start.max(self.floor)..range.end;
		if range.is_empty() {
			return None;
		}
		self.reported.gaps(&range).find_map(&mut find)
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

	/// Record that the wire was told of declared skip `range` (for example a `GROUP_END` inner
	/// span or `SUBSCRIBE_DROP(SKIP)`), bridging it in `delivered` if it connects handed-out runs.
	pub fn tell_skip(&mut self, range: Range<u64>) {
		if !range.is_empty() {
			self.reported.insert(range.clone(), Report::Final);
			self.delivered.bridge_skip(range);
		}
	}

	/// Bridge a declared skip `range` in `delivered` after it was told to the wire.
	pub fn bridge_skip(&mut self, range: Range<u64>) {
		self.delivered.bridge_skip(range);
	}

	/// Whether this record has already recorded reports or folded a report floor.
	pub fn has_reports(&self) -> bool {
		self.floor > 0 || !self.reported.is_empty()
	}

	/// Forget every report of `range`, which the wire was never told: a run given up below
	/// where delivery starts, which a later widening may still serve or report.
	pub fn untell(&mut self, range: Range<u64>) {
		if !range.is_empty() {
			self.reported.remove(range);
		}
	}
}

impl Accounted for Record {
	fn first_gap(&self, range: &Range<u64>) -> Option<u64> {
		let range = range.start.max(self.floor)..range.end;
		if range.is_empty() {
			return None;
		}
		self.reported.gaps(&range).next().map(|gap| gap.start)
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
	/// Forget every run within `folded`, which the record folded as settled or forgot.
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

/// A final stretch of keys a fold may settle, or a retirement forget.
struct Stretch {
	/// One past its last key.
	end: u64,
	/// How many runs it spans.
	runs: usize,
	/// Where each gap it passes through that the reader's ranges left behind starts.
	gaps: Vec<u64>,
}

/// What a walk through the gaps the reader's ranges left behind found, and forgot.
#[derive(Default)]
struct Walk {
	gaps: Vec<u64>,
	forgot: Vec<std::ops::Range<u64>>,
}

/// Which keys a fold may settle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fold {
	/// Under a debt: every final key, a stale skip and what `beside` gave up included, which
	/// costs a wider budget the offer of them again.
	Debt,
	/// After the ranges moved: only groups handed out, reports, and the gaps left outside the
	/// ranges that start below `below`, so a stale skip stays for a wider budget to offer
	/// again. What it passes is forgotten, not settled.
	Retire { below: u64 },
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
		let bytes = (self.runs() + beside.runs()) * RUN_OVERHEAD;
		let bill = match &mut self.bill {
			Some(bill) => bill,
			None if bytes == 0 => return,
			None => self.bill.insert(account().reader()),
		};
		bill.set(bytes);
		let owed = bill.debt() > 0;
		if !owed && !moved {
			return;
		}
		let mut folded = Vec::new();
		if moved {
			folded.extend(self.retire(&reader, &*beside));
		}
		if owed {
			folded.extend(self.fold(&reader, &*beside));
		}
		for folded in folded {
			beside.forget(folded);
		}
		let bytes = (self.runs() + beside.runs()) * RUN_OVERHEAD;
		self.bill.as_mut().expect("opened above").set(bytes);
	}

	/// Retire the gaps the reader's ranges left behind, which nothing settles while they stay
	/// where they are: keys nothing settled, between two that something did, that the ranges
	/// no longer hold, and that nothing owes for a moment (a run given up and not reported
	/// yet, or a miss `beside` still asks). A record keeps as many such gaps as its ranges
	/// hold runs, so a narrowing its reader undoes still offers what it narrowed past, as the
	/// ranges themselves bound; past that, the lowest retire. The record forgets each final
	/// stretch through them outside the ranges, above its handed-out prefix, so a reader whose
	/// ranges keep moving to disjoint groups keeps a record no larger than its ranges' share.
	///
	/// A retired gap was never served nor reported, so settling it would leave ranges that
	/// widen over it again owed a key no group or report ever comes for. Forgetting it, and the
	/// keys around it, asks for the whole stretch afresh instead, as a new reader would: its
	/// gaps are served, and a group handed out there before may be handed out again, which
	/// the same identity makes harmless.
	///
	/// Only a stretch through a retired gap is forgotten, and stale skips stop one: without a
	/// debt, the record otherwise stays as it was, and a wider budget still offers a stale skip
	/// again. Returns the runs it forgot.
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
				self.walk(reader, beside, below, true).forgot
			}
		}
	}

	/// Walk each final stretch through the gaps the ranges left behind below `below`, from the
	/// floor up, forgetting each when `fold` says so.
	fn walk(&mut self, reader: &Standing, beside: &dyn Beside, below: u64, fold: bool) -> Walk {
		let mut walk = Walk::default();
		let retire = Fold::Retire { below };
		let floor = self.final_end(self.floor, reader, beside, retire);
		walk.gaps.extend(&floor.gaps);
		if fold && !floor.gaps.is_empty() {
			walk.forgot.extend(self.forget_outside(self.floor..floor.end, reader));
		}
		let mut from = floor.end.max(self.floor);
		while let Some(start) = self.next_told(from) {
			let stretch = self.final_end(start, reader, beside, retire);
			walk.gaps.extend(&stretch.gaps);
			if fold && !stretch.gaps.is_empty() {
				walk.forgot.extend(self.forget_outside(start..stretch.end, reader));
			}
			from = stretch.end.max(start + 1);
		}
		walk
	}

	/// Forget what the record holds of `stretch` outside the reader's ranges, above the
	/// handed-out prefix: keys the ranges no longer hold, which they ask for afresh should they
	/// cover them again. The prefix stays, as it bills nothing. Returns the runs it forgot.
	fn forget_outside(&mut self, stretch: Range<u64>, reader: &Standing) -> Vec<Range<u64>> {
		let start = stretch.start.max(self.delivered.floor());
		let Some(ranges) = reader.ranges.as_ref().filter(|_| start < stretch.end) else {
			return Vec::new();
		};
		let forgot: Vec<Range<u64>> = (minus(start..=stretch.end - 1, ranges).into_iter())
			.map(|run| *run.start()..*run.end() + 1)
			.collect();
		for run in &forgot {
			self.delivered.remove(run.clone());
			self.reported.remove(run.clone());
		}
		forgot
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
	fn fold(&mut self, reader: &Standing, beside: &dyn Beside) -> Vec<std::ops::Range<u64>> {
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
		let budget = self.delivered.budget_floor();
		if budget > self.floor {
			// Keys retired by the delivery budget are already in `delivered` and never offered
			// again; under pool debt, drop stale write-offs below that prefix without raising
			// the report floor over keys that were never reported.
			self.written_off.remove(0..budget);
		}
		let floor = self.final_end(self.floor, reader, beside, Fold::Debt).end;
		if floor > self.floor {
			self.raise(floor);
			folded.push(0..floor);
		}
		let mut from = Some(reader.from.max(self.lowest_settled().unwrap_or(0))).filter(|from| *from > self.floor);
		while let Some(start) = from {
			// Folded, a stretch costs two runs: only one spanning more is worth it.
			let stretch = self.final_end(start, reader, beside, Fold::Debt);
			if stretch.runs > 2 {
				self.settle_stretch(start..stretch.end);
				folded.push(start..stretch.end);
			}
			let next = stretch.end.max(start + 1);
			from = [
				self.delivered.next_from(next.max(budget)),
				self.reported
					.overlapping(&(next..u64::MAX))
					.next()
					.map(|(run, _)| run.start.max(next)),
			]
			.into_iter()
			.flatten()
			.min();
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
		self.reported.insert(stretch, Report::Final);
	}

	/// The lowest key from `key` on that was handed out or reported.
	fn next_told(&self, key: u64) -> Option<u64> {
		let reported = self.reported.overlapping(&(key..u64::MAX)).next();
		[
			self.delivered.next_from(key),
			reported.map(|(run, _)| run.start.max(key)),
		]
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
			let (next, step) = match self.reported.get_key_value(&end) {
				Some((run, _)) => {
					// Count distinct runs overlapping this reported run up to the folding
					// threshold so alternating served groups and drops under one coalesced
					// `Report::Final` run compact under debt.
					let span = end..run.end;
					let mut extra = self.delivered.count_runs_in(span.clone(), 2);
					if extra < 2 && fold == Fold::Debt {
						extra += self.written_off.overlapping(&span).take(2 - extra).count();
						if extra < 2 && self.skipped.next_from(end).is_some_and(|key| key < run.end) {
							extra += 1;
						}
					}
					(run.end, 1 + extra)
				}
				None if self.pending.contains_key(&end) => break,
				None => {
					let local = match fold {
						Fold::Debt => [
							(end >= self.delivered.budget_floor())
								.then(|| self.delivered.run_end(end))
								.flatten(),
							self.written_off.get(&end).map(|run| run.end),
							self.skipped.run_end(end),
							beside.given_up_end(end),
						],
						Fold::Retire { .. } => [self.delivered.run_end(end), None, None, None],
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
					let next = [
						self.reported.overlapping(&span).next().map(|(run, _)| run.start),
						self.pending.overlapping(&span).next().map(|(run, _)| run.start),
					]
					.into_iter()
					.flatten()
					.fold(run_end, |run_end, start| run_end.min(start.max(end)));
					(next, 1)
				}
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
			runs += step;
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
			record.reported.insert(key..key + 1, Report::Final);
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
		record.reported.insert(1..u64::MAX / 2, Report::Final);
		record.delivered.insert(u64::MAX / 2);
		record
			.reported
			.insert(u64::MAX / 2 + 1..u64::MAX / 2 + 2, Report::Final);
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 3 * RUN_OVERHEAD);
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert_eq!(pool.used(), 0, "all of it folded");
		assert!(
			record.told(7) && record.delivered.contains(7),
			"a folded drop stays dropped"
		);
		assert_eq!(record.report(0..u64::MAX / 2 + 2, &Hole::Skip, false), None);
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
			record.reported.insert(key + 1..key + 1_000, Report::Final);
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
			record.reported.insert(key + 1..end, Report::Final);
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
			record.reported.insert(key + 1..key + 1_000, Report::Final);
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
		record.reported.insert(1..3, Report::Final);
		record.delivered.insert(3);
		record.reported.insert(5..6, Report::Final);
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
		record.reported.insert(4..5, Report::Final);
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
		record.reported.insert(1..2, Report::Final);
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
		record.reported.insert(1..2, Report::Final);
		record
			.pending
			.insert(2..3, Pending::own(Hole::drop(StreamError::NotFound)));
		for key in (5..1_000).step_by(2) {
			record.reported.insert(key..key + 1, Report::Final);
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
	/// past one per run the ranges hold, lowest first: the record forgets each stretch through
	/// them, so none of it counts as handed out or told, and ranges widening over it again ask
	/// for it afresh. A retired gap is never settled, which would leave such ranges owed a key
	/// nothing comes for.
	#[test]
	fn moved_ranges_retire_the_gaps_they_left() {
		let (_pool, track) = roomy();
		let mut record = Record::default();
		for key in (10..=50).step_by(10) {
			record.delivered.insert(key);
			record.tell(key..key + 1, Report::Final);
		}
		record.settle(|| track.clone(), &mut (), ranged(&[(50, 50)]));
		assert_eq!(record.delivered.runs(), 1, "50 alone");
		assert_eq!(record.reported.len(), 1, "and its report");
		for key in [10, 25, 40] {
			assert!(!record.delivered.contains(key) && !record.told(key), "{key} forgotten");
		}

		// Its ranges move above it: the gap the move left retires.
		for key in [60, 70] {
			record.delivered.insert(key);
			record.tell(key..key + 1, Report::Final);
		}
		record.settle(|| track.clone(), &mut (), ranged(&[(70, 70)]));
		assert_eq!(record.delivered.runs(), 1, "70 alone");
		assert!(!record.told(55) && !record.told(60));
		assert!(record.told(70));
	}

	/// A key the ranges still hold stays as it was, as does a run owed for a moment: one given
	/// up and not reported yet, or a miss still asked. So does a stale skip, which a wider
	/// budget offers again while no debt folds it. Each stops the stretch that would retire
	/// past it, and the highest gap stays.
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
		record.settle(|| track.clone(), &mut asked, ranged(&[(36, 40)]));
		for owed in [25, 65] {
			assert_eq!(record.next_held(owed), Some(owed), "{owed} stays owed");
		}
		for kept in [40, 70, 80] {
			assert!(record.delivered.contains(kept), "{kept} kept");
		}
		for retired in [10, 20, 30, 50, 60] {
			assert!(!record.delivered.contains(retired), "{retired} forgotten");
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

	/// Served groups, declared skips, and wire-final stale drops all coalesce into one final run
	/// in `reported`, while `delivered` absorbs skips only rightward from delivered groups so a
	/// lagging viewer skipping groups as stale never piles up disjoint runs in `delivered`.
	#[test]
	fn final_reports_coalesce_and_skips_never_fragment_delivered() {
		let mut record = Record::default();
		// Lagging viewer: stale OLD drops alternating with skips, nothing handed out.
		for i in 0..500u64 {
			let key = 1_000 + i * 17;
			assert_eq!(
				record.report(key..key + 1, &Hole::stale(Duration::from_secs(1)), false),
				Some(key..key + 1)
			);
			assert_eq!(
				record.report(key + 1..key + 17, &Hole::Skip, false),
				Some(key + 1..key + 17)
			);
		}
		assert_eq!(record.reported.len(), 1, "stale wire drops and skips coalesce");
		assert_eq!(record.delivered.runs(), 0, "skips alone never create runs in delivered");

		// Healthy sparse live reader: handed-out keys bridge across declared skips.
		let mut healthy = Record::default();
		for i in 0..500u64 {
			let key = 1_000 + i * 17;
			healthy.delivered.insert(key);
			healthy.tell(key..key + 1, Report::Final);
			if i > 0 {
				assert_eq!(healthy.report(key - 16..key, &Hole::Skip, false), Some(key - 16..key));
			}
		}
		assert_eq!(healthy.reported.len(), 1);
		assert_eq!(healthy.delivered.runs(), 1);
	}

	/// A key-order reader's unreturned stale gap stays withdrawable until finalized or untold,
	/// and `untell` never punches a hole in `delivered`.
	#[test]
	fn withdrawable_report_stays_distinct_and_untell_preserves_delivered() {
		let mut record = Record::default();
		record.delivered.insert(10);
		record.tell(10..11, Report::Final);
		assert_eq!(
			record.report(11..12, &Hole::stale(Duration::from_secs(1)), true),
			Some(11..12)
		);
		assert_eq!(record.reported.get(&11), Some(&Report::Withdrawable));
		assert!(!record.delivered.contains(11), "a stale drop is never delivered");

		// Passing 11 in `delivered` and then withdrawing its report keeps `delivered` intact.
		record.delivered.forget_below(12);
		record.untell(11..12);
		assert!(!record.told(11));
		assert!(
			record.delivered.contains(11),
			"untell never punches a hole in delivered"
		);

		// Finalizing a withdrawable report coalesces it with neighbouring final runs.
		assert_eq!(
			record.report(11..12, &Hole::stale(Duration::from_secs(1)), true),
			Some(11..12)
		);
		record.tell(12..13, Report::Final);
		assert_eq!(record.reported.len(), 3);
		record.tell(11..12, Report::Final);
		assert_eq!(record.reported.len(), 1);
	}

	/// Differential property test: across random sparse deliveries, out-of-order skips, final and
	/// stale drops, `tell`, and `untell`, `Record` matches a ground-truth key-by-key model:
	/// dropped-only keys never appear in `delivered`, `told` and `Accounted::first_gap` stay
	/// exact, and steady sparse patterns stay O(1) runs.
	#[test]
	fn structural_coalescing_matches_reference_model() {
		use std::collections::{BTreeMap, BTreeSet};

		struct Rng(u64);
		impl Rng {
			fn next(&mut self) -> u64 {
				self.0 ^= self.0 << 13;
				self.0 ^= self.0 >> 7;
				self.0 ^= self.0 << 17;
				self.0
			}
			fn below(&mut self, n: u64) -> u64 {
				self.next() % n
			}
		}

		for seed in 1..=64u64 {
			let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
			let mut record = Record::default();
			let mut handed = BTreeSet::<u64>::new();
			let mut skipped = BTreeSet::<u64>::new();
			let mut reported = BTreeMap::<u64, Report>::new();

			const MAX_KEY: u64 = 128;
			for _ in 0..250 {
				match rng.below(6) {
					// Hand out a non-skipped key.
					0 => {
						let key = rng.below(MAX_KEY);
						if !skipped.contains(&key) {
							record.delivered.insert(key);
							handed.insert(key);
						}
					}
					// Report a skip on a range containing no handed-out key.
					1 => {
						let start = rng.below(MAX_KEY);
						let end = (start + 1 + rng.below(12)).min(MAX_KEY);
						if (start..end).all(|k| !handed.contains(&k)) {
							let withdrawable = rng.below(2) == 0;
							while let Some(gap) = record.report(start..end, &Hole::Skip, withdrawable) {
								for k in gap {
									reported.insert(k, Report::Final);
								}
							}
							for k in start..end {
								skipped.insert(k);
							}
						}
					}
					// Report a final or stale drop.
					2 => {
						let start = rng.below(MAX_KEY);
						let end = (start + 1 + rng.below(8)).min(MAX_KEY);
						let stale = rng.below(2) == 0;
						let withdrawable = rng.below(2) == 0;
						let hole = match stale {
							true => Hole::stale(Duration::from_secs(1)),
							false => Hole::drop(StreamError::NotFound),
						};
						let expected_report = match stale && withdrawable {
							true => Report::Withdrawable,
							false => Report::Final,
						};
						while let Some(gap) = record.report(start..end, &hole, withdrawable) {
							for k in gap {
								assert!(!reported.contains_key(&k), "seed {seed}: duplicate report at {k}");
								reported.insert(k, expected_report);
							}
						}
					}
					// Tell a range final.
					3 => {
						let start = rng.below(MAX_KEY);
						let end = (start + 1 + rng.below(6)).min(MAX_KEY);
						record.tell(start..end, Report::Final);
						for k in start..end {
							reported.insert(k, Report::Final);
						}
					}
					// Untell a range.
					4 => {
						let start = rng.below(MAX_KEY);
						let end = (start + 1 + rng.below(6)).min(MAX_KEY);
						record.untell(start..end);
						for k in start..end {
							reported.remove(&k);
						}
					}
					// Bridge a skip strictly between handed-out runs.
					_ => {
						let start = rng.below(MAX_KEY);
						let end = (start + 1 + rng.below(12)).min(MAX_KEY);
						if (start..end).all(|k| !handed.contains(&k)) {
							let before_runs = record.delivered.runs();
							record.delivered.bridge_skip(start..end);
							assert!(
								record.delivered.runs() <= before_runs,
								"seed {seed}: bridge_skip grew delivered from {before_runs} to {}",
								record.delivered.runs()
							);
							for k in start..end {
								skipped.insert(k);
							}
						}
					}
				}

				// Every handed-out key is in `delivered`; no key that is neither handed-out nor
				// skipped (i.e. dropped-only or untouched) ever appears in `delivered`.
				for k in 0..MAX_KEY {
					if handed.contains(&k) {
						assert!(record.delivered.contains(k), "seed {seed}: missing handed key {k}");
					} else if !skipped.contains(&k) {
						assert!(
							!record.delivered.contains(k),
							"seed {seed}: non-skipped dropped/unhanded key {k} leaked into delivered"
						);
					}
					assert_eq!(record.told(k), reported.contains_key(&k), "seed {seed}: told({k})");
					assert_eq!(
						record.reported.get(&k).copied(),
						reported.get(&k).copied(),
						"seed {seed}: report variant at {k}"
					);
				}

				// Accounted::first_gap matches the reference map.
				let q_start = rng.below(MAX_KEY);
				let q_end = (q_start + rng.below(MAX_KEY - q_start + 1)).min(MAX_KEY);
				let expected_gap = (q_start..q_end).find(|k| !reported.contains_key(k));
				assert_eq!(
					record.first_gap(&(q_start..q_end)),
					expected_gap,
					"seed {seed}: first_gap({q_start}..{q_end})"
				);
			}
		}
	}

	/// Alternating served groups and real drops above key 0 coalesce into one `Report::Final` run
	/// in `reported` while leaving one run per served group in `delivered`. Under cache debt,
	/// `Record::fold` counts those fragmented `delivered` runs and compacts the stretch to 2 runs.
	#[test]
	fn a_debt_folds_fragmented_delivered_runs_under_one_final_report() {
		let (pool, track) = account();
		let mut record = Record::default();
		const BASE: u64 = 1_000;
		for n in 0..500u64 {
			let key = BASE + 2 * n;
			record.delivered.insert(key);
			record.tell(key..key + 1, Report::Final);
			assert_eq!(
				record.report(key + 1..key + 2, &Hole::drop(StreamError::NotFound), false),
				Some(key + 1..key + 2)
			);
		}
		assert_eq!(record.reported.len(), 1, "reported coalesced into one Final run");
		assert_eq!(record.delivered.runs(), 500, "delivered fragmented by real drops");
		record.settle(
			|| track.clone(),
			&mut (),
			Standing {
				from: BASE,
				..Default::default()
			},
		);
		assert!(pool.used() >= 500 * RUN_OVERHEAD);

		owe(&track, pool.used());
		record.settle(
			|| track.clone(),
			&mut (),
			Standing {
				from: BASE,
				..Default::default()
			},
		);
		assert_eq!(record.delivered.runs(), 1, "delivered compacted to one run");
		assert_eq!(record.reported.len(), 1, "reported stays one run");
		assert_eq!(
			pool.used(),
			RUN_OVERHEAD,
			"only the reported run bills above one delivered run"
		);
		assert!(!record.told(BASE - 1), "keys below the reader's start stay untold");
	}

	/// Keys retired solely by the freshness budget (`Delivered::retire_below`) stay out of the
	/// report floor under cache debt, so widening back over them still reports skips there.
	#[test]
	fn a_debt_does_not_promote_budget_retired_delivery_prefix_into_report_floor() {
		let (pool, track) = account();
		let mut record = Record::default();
		record.delivered.insert(2);
		record.delivered.insert(4);
		record.delivered.retire_below(5);
		assert!(record.delivered.contains(1), "delivery prefix covers key 1");
		// Bill a run so pool debt actually triggers `Record::fold`.
		record.tell(10..11, Report::Final);
		record.settle(
			|| track.clone(),
			&mut (),
			Standing {
				from: 2,
				..Default::default()
			},
		);
		owe(&track, pool.used());
		record.settle(
			|| track.clone(),
			&mut (),
			Standing {
				from: 2,
				..Default::default()
			},
		);
		assert!(!record.told(1), "budget-retired prefix must not raise the report floor");
		assert!(!record.told_all(1..2));

		// Once 0..5 is actually reported, debt folding raises the floor over it normally.
		record.tell(0..5, Report::Final);
		record.settle(|| track.clone(), &mut (), Standing::default());
		owe(&track, pool.used());
		record.settle(|| track.clone(), &mut (), Standing::default());
		assert!(record.told(1) && record.told_all(0..5));
	}

	/// `told_all` and `find_untold_gap` match `untold` while stopping at the first accepted gap.
	#[test]
	fn told_all_and_find_untold_gap_match_untold_lazily() {
		let mut record = Record::default();
		for k in (0..200).step_by(2) {
			record.tell(k..k + 1, Report::Final);
		}
		assert!(!record.told_all(0..200));
		assert!(record.told_all(2..3));

		let mut visited = 0usize;
		let found = record.find_untold_gap(0..200, |gap| {
			visited += 1;
			(gap.start >= 5).then_some(gap)
		});
		assert_eq!(found, Some(5..6));
		assert_eq!(
			visited, 3,
			"stopped at 5..6 without materializing the remaining 97 gaps"
		);
	}
}

//! A reader's misses: the sequences it reads that the cache doesn't hold and the live feed
//! will never deliver, asked of the track's fills until each arrives or is given up.
//!
//! Nothing above the newest group the feed produced is a miss, since it arrives live, so a
//! fill is bounded by construction. Below it, a hole is a miss once the feed is past it:
//! under where the feed starts, or accounted for by the arrival ledger without being cached
//! (evicted, or a gap the feed skipped). A hole the ledger still waits for is deferred and
//! judged again as the feed moves.
//!
//! Each sequence is judged once, so a group evicted after the reader judged it cached is
//! not a miss: judging it again would refill a group an arrival-order reader already took.
//!
//! A start is no promise, since a group below it may still arrive late. A hole under it is
//! asked at once, but a refusal is final only once the ledger gives up on it too; until
//! then the live feed may still deliver it.

use std::{
	collections::BTreeSet,
	ops::{self, RangeInclusive},
	task::Poll,
	time::Duration,
};

use rangemap::{RangeInclusiveSet, RangeSet};

use super::fill::{Ask, FillState, Join};
use super::{Drift, Order, Position, TrackState};
use crate::Error;

/// What one reader asks its fills with.
pub(super) struct Asking {
	pub priority: i8,
	pub max_delay: Option<Duration>,
	pub order: Order,
}

/// What a probe judges against: the span read, the feed's newest group, where the feed
/// and readers start, and the drops revision.
type JudgedAt = (u64, u64, Option<u64>, Option<u64>, Option<u64>, Option<u64>, u64);

/// The misses of one cursor, judged as what it reads or the feed moves.
#[derive(Default)]
pub(super) struct Misses {
	/// What the last probe judged against. A probe that sees the same skips judging.
	at: Option<JudgedAt>,
	/// The sequences judged already, within what the reader reads up to the newest group.
	judged: RangeSet<u64>,
	/// Holes the live feed may still deliver, judged again on every probe.
	deferred: RangeSet<u64>,
	/// Asked under the feed's start and refused, never asked again: given up once the feed
	/// gives up on them, unless they arrive first.
	refused: RangeSet<u64>,
	/// The fills this reader waits on.
	joins: Vec<Join>,
	/// The sequences of `joins` still unanswered.
	wanted: RangeSet<u64>,
	/// Answered by a fill and cached, not handed out yet.
	filled: BTreeSet<u64>,
	/// Never asked again: named dropped to the reader.
	given_up: RangeSet<u64>,
}

/// A run of misses the reader gives up, for the drop cursor to name.
pub(super) type GivenUp = Vec<ops::Range<u64>>;

impl Misses {
	/// The lowest sequence still to come from a fill: asked, or answered and not handed out.
	pub fn lowest(&self) -> Option<u64> {
		let wanted = self.wanted.iter().next().map(|run| run.start);
		let filled = self.filled.first().copied();
		match (wanted, filled) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		}
	}

	/// Whether a fill still owes this reader a group.
	pub fn is_waiting(&self) -> bool {
		!self.wanted.is_empty()
	}

	/// The lowest sequence asked and not answered yet.
	pub fn lowest_wanted(&self) -> Option<u64> {
		self.wanted.iter().next().map(|run| run.start)
	}

	/// Everything given up so far, for a drop cursor created after.
	pub fn given_up(&self) -> &RangeSet<u64> {
		&self.given_up
	}

	/// The next answered group still cached in `order`, forgetting one evicted before it was
	/// read.
	pub fn next_filled(
		&mut self,
		state: &TrackState,
		order: Order,
		given_up: &mut GivenUp,
	) -> Option<crate::group::Handle> {
		loop {
			let sequence = match order {
				Order::Ascending => self.filled.first(),
				Order::Descending => self.filled.last(),
			}
			.copied()?;
			if let Some(slot) = state.lookup.get(&sequence).filter(|slot| !slot.is_aborted()) {
				return Some(slot.serving().clone());
			}
			self.filled.remove(&sequence);
			self.release(sequence);
			self.give_up(sequence..sequence + 1, given_up);
		}
	}

	/// Take `sequence` as handed out, if a fill answered it.
	pub fn take_filled(&mut self, sequence: u64) {
		if self.filled.remove(&sequence) {
			self.release(sequence);
		}
	}

	/// Stop holding a filled `sequence` in its fill: handed out, the group's own reader
	/// keeps it wanted, or nothing does.
	fn release(&mut self, sequence: u64) {
		let mut lost = RangeSet::new();
		lost.insert(sequence..sequence + 1);
		self.joins.retain_mut(|join| {
			join.release(&lost);
			!join.is_empty()
		});
	}

	/// Forget what the reader no longer reads: below `from`, or outside `admitted`. Its fills
	/// no longer owe it those groups, and one nobody waits on is withdrawn once its last join
	/// goes.
	pub fn retain(&mut self, from: u64, admitted: &RangeInclusiveSet<u64>) {
		if self.wanted.is_empty() && self.filled.is_empty() && self.joins.is_empty() {
			return;
		}
		let outside = |sequence: u64| sequence < from || !admitted.contains(&sequence);
		self.filled.retain(|sequence| !outside(*sequence));
		let mut kept = RangeSet::new();
		for run in self.wanted.iter() {
			for inside in clip(run.clone(), from, admitted) {
				kept.insert(inside);
			}
		}
		self.wanted = kept;
		self.joins.retain_mut(|join| {
			// By the join's whole span, not just what is unanswered: a filled group not
			// taken yet is held too.
			let inside: RangeSet<u64> = clip(join.groups.clone(), from, admitted).into_iter().collect();
			join.release(&inside.gaps(&join.groups).collect());
			!join.is_empty()
		});
	}

	/// Take in what the joined fills answered, parking `waiter` on the rest. Returns the
	/// runs to ask again: refused as too old for a budget tighter than this reader's.
	pub fn resolve(
		&mut self,
		state: &kio::Consumer<TrackState>,
		max_delay: Option<Duration>,
		given_up: &mut GivenUp,
		waiter: &kio::Waiter,
	) -> RangeSet<u64> {
		let mut retry = RangeSet::new();
		let mut i = 0;
		while i < self.joins.len() {
			let join = &self.joins[i];
			let wanted: RangeSet<u64> = self
				.wanted
				.overlapping(&join.groups)
				.map(|run| run.start.max(join.groups.start)..run.end.min(join.groups.end))
				.collect();
			if wanted.is_empty() {
				// Holding only filled groups until they are taken, it has nothing to answer.
				match join.is_empty() {
					true => drop(self.joins.swap_remove(i)),
					false => i += 1,
				}
				continue;
			}
			let answers = match join.outcome().poll(waiter, |outcome| {
				let answered = outcome.within(&wanted);
				match answered.is_empty() {
					true => Poll::Pending,
					false => Poll::Ready(answered),
				}
			}) {
				Poll::Ready(Ok(answers)) => answers,
				// Ended without answering the rest: its handlers went away. Whatever the cache
				// holds by now is served from it, and the rest is given up, as for an accept.
				Poll::Ready(Err(closed)) => {
					let mut answers = closed.within(&wanted);
					let mut left = wanted.clone();
					for (groups, _) in &answers {
						left.remove(groups.clone());
					}
					answers.extend(left.iter().map(|run| (run.clone(), Ok(()))));
					answers
				}
				Poll::Pending => {
					i += 1;
					continue;
				}
			};
			// Read only after the outcome's lock is released.
			let state = state.read();
			// What is answered is no longer owed this reader, except a filled group: held until
			// taken, so its source keeps writing it while the reader picks it up.
			let mut taken = RangeSet::new();
			for (groups, answer) in answers {
				self.wanted.remove(groups.clone());
				taken.insert(groups.clone());
				match answer {
					// What the cache holds is filled, and the rest settled by run: a fill that
					// ended unanswered may span far more groups than were ever cached.
					Ok(()) => {
						self.settle(&state, groups.clone(), given_up);
						for (&sequence, slot) in state.lookup.range(groups) {
							if !slot.is_aborted() {
								self.filled.insert(sequence);
								taken.remove(sequence..sequence + 1);
							}
						}
					}
					Err((Error::Old, asked)) if looser(max_delay, asked) => retry.insert(groups),
					// A drop the track recorded is named by the drop cursor already.
					Err(_) => self.settle(&state, groups, given_up),
				}
			}
			drop(state);
			self.joins[i].release(&taken);
			// Judged again: the same join may have more to answer, or park the waiter now.
		}
		retry
	}

	/// Judge the holes in `target` the last probe has not, returning the runs to ask a
	/// fill for. Gives up a run already stale under this reader's budget.
	pub fn judge(
		&mut self,
		state: &TrackState,
		key: (u64, u64, Option<u64>),
		target: impl FnOnce() -> RangeInclusiveSet<u64>,
		drift: &Drift,
		given_up: &mut GivenUp,
		waiter: &kio::Waiter,
	) -> RangeSet<u64> {
		let mut asks = RangeSet::new();
		// The feed's start is unknown until the serving session resolves it, and readers
		// take nothing from a cache that does not reflect the feed.
		if state.start_pending || !state.readable() {
			return asks;
		}
		let Some(newest) = state.newest_produced else {
			return asks;
		};
		let at = (
			key.0,
			key.1,
			key.2,
			Some(newest),
			state.live_floor,
			state.start_sequence,
			state.drops_revision,
		);
		if self.at == Some(at) && self.deferred.is_empty() {
			return asks;
		}
		self.at = Some(at);

		let low = state.live_floor.unwrap_or(0);
		// A run's exclusive end must exist, so the last sequence is never judged.
		let top = newest.min(u64::MAX - 1);
		let mut span = RangeSet::new();
		if low <= top {
			for run in target().overlapping(&(low..=top)) {
				span.insert((*run.start()).max(low)..(*run.end()).min(top) + 1);
			}
		}
		// What left the span is forgotten, so it is judged afresh if it comes back, and a
		// verdict behind the reader's floor stops costing it memory.
		let clip = |set: &RangeSet<u64>| -> RangeSet<u64> {
			set.iter()
				.flat_map(|run| {
					span.overlapping(run)
						.map(move |s| s.start.max(run.start)..s.end.min(run.end))
				})
				.collect()
		};
		let judged = clip(&self.judged);
		self.refused = clip(&self.refused);
		self.given_up = clip(&self.given_up);
		let mut candidates: RangeSet<u64> = span.iter().flat_map(|run| judged.gaps(run)).collect();
		for run in std::mem::take(&mut self.deferred).iter() {
			for s in span.overlapping(run) {
				candidates.insert(s.start.max(run.start)..s.end.min(run.end));
			}
		}
		self.judged = judged;
		for run in candidates.iter() {
			self.judged.insert(run.clone());
		}

		// A refused group that arrived live is the feed's, and no longer refused.
		let arrived: Vec<u64> = self
			.refused
			.iter()
			.flat_map(|run| state.lookup.range(run.clone()).map(|(sequence, _)| *sequence))
			.collect();
		for sequence in arrived {
			self.refused.remove(sequence..sequence + 1);
		}

		let now = state.cache.pool().now_wall();
		let start = state.start_sequence.unwrap_or(0);
		let mut wake: Option<crate::time::Instant> = None;
		for run in candidates.iter() {
			for hole in holes(state, run.clone()) {
				for hole in self.unclaimed(state, hole) {
					let (unfed, awaited) = state.unfed(hole, now);
					for run in unfed.iter() {
						asks.insert(run.clone());
						// Refused already: given up now the feed gave up too, not asked again.
						for refused in self.refused.overlapping(run).cloned().collect::<Vec<_>>() {
							let gone = refused.start.max(run.start)..refused.end.min(run.end);
							self.refused.remove(gone.clone());
							asks.remove(gone.clone());
							self.give_up(gone, given_up);
						}
					}
					for (run, folds) in awaited {
						// Under the feed's start: asked now rather than after the grace.
						let early = run.start..run.end.min(start);
						if !early.is_empty() {
							asks.insert(early);
						}
						self.deferred.insert(run);
						wake = match (wake, folds) {
							(Some(a), Some(b)) => Some(a.min(b)),
							(a, b) => a.or(b),
						};
					}
				}
			}
		}
		for run in self.refused.iter() {
			asks.remove(run.clone());
		}
		for run in asks.iter() {
			self.deferred.remove(run.clone());
		}
		// A buffered datagram arrived, and is served as one.
		for datagram in &state.datagrams {
			asks.remove(datagram.sequence..datagram.sequence.saturating_add(1));
		}
		// A gap the feed still waits for becomes a miss when it folds.
		if let Some(at) = wake {
			state.cache.watch_wall(at, waiter);
		}

		// Starts never rewind, so a run whose newest group is stale under this reader's
		// budget is stale throughout.
		let stale: Vec<ops::Range<u64>> = asks
			.iter()
			.filter(|run| state.drifted(run.end - 1, &drift.edge, drift.budget))
			.cloned()
			.collect();
		for run in stale {
			asks.remove(run.clone());
			self.give_up(run, given_up);
		}
		asks
	}

	/// Give up what a fill still owes this reader once it is stale under the reader's budget,
	/// as the reader skips a stale cached group, parking `waiter` on each run going stale.
	/// A fill answers on its handler's time, so an ordered reader would otherwise wait out
	/// a stalled one for good.
	pub fn expire(&mut self, state: &TrackState, drift: &Drift, given_up: &mut GivenUp, waiter: &kio::Waiter) {
		let (mut stale, mut arrived) = (RangeSet::new(), RangeSet::new());
		for run in self.wanted.iter() {
			// Each stretch between the groups cached inside the run shares one successor, so
			// one check judges it whole. Groups arrive out of order, so a fresh late group
			// keeps the stretch below it owed while the ones around it go stale.
			let mut from = run.start;
			// Only the live feed's arrivals split the run: an aborted group is a hole, and a
			// backfill is a fill's answer, reaching the reader once that fill publishes it.
			let cached = state
				.lookup
				.range(run.clone())
				.filter(|(_, slot)| slot.is_walked())
				.map(|(&sequence, _)| sequence);
			for at in cached.chain(std::iter::once(run.end)) {
				if from < at && state.poll_drifted(from, &drift.edge.servable, drift.budget, None, waiter) {
					stale.insert(from..at);
				}
				// What arrived live is the feed's to serve, as the reader serves any cached group.
				if at < run.end {
					arrived.insert(at..at + 1);
				}
				from = at.saturating_add(1);
			}
		}
		if stale.is_empty() && arrived.is_empty() {
			return;
		}
		for run in stale.iter().chain(arrived.iter()) {
			self.wanted.remove(run.clone());
		}
		let mut released = stale.clone();
		for run in arrived.iter() {
			released.insert(run.clone());
		}
		self.joins.retain_mut(|join| {
			join.release(&released);
			!join.is_empty()
		});
		for run in stale.iter() {
			self.give_up(run.clone(), given_up);
		}
	}

	/// Ask the fills for `runs` in the reader's order, settling what no handler can take as
	/// refused. Fills queue in the order asked, so a descending reader's newest run comes first.
	pub fn ask(
		&mut self,
		state: &kio::Consumer<TrackState>,
		fills: &kio::Shared<FillState>,
		runs: &RangeSet<u64>,
		asking: &Asking,
		given_up: &mut GivenUp,
	) {
		let mut runs: Vec<ops::Range<u64>> = runs.iter().cloned().collect();
		if asking.order == Order::Descending {
			runs.reverse();
		}
		for run in runs {
			let ask = Ask {
				start: Position::group(run.start),
				end: run.end,
				priority: asking.priority,
				max_delay: asking.max_delay,
				order: asking.order,
			};
			let (joins, unfillable) = fills.lock().join(fills, &ask);
			self.wanted.insert(run.clone());
			for groups in unfillable {
				self.wanted.remove(groups.clone());
				self.settle(&state.read(), groups, given_up);
			}
			self.joins.extend(joins);
		}
	}

	/// The part of `hole` nobody has a verdict on yet.
	fn unclaimed(&self, state: &TrackState, hole: ops::Range<u64>) -> Vec<ops::Range<u64>> {
		let mut left = RangeSet::new();
		left.insert(hole.clone());
		// Only what overlaps the hole, so judging every run costs each one the same.
		for (run, _) in state.drops.overlapping(&hole) {
			left.remove(run.clone());
		}
		for claimed in [&self.given_up, &self.wanted] {
			for run in claimed.overlapping(&hole) {
				left.remove(run.clone());
			}
		}
		for sequence in self.filled.range(hole) {
			left.remove(*sequence..*sequence + 1);
		}
		left.iter().cloned().collect()
	}

	/// Take in a fill's refusal of `groups`: final where the feed gave up on them too,
	/// otherwise held until it does, since the live feed may still deliver them.
	fn settle(&mut self, state: &TrackState, groups: ops::Range<u64>, given_up: &mut GivenUp) {
		let now = state.cache.pool().now_wall();
		// What arrived meanwhile is the feed's to serve.
		for hole in holes(state, groups) {
			let mut left = RangeSet::new();
			left.insert(hole.clone());
			for run in state.unfed(hole, now).0.iter() {
				left.remove(run.clone());
				self.give_up(run.clone(), given_up);
			}
			for run in left.iter() {
				self.refused.insert(run.clone());
				self.deferred.insert(run.clone());
			}
		}
	}

	fn give_up(&mut self, groups: ops::Range<u64>, given_up: &mut GivenUp) {
		if groups.is_empty() {
			return;
		}
		self.given_up.insert(groups.clone());
		given_up.push(groups);
	}
}

impl TrackState {
	/// Split `hole`, which the cache holds nothing for, into what the live feed will never
	/// deliver and the gaps it still waits for, with when each stops being waited for.
	///
	/// Never: accounted for by its arrivals (evicted, or a gap it skipped), or in a gap its
	/// grace gave up on, or anything once the feed is over. A declared start is no promise:
	/// a group below it may still arrive.
	#[expect(
		clippy::type_complexity,
		reason = "the never-fed runs and the awaited gaps are returned together; naming them would only move the type"
	)]
	pub(super) fn unfed(
		&self,
		hole: ops::Range<u64>,
		now: crate::time::Instant,
	) -> (RangeSet<u64>, Vec<(ops::Range<u64>, Option<crate::time::Instant>)>) {
		let mut unfed = RangeSet::new();
		let mut awaited = Vec::new();
		// Nothing more arrives after an abort, a seal, or once the live edge reached a
		// declared end with no tail owed: until then, a group below the end may still arrive.
		if self.abort.is_some() || self.is_complete() {
			unfed.insert(hole);
			return (unfed, awaited);
		}
		let mut from = hole.start;
		let grace = self.arrival_grace();
		let mut gap =
			|groups: ops::Range<u64>, unfed: &mut RangeSet<u64>| match self.arrivals.folds_at(groups.start, grace) {
				Some(at) if at <= now => unfed.insert(groups),
				folds => awaited.push((groups, folds)),
			};
		for arrived in self.arrivals.accounted(from..hole.end) {
			if from < arrived.start {
				gap(from..arrived.start, &mut unfed);
			}
			from = arrived.end;
			unfed.insert(arrived);
		}
		if from < hole.end {
			gap(from..hole.end, &mut unfed);
		}
		(unfed, awaited)
	}
}

/// The runs of `run` the cache holds nothing servable for.
pub(super) fn holes(state: &TrackState, run: ops::Range<u64>) -> Vec<ops::Range<u64>> {
	let mut holes = Vec::new();
	let mut from = run.start;
	for (&sequence, slot) in state.lookup.range(run.clone()) {
		if slot.is_aborted() {
			continue;
		}
		if from < sequence {
			holes.push(from..sequence);
		}
		from = sequence + 1;
	}
	if from < run.end {
		holes.push(from..run.end);
	}
	holes
}

/// The parts of `run` at or above `from` inside `admitted`.
fn clip(run: ops::Range<u64>, from: u64, admitted: &RangeInclusiveSet<u64>) -> Vec<ops::Range<u64>> {
	let run = run.start.max(from)..run.end;
	if run.is_empty() {
		return Vec::new();
	}
	let inclusive: RangeInclusive<u64> = run.start..=run.end - 1;
	admitted
		.overlapping(&inclusive)
		.map(|inside| (*inside.start()).max(run.start)..(*inside.end()).min(run.end - 1) + 1)
		.collect()
}

/// Whether `ours` tolerates more than the budget a fill `asked` with, so its [`Error::Old`]
/// was not this reader's.
fn looser(ours: Option<Duration>, asked: Option<Duration>) -> bool {
	match (asked, ours) {
		(Some(asked), Some(ours)) => ours > asked,
		(Some(_), None) => true,
		(None, _) => false,
	}
}

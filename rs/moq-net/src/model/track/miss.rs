//! A reader's misses: the sequences it reads that the cache doesn't hold and the live feed
//! will never deliver, asked of the track's fills until each arrives or is given up.
//!
//! Nothing above the newest group the feed produced is a miss, since it arrives live, so a
//! fill is bounded by construction. Below it, a hole is a miss once the feed is past it:
//! under where the feed starts, or accounted for by the arrival ledger without being cached
//! (evicted, or a gap the feed skipped). A hole the ledger still waits for is deferred and
//! judged again as the feed moves.
//!
//! Each sequence is judged once, except a group evicted after the reader judged it cached
//! and before it took it: judged again as a miss, once. A refill evicted again before it is
//! taken is given up, so a slow reader's refills cannot evict each other without bound.
//!
//! A start is no promise, since a group below it may still arrive late. A hole under it is
//! asked at once, but a refusal is final only once the ledger gives up on it too; until
//! then the live feed may still deliver it. Under a received copy's start, only what the
//! reader may still want is a hole: see [`Misses::judge`].

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::{self, RangeInclusive},
	sync::{Arc, Mutex, PoisonError},
	task::Poll,
	time::Duration,
};

use rangemap::{RangeInclusiveSet, RangeSet};

use super::super::delivered::Delivered;
pub(super) use super::fill::Asking;
use super::fill::{Ask, FillState, Join};
use super::{Drift, Order, Position, TrackState};
use crate::Error;

/// What a probe judges against: the span read, the feed's newest group, where the feed
/// and readers start, and the drops revision.
type JudgedAt = (u64, u64, Option<u64>, Option<u64>, Option<u64>, Option<u64>, u64);

/// The misses of one cursor, judged as what it reads or the feed moves.
#[derive(Default)]
pub(super) struct Misses {
	/// What the last probe judged against. A probe that sees the same skips judging.
	at: Option<JudgedAt>,
	ledger_revision: u64,
	drops_revision: u64,
	budget: Option<Duration>,
	grace: Option<Duration>,
	ended: bool,
	/// The sequences judged already, within what the reader reads up to the newest group.
	judged: RangeSet<u64>,
	/// Holes the live feed may still deliver, revisited on a ledger change or deadline.
	deferred: Deferred,
	/// Asked under the feed's start and refused, never asked again: given up once the feed
	/// gives up on them, unless they arrive first.
	refused: RangeSet<u64>,
	/// The fills this reader waits on.
	joins: Vec<Join>,
	/// What the reader asked its fills with last.
	asking: Option<Asking>,
	/// The sequences of `joins` still unanswered.
	wanted: RangeSet<u64>,
	/// Answered by a fill and cached, not handed out yet.
	filled: BTreeSet<u64>,
	/// Never asked again: named dropped to the reader.
	given_up: RangeSet<u64>,
	/// Where the track leaves what it evicts that this reader judged cached, once it judged.
	inbox: Option<Inbox>,
	/// The track's eviction count this reader last emptied its inbox at.
	evictions: u64,
	/// Judged again after an eviction, within the judged span: never refilled twice.
	refilled: RangeSet<u64>,
}

/// A run of misses the reader gives up, for the drop cursor to name.
pub(super) type GivenUp = Vec<ops::Range<u64>>;

/// The current deferred runs, indexed by sequence and by their next judgement deadline.
/// Splits remove the old age entry before publishing either piece; no stale timers remain.
#[derive(Default)]
struct Deferred {
	runs: BTreeMap<u64, (u64, Option<crate::time::Instant>)>,
	ages: BTreeSet<(crate::time::Instant, u64)>,
}

impl Deferred {
	fn put(&mut self, run: ops::Range<u64>, at: Option<crate::time::Instant>) {
		if run.is_empty() {
			return;
		}
		if let Some(at) = at {
			self.ages.insert((at, run.start));
		}
		self.runs.insert(run.start, (run.end, at));
	}

	fn take(&mut self, start: u64) -> (u64, Option<crate::time::Instant>) {
		let (end, at) = self.runs.remove(&start).expect("an indexed deferred run");
		if let Some(at) = at {
			self.ages.remove(&(at, start));
		}
		(end, at)
	}

	fn remove(&mut self, run: ops::Range<u64>) {
		if run.is_empty() {
			return;
		}
		let first = self
			.runs
			.range(..=run.start)
			.next_back()
			.filter(|(_, (end, _))| *end > run.start)
			.map_or(run.start, |(start, _)| *start);
		let starts: Vec<_> = self.runs.range(first..run.end).map(|(start, _)| *start).collect();
		for start in starts {
			let (end, at) = self.take(start);
			self.put(start..end.min(run.start), at);
			self.put(start.max(run.end)..end, at);
		}
	}

	fn insert(&mut self, mut run: ops::Range<u64>, at: Option<crate::time::Instant>) {
		if run.is_empty() {
			return;
		}
		self.remove(run.clone());
		if let Some((&start, &(end, previous))) = self.runs.range(..run.start).next_back()
			&& end == run.start
			&& previous == at
		{
			self.take(start);
			run.start = start;
		}
		if let Some(&(end, next)) = self.runs.get(&run.end)
			&& next == at
		{
			self.take(run.end);
			run.end = end;
		}
		self.put(run, at);
	}

	fn overlapping(&self, run: ops::Range<u64>) -> impl Iterator<Item = ops::Range<u64>> + '_ {
		let first = self
			.runs
			.range(..=run.start)
			.next_back()
			.filter(|(_, (end, _))| *end > run.start)
			.map_or(run.start, |(start, _)| *start);
		self.runs
			.range(first..run.end)
			.map(move |(&start, &(end, _))| start.max(run.start)..end.min(run.end))
	}

	fn due(&mut self, now: crate::time::Instant) -> RangeSet<u64> {
		let mut due = RangeSet::new();
		while let Some(&(at, start)) = self.ages.first() {
			if at > now {
				break;
			}
			let (end, _) = self.take(start);
			due.insert(start..end);
		}
		due
	}

	fn deadline(&self) -> Option<crate::time::Instant> {
		self.ages.first().map(|(at, _)| *at)
	}
	fn clear(&mut self) {
		self.runs.clear();
		self.ages.clear();
	}
}

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

	/// What was refilled after an eviction and not handed out yet.
	#[cfg(all(test, feature = "async"))]
	pub fn refilled(&self) -> &RangeSet<u64> {
		&self.refilled
	}

	/// The next answered group still cached in `order`, giving up one evicted before it was
	/// read: [`Self::judge`] asked a first eviction again, so this one was refilled already.
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
			if let Some(slot) = state.lookup.get(&sequence).filter(|slot| slot.serves_end()) {
				return Some(slot.serving().clone());
			}
			self.filled.remove(&sequence);
			self.release(sequence);
			self.give_up(sequence..sequence + 1, given_up);
		}
	}

	/// Take `sequence` as handed out, if a fill answered it. A refill handed out is never
	/// judged again, so it is forgotten too.
	pub fn take_filled(&mut self, sequence: u64) {
		// The last sequence is never judged, so never refilled.
		if let Some(end) = sequence.checked_add(1) {
			self.refilled.remove(sequence..end);
		}
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

	/// Ask every fill this reader waits on with `asking` from now on, queued or taken.
	pub fn update(&mut self, asking: &Asking) {
		if self.asking.as_ref() == Some(asking) {
			return;
		}
		self.asking = Some(*asking);
		for join in &mut self.joins {
			join.update(asking);
		}
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
							if slot.serves_end() {
								self.filled.insert(sequence);
								taken.remove(sequence..sequence + 1);
							}
						}
						// Seen cached, though judging may not run again before it is evicted.
						if let Some(inbox) = &self.inbox {
							inbox.saw(state.cached);
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
	/// fill for. Gives up a run already stale under this reader's budget. `named` is the
	/// lowest group the reader's ranges name, which may reach below the feed's start, and
	/// `taken` what the reader handed out, which an eviction never makes a miss again.
	#[expect(
		clippy::too_many_arguments,
		reason = "one probe's inputs, borrowed apart from its cursor"
	)]
	pub fn judge(
		&mut self,
		state: &TrackState,
		key: (u64, u64, Option<u64>),
		named: Option<u64>,
		mut target: impl FnMut(RangeInclusive<u64>) -> RangeInclusiveSet<u64>,
		taken: &Delivered,
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
		let ended = state.abort.is_some() || state.is_complete();
		let ledger = state.arrivals.changes();
		let drops = &state.drops_changes;
		// Every reader probes on every poll, mostly with nothing changed since the last, so
		// that answer costs neither the subscriptions' lock nor a clock read.
		let unchanged = self.at == Some(at)
			&& self.budget == Some(drift.budget)
			&& self.ended == ended
			&& self.ledger_revision == ledger.revision()
			&& self.drops_revision == drops.revision()
			&& self.evictions == state.evictions;
		// The grace only times deferred gaps, so with none a new one has nothing to move.
		// It is judged once something else changes.
		if unchanged && self.deferred.runs.is_empty() {
			return asks;
		}
		let grace = state.arrival_grace();
		let unchanged = unchanged && self.grace == Some(grace);
		let deadline = self.deferred.deadline();
		if unchanged && deadline.is_none() {
			return asks;
		}
		let now = state.cache.pool().now_wall();
		if unchanged && let Some(at) = deadline.filter(|at| *at > now) {
			state.cache.watch_wall(at, waiter);
			return asks;
		}
		let terms = self.at.is_none_or(|previous| {
			previous.1 != at.1 || previous.2 != at.2 || previous.4 != at.4 || previous.5 != at.5 || at.0 < previous.0
		}) || self.budget != Some(drift.budget)
			|| self.grace != Some(grace)
			|| self.ended != ended;
		let low = state.live_floor.unwrap_or(0).max(key.0);
		let top = newest.min(u64::MAX - 1);
		let mut candidates = RangeSet::new();
		if terms {
			let mut span = RangeSet::new();
			if low <= top {
				for run in target(low..=top).iter() {
					span.insert(*run.start()..*run.end() + 1);
				}
			}
			let clip = |set: &RangeSet<u64>| -> RangeSet<u64> {
				set.iter()
					.flat_map(|run| {
						span.overlapping(run)
							.map(move |s| s.start.max(run.start)..s.end.min(run.end))
					})
					.collect()
			};
			self.judged = clip(&self.judged);
			self.refused = clip(&self.refused);
			self.given_up = clip(&self.given_up);
			self.refilled = clip(&self.refilled);
			for (&start, &(end, _)) in &self.deferred.runs {
				for run in span.overlapping(&(start..end)) {
					candidates.insert(start.max(run.start)..end.min(run.end));
				}
			}
			self.deferred.clear();
			for run in span.iter() {
				for fresh in self.judged.gaps(run) {
					candidates.insert(fresh);
				}
			}
		} else {
			// A moving read floor removes only its retired prefix. New production extends the
			// judged span; neither path visits older deferred gaps.
			if low > 0 {
				for set in [
					&mut self.judged,
					&mut self.refused,
					&mut self.given_up,
					&mut self.refilled,
				] {
					set.remove(0..low);
				}
				self.deferred.remove(0..low);
			}
			let first = self
				.at
				.and_then(|previous| previous.3)
				.map_or(low, |previous| previous.saturating_add(1).max(low));
			if first <= top {
				for run in target(first..=top).iter() {
					let run = *run.start()..*run.end() + 1;
					for fresh in self.judged.gaps(&run) {
						candidates.insert(fresh);
					}
				}
			}
			for run in self.deferred.due(now).iter() {
				candidates.insert(run.clone());
			}
		}
		// Current ledger runs are indexed by their last mutation. Coalescing retires the
		// old entries, so a reader that was idle needs no shared mutation history.
		for changed in ledger
			.since(self.ledger_revision)
			.chain(drops.since(self.drops_revision))
		{
			for run in self.deferred.overlapping(changed.clone()) {
				candidates.insert(run);
			}
			let arrived: Vec<_> = self
				.refused
				.overlapping(&changed)
				.flat_map(|run| {
					state
						.lookup
						.range(run.start.max(changed.start)..run.end.min(changed.end))
						// An aborted or back-cut slot is still a hole, so its refusal stands.
						.filter(|(_, slot)| slot.serves_end())
						.map(|(&sequence, _)| sequence)
				})
				.collect();
			for sequence in arrived {
				self.refused.remove(sequence..sequence + 1);
			}
		}
		// What it judged cached and the track evicted since, before this reader took it, is a
		// miss now.
		let inbox = self.inbox.get_or_insert_with(|| {
			let inbox = Inbox::default();
			state
				.inboxes
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.register(inbox.clone());
			inbox
		});
		let evicted = inbox.judge(state.cached, low, top);
		self.evictions = state.evictions;
		let mut refills = RangeSet::new();
		let mut again = RangeSet::new();
		for run in evicted.iter() {
			let run = run.start.max(low)..run.end.min(top.saturating_add(1));
			if run.is_empty() {
				continue;
			}
			for part in target(run.start..=run.end - 1).iter() {
				for gap in taken.gaps(*part.start()..*part.end() + 1) {
					for judged in self.judged.overlapping(&gap) {
						let judged = judged.start.max(gap.start)..judged.end.min(gap.end);
						for fresh in self.refilled.gaps(&judged) {
							refills.insert(fresh);
						}
						for refilled in self.refilled.overlapping(&judged) {
							again.insert(refilled.start.max(judged.start)..refilled.end.min(judged.end));
						}
					}
				}
			}
		}
		// Settled here rather than when read, since an ordered reader never looks at what a
		// fill answered: it reads past a group gone from the cache. Still asked, its fill
		// settles it; given up already, it was named.
		for run in self.wanted.iter().chain(self.given_up.iter()) {
			again.remove(run.clone());
		}
		for (run, refill) in refills
			.iter()
			.map(|run| (run, true))
			.chain(again.iter().map(|run| (run, false)))
		{
			// A fill's answer evicted before it was taken is no longer held for this reader.
			let filled: Vec<u64> = self.filled.range(run.clone()).copied().collect();
			for sequence in filled {
				self.filled.remove(&sequence);
				self.release(sequence);
			}
			match refill {
				true => {
					self.refilled.insert(run.clone());
					candidates.insert(run.clone());
				}
				false => self.give_up(run.clone(), given_up),
			}
		}
		self.ledger_revision = ledger.revision();
		self.drops_revision = drops.revision();
		self.budget = Some(drift.budget);
		self.grace = Some(grace);
		self.ended = ended;
		self.at = Some(at);
		for run in candidates.iter() {
			self.deferred.remove(run.clone());
			self.judged.insert(run.clone());
		}
		let start = state.start_sequence.unwrap_or(0);
		let (stale_below, unasked) = match state.start_budget {
			// Its publisher skipped every older group as stale under a budget no tighter
			// than this reader's, so they are stale for it too.
			Some(budget) if drift.budget <= budget => (start, false),
			// Such a copy cannot tell a group a looser reader's budget reaches, so it leaves
			// them to the feed's re-offer.
			Some(_) => (0, state.fills_from_start),
			// The feed began at the live edge or the floor the copy asked for, so older
			// groups are history only a reader naming them asked for.
			None => (
				0,
				state.fills_from_start || state.received && named.is_none_or(|named| named >= start),
			),
		};
		if unasked && start > 0 {
			candidates.remove(0..start);
		}
		for run in candidates.iter() {
			// Backfill another reader's fill cached is no hole, yet the arrival walk never
			// reaches it: this reader takes it as filled where it would ask for a hole.
			for held in backfill(state, run.clone()).iter() {
				let (unfed, awaited) = state.unfed(held.clone(), now);
				for run in unfed.iter() {
					self.filled.extend(run.clone());
				}
				for (run, folds) in awaited {
					self.filled.extend(run.start..run.end.min(start));
					self.deferred.insert(run.start.max(start)..run.end, folds);
				}
			}
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
						self.deferred.insert(run, folds);
					}
				}
			}
		}
		let refused: Vec<_> = asks
			.iter()
			.flat_map(|ask| self.refused.overlapping(ask).cloned())
			.collect();
		for run in refused {
			asks.remove(run);
		}
		for run in asks.iter() {
			self.deferred.remove(run.clone());
		}
		// A buffered datagram arrived, and is served as one.
		for datagram in &state.datagrams {
			asks.remove(datagram.sequence..datagram.sequence.saturating_add(1));
		}
		// A gap the feed still waits for becomes a miss when it folds.
		if let Some(at) = self.deferred.deadline() {
			state.cache.watch_wall(at, waiter);
		}

		// Starts never rewind, so a run whose newest group is stale under this reader's
		// budget is stale throughout.
		let stale: Vec<ops::Range<u64>> = asks
			.iter()
			.filter_map(|run| match state.drifted(run.end - 1, &drift.edge, drift.budget) {
				true => Some(run.clone()),
				false => Some(run.start..run.end.min(stale_below)).filter(|run| !run.is_empty()),
			})
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
		self.update(asking);
		let mut runs: Vec<ops::Range<u64>> = runs.iter().cloned().collect();
		if asking.order == Order::Descending {
			runs.reverse();
		}
		for run in runs {
			let ask = Ask {
				start: Position::group(run.start),
				end: run.end,
				frame_end: None,
				asking: *asking,
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
				self.deferred
					.insert(run.clone(), state.arrivals.folds_at(run.start, state.arrival_grace()));
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

/// What the track evicted that one reader judged cached, and what it judged: a slot cached
/// by the count it judged at, within the span it judged. Holds no more than the slots it
/// judged cached, so a stalled reader pins nothing cached after.
#[derive(Clone, Default)]
pub(super) struct Inbox(Arc<Mutex<Judging>>);

#[derive(Default)]
struct Judging {
	cached: u64,
	span: Option<RangeInclusive<u64>>,
	evicted: RangeSet<u64>,
}

impl Inbox {
	/// Take what was evicted since the last call, and judge `low..=top` from here on, with
	/// every slot cached by `cached`.
	fn judge(&self, cached: u64, low: u64, top: u64) -> RangeSet<u64> {
		let mut judging = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		judging.cached = cached;
		judging.span = Some(low..=top);
		std::mem::take(&mut judging.evicted)
	}

	/// Count every slot cached by `cached` as judged cached, within the span judged last.
	fn saw(&self, cached: u64) {
		let mut judging = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		judging.cached = judging.cached.max(cached);
	}
}

/// Every reader's [`Inbox`], pruned of dropped readers on each eviction, or once
/// registrations double it past what the last prune kept.
#[derive(Default)]
pub(super) struct Inboxes {
	inboxes: Vec<Inbox>,
	kept: usize,
}

impl Inboxes {
	fn register(&mut self, inbox: Inbox) {
		self.inboxes.push(inbox);
		if self.inboxes.len() > 2 * self.kept.max(1) {
			self.inboxes.retain(|inbox| Arc::strong_count(&inbox.0) > 1);
			self.kept = self.inboxes.len();
		}
	}

	/// How many inboxes are registered, live or not.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn len(&self) -> usize {
		self.inboxes.len()
	}

	/// What each live reader's inbox holds, in registration order.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn held(&self) -> Vec<Vec<(u64, u64)>> {
		self.inboxes
			.iter()
			.filter(|inbox| Arc::strong_count(&inbox.0) > 1)
			.map(|inbox| {
				let judging = inbox.0.lock().unwrap_or_else(PoisonError::into_inner);
				judging.evicted.iter().map(|run| (run.start, run.end)).collect()
			})
			.collect()
	}

	/// Leave `sequence`, cached as count `cached`, with each reader that judged it cached.
	/// Returns whether any did.
	pub(super) fn evicted(&mut self, sequence: u64, cached: u64) -> bool {
		let Some(end) = sequence.checked_add(1) else {
			return false;
		};
		let mut landed = false;
		// Every inbox is visited anyway, so a dropped reader's goes here too.
		self.inboxes.retain(|inbox| {
			if Arc::strong_count(&inbox.0) == 1 {
				return false;
			}
			let mut judging = inbox.0.lock().unwrap_or_else(PoisonError::into_inner);
			if cached <= judging.cached && judging.span.as_ref().is_some_and(|span| span.contains(&sequence)) {
				judging.evicted.insert(sequence..end);
				landed = true;
			}
			true
		});
		self.kept = self.inboxes.len();
		landed
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

/// The runs of `run` the cache holds as backfill: servable, but out of the arrival walk.
fn backfill(state: &TrackState, run: ops::Range<u64>) -> RangeSet<u64> {
	state
		.lookup
		.range(run)
		.filter(|(_, slot)| !slot.is_walked() && slot.serves_end())
		.map(|(&sequence, _)| sequence..sequence + 1)
		.collect()
}

/// The runs of `run` the cache holds nothing servable for.
pub(super) fn holes(state: &TrackState, run: ops::Range<u64>) -> Vec<ops::Range<u64>> {
	let mut holes = Vec::new();
	let mut from = run.start;
	for (&sequence, slot) in state.lookup.range(run.clone()) {
		if !slot.serves_end() {
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

#[cfg(test)]
mod tests {
	use super::super::{Servable, Subscription};
	use super::*;

	#[moq_clock::test]
	async fn a_recreated_accounted_group_clears_its_earlier_fill_refusal() {
		let broadcast = crate::broadcast::Info::default().produce();
		let mut track = broadcast
			.create_track("recreated", crate::track::Info::default())
			.unwrap();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_max_delay(budget));
		for sequence in [0, 4] {
			track
				.create_group(crate::group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		track.start_at(3).unwrap();
		let mut misses = Misses::default();
		let mut given_up = Vec::new();
		let waiter = kio::Waiter::noop();
		let state = track.state.read();
		let drift = Drift {
			budget,
			bounded: false,
			edge: state.drift_edge(&Servable::ALL),
		};
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
			|span| [span].into_iter().collect(),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		let fills = state.fills.clone();
		drop(state);
		misses.ask(
			&track.state.consume(),
			&fills,
			&asks,
			&Asking {
				priority: 0,
				max_delay: Some(budget),
				order: Order::Ascending,
			},
			&mut given_up,
		);
		assert!(
			misses.refused.contains(&2),
			"a missing fill is refused while the live gap stays open"
		);

		// An arrival and cut happen while the cursor is idle. The drop suppresses asking
		// again, and reclaiming the cut removes the cached incarnation before the probe.
		track
			.create_group(crate::group::Info { sequence: 2 })
			.unwrap()
			.abort(Error::Cancel)
			.unwrap();
		track.drop_groups(2..3, crate::StreamError::Old);
		track.modify().unwrap().uncache(2);
		let state = track.state.read();
		assert!(state.drops.contains_key(&2));
		assert!(!state.lookup.contains_key(&2));
		misses.judge(
			&state,
			(0, 0, None),
			None,
			|_| panic!("the edge has not moved"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(misses.refused.contains(&2));
		let revision = state.arrivals.changes().revision();
		drop(state);
		track
			.create_group(crate::group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		let state = track.state.read();
		assert!(state.drops.is_empty(), "recreation removed the last drop run");
		assert!(state.arrivals.covers(2..3));
		assert!(
			state.arrivals.changes().revision() > revision,
			"the real incarnation invalidates its current run"
		);
		misses.judge(
			&state,
			(0, 0, None),
			None,
			|_| panic!("recreation extends no span"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(
			!misses.refused.contains(&2),
			"the live cached incarnation is no longer refused"
		);
		assert!(given_up.is_empty(), "a live recreation is never given up");
	}

	#[moq_clock::test]
	async fn an_aborted_live_group_keeps_its_earlier_fill_refusal() {
		let broadcast = crate::broadcast::Info::default().produce();
		let mut track = broadcast
			.create_track("aborted", crate::track::Info::default())
			.unwrap();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_max_delay(budget));
		for sequence in [0, 4] {
			track
				.create_group(crate::group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		track.start_at(3).unwrap();
		let mut misses = Misses::default();
		let mut given_up = Vec::new();
		let waiter = kio::Waiter::noop();
		let state = track.state.read();
		let drift = Drift {
			budget,
			bounded: false,
			edge: state.drift_edge(&Servable::ALL),
		};
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
			|span| [span].into_iter().collect(),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		let fills = state.fills.clone();
		drop(state);
		misses.ask(
			&track.state.consume(),
			&fills,
			&asks,
			&Asking {
				priority: 0,
				max_delay: Some(budget),
				order: Order::Ascending,
			},
			&mut given_up,
		);
		assert!(misses.refused.contains(&2));

		// The live feed accounts for group 2 but aborts it without a cut, so its slot is
		// still a hole: the refusal stands and the hole is given up, not asked again.
		track
			.create_group(crate::group::Info { sequence: 2 })
			.unwrap()
			.abort(Error::Cancel)
			.unwrap();
		let state = track.state.read();
		assert!(state.lookup.get(&2).is_some_and(|slot| slot.is_aborted()));
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
			|_| panic!("the edge has not moved"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(!asks.contains(&2), "a refused hole is not asked again");
		assert_eq!(given_up, vec![2..3]);
	}
	#[moq_clock::test]
	async fn unchanged_probes_and_late_arrivals_keep_other_gap_deadlines() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast.create_track("sparse", crate::track::Info::default()).unwrap();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_max_delay(budget));
		for sequence in [0, 2, 4] {
			track
				.create_group(crate::group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		let mut misses = Misses::default();
		let mut given_up = Vec::new();
		let waiter = kio::Waiter::noop();
		let state = track.state.read();
		let drift = Drift {
			budget,
			bounded: false,
			edge: state.drift_edge(&Servable::ALL),
		};
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					|span| [span].into_iter().collect(),
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		assert_eq!(misses.deferred.overlapping(0..5).collect::<Vec<_>>(), vec![1..2, 3..4]);
		let deadline = misses.deferred.deadline();
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					|_| panic!("an unchanged probe rebuilds no span"),
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		drop(state);
		track
			.create_group(crate::group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		let state = track.state.read();
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					|_| panic!("a late arrival extends no span"),
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		assert_eq!(misses.deferred.overlapping(0..5).collect::<Vec<_>>(), vec![3..4]);
		assert_eq!(misses.deferred.deadline(), deadline);
		drop(state);
		broadcast.info().pool.step(budget);
		let state = track.state.read();
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
			|_| panic!("a due gap extends no span"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(asks.is_empty(), "the successor has aged past this reader's budget too");
		assert_eq!(given_up, vec![3..4]);
		assert!(misses.deferred.runs.is_empty());
	}

	/// A grace change with nothing deferred has no deadline to move, so an unchanged probe
	/// skips it; the next gap is still timed by the new grace.
	#[moq_clock::test]
	async fn a_grace_change_with_nothing_deferred_times_the_next_gap() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast.create_track("grace", crate::track::Info::default()).unwrap();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_max_delay(budget));
		for sequence in 0..3 {
			track
				.create_group(crate::group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		let mut misses = Misses::default();
		let mut given_up = Vec::new();
		let waiter = kio::Waiter::noop();
		let state = track.state.read();
		let drift = Drift {
			budget,
			bounded: false,
			edge: state.drift_edge(&Servable::ALL),
		};
		let full = |span| [span].into_iter().collect();
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					full,
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		assert!(misses.deferred.runs.is_empty(), "a contiguous feed defers nothing");
		drop(state);

		let wider = Duration::from_secs(20);
		let _wide = track.subscribe(Subscription::default().with_max_delay(wider));
		let state = track.state.read();
		assert_eq!(state.arrival_grace(), wider);
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					|_| panic!("nothing deferred: the new grace waits for a change"),
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		drop(state);

		track
			.create_group(crate::group::Info { sequence: 4 })
			.unwrap()
			.finish()
			.unwrap();
		let state = track.state.read();
		let opened = state.cache.pool().now_wall();
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
					full,
					&Delivered::default(),
					&drift,
					&mut given_up,
					&waiter
				)
				.is_empty()
		);
		assert_eq!(misses.deferred.overlapping(0..5).collect::<Vec<_>>(), vec![3..4]);
		assert_eq!(
			misses.deferred.deadline(),
			Some(opened + wider),
			"timed by the new grace"
		);
	}

	#[test]
	fn deferred_splits_and_replacements_retire_the_old_age_entries() {
		let mut deferred = Deferred::default();
		let early = crate::time::Instant::ZERO + Duration::from_secs(1);
		let late = early + Duration::from_secs(1);
		deferred.insert(0..u64::MAX, Some(early));
		deferred.insert(10..20, Some(late));
		deferred.remove(5..25);
		assert_eq!(deferred.ages.len(), 2);
		assert_eq!(
			deferred.due(early).iter().cloned().collect::<Vec<_>>(),
			vec![0..5, 25..u64::MAX]
		);
		assert!(deferred.due(late).is_empty());
		assert!(deferred.runs.is_empty());
	}
}

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
	task::{Poll, Wake, Waker},
	time::Duration,
};

use rangemap::{RangeInclusiveSet, RangeMap, RangeSet};

use super::super::delivered::Delivered;
use super::super::subscription::Layered;
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
	/// The fills this reader waits on, under the keys `holders` names them by.
	joins: BTreeMap<u64, Join>,
	/// The join holding each sequence: a reader's joins hold disjoint groups, so a change
	/// touches only the joins holding what it changes, never every fill the reader asked.
	holders: RangeMap<u64, u64>,
	/// What each join parks on, under its key: a waiter of its own, which records that its
	/// fill answered before waking the reader, so [`Self::resolve`] looks only at the fills
	/// that answered, never at every fill still out.
	parked: BTreeMap<u64, kio::Waiter>,
	/// The joins whose fills answered since the reader last looked.
	answered: Arc<Answered>,
	/// The key the next join takes.
	next_join: u64,
	/// What the reader asked its fills with last.
	asking: Option<Asking>,
	/// The sequences of `joins` still unanswered.
	wanted: RangeSet<u64>,
	/// Answered by a fill and cached, not handed out yet.
	filled: BTreeSet<u64>,
	/// Never asked again: named dropped to the reader.
	given_up: RangeSet<u64>,
	/// The window revision and cap what this holds was last clipped to, with the window's
	/// missable groups then, `None` while every group was.
	clipped: Option<(u64, Option<u64>, Option<Layered>)>,
	/// The window's missable groups the last probe judged against, `None` while every group
	/// was, so a move of only its time ends' runs judges only what they moved over.
	view: Option<Layered>,
	/// Where the track leaves what it evicts that this reader judged cached, once it judged.
	inbox: Option<Inbox>,
	/// The track's eviction count this reader last emptied its inbox at.
	evictions: u64,
	/// Judged again after an eviction, within the judged span: never refilled twice.
	refilled: RangeSet<u64>,
}

/// A run of misses the reader gives up, for the drop cursor to name.
pub(super) type GivenUp = Vec<ops::Range<u64>>;

/// The joins whose fills changed their answer, recorded by each join's own waiter before it
/// wakes the reader's task.
///
/// The task is the one the reader last resolved for. A reader polled by another task since
/// still learns of every answer, since the joins' waiters stay registered and wake whichever
/// task the reader resolved for last.
#[derive(Default)]
struct Answered {
	keys: Mutex<BTreeSet<u64>>,
	task: Mutex<Option<Waker>>,
}

/// Wakes the reader once join `key`'s fill answers, recording which join it was.
struct JoinWake {
	key: u64,
	answered: Arc<Answered>,
}

impl Wake for JoinWake {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// Recorded before the task wakes, so the resolve it triggers sees the change.
		let answered = &self.answered;
		answered
			.keys
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(self.key);
		// Woken with no lock held: an executor may poll inline.
		let task = answered.task.lock().unwrap_or_else(PoisonError::into_inner).clone();
		if let Some(task) = task {
			task.wake();
		}
	}
}

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

	/// The highest sequence still to come from a fill.
	fn highest(&self) -> Option<u64> {
		let wanted = self.wanted.iter().next_back().map(|run| run.end - 1);
		wanted.max(self.filled.last().copied())
	}

	/// Forget `lost`, which the reader no longer reads: its fills no longer owe it those
	/// groups.
	fn lose(&mut self, lost: &RangeSet<u64>) {
		for run in lost.iter() {
			let filled: Vec<u64> = self.filled.range(run.clone()).copied().collect();
			for sequence in filled {
				self.filled.remove(&sequence);
			}
			self.wanted.remove(run.clone());
		}
		self.release(lost);
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
			let mut lost = RangeSet::new();
			lost.insert(sequence..sequence + 1);
			self.release(&lost);
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
			let mut lost = RangeSet::new();
			lost.insert(sequence..sequence + 1);
			self.release(&lost);
		}
	}

	/// Hold `joins` until each fill answers and the reader takes what it filled.
	fn hold(&mut self, joins: Vec<Join>) {
		for join in joins {
			debug_assert!(
				self.holders.overlapping(&join.groups).next().is_none(),
				"a reader asks only for what no join holds"
			);
			let key = self.next_join;
			self.next_join += 1;
			self.holders.insert(join.groups.clone(), key);
			self.joins.insert(key, join);
			let wake = Arc::new(JoinWake {
				key,
				answered: self.answered.clone(),
			});
			self.parked.insert(key, kio::Waiter::new(Waker::from(wake)));
			// Looked at once, which parks it.
			self.answered
				.keys
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.insert(key);
		}
	}

	/// Stop holding `lost` in the joins holding any of it, so their fills no longer owe it
	/// this reader, and drop each join left holding nothing.
	fn release(&mut self, lost: &RangeSet<u64>) {
		for run in lost.iter() {
			while let Some((held, &key)) = self.holders.overlapping(run).next() {
				// Only this piece: a join split by an earlier release has other pieces in
				// `lost`, and each must still find the join when its turn comes.
				let piece = held.start.max(run.start)..held.end.min(run.end);
				self.holders.remove(piece.clone());
				let join = self.joins.get_mut(&key).expect("a holder names a join");
				join.release(&RangeSet::from_iter([piece]));
				if join.is_empty() {
					self.joins.remove(&key);
					self.parked.remove(&key);
				}
			}
		}
	}

	/// Ask every fill this reader waits on with `asking` from now on, queued or taken.
	pub fn update(&mut self, asking: &Asking) {
		if self.asking.as_ref() == Some(asking) {
			return;
		}
		self.asking = Some(*asking);
		for join in self.joins.values_mut() {
			join.update(asking);
		}
	}

	/// Forget what the reader no longer reads: below `from`, or outside `admitted`, its window
	/// at `revision` below the exclusive `cap`, whose missable groups are `view`. Its fills no
	/// longer owe it those groups, and one nobody waits on is withdrawn once its last join goes.
	///
	/// Clips to the window only once it moves, since everything taken in since lies inside it,
	/// so a poll under the same window costs only what falls below a raised floor. A move of
	/// only the window's time ends costs what their runs no longer cover where this holds
	/// anything, never every run of the window.
	pub fn retain(
		&mut self,
		(from, revision, cap): (u64, u64, Option<u64>),
		view: Option<&Layered>,
		admitted: impl FnOnce() -> RangeInclusiveSet<u64>,
	) {
		let clipped = self.clipped.replace((revision, cap, view.cloned()));
		// A join holds only what is still wanted or filled.
		let Some(lowest) = self.lowest() else {
			return;
		};
		let moved = match &clipped {
			Some((clipped, at, _)) if (*clipped, *at) == (revision, cap) => Some(Vec::new()),
			Some((_, at, Some(old))) if *at == cap => {
				let highest = self.highest().unwrap_or(lowest);
				view.and_then(|view| old.gained(view, from, highest))
			}
			_ => None,
		};
		if let Some(lost) = moved {
			if lowest < from {
				self.filled = self.filled.split_off(&from);
				self.wanted.remove(0..from);
				let mut below = RangeSet::new();
				below.insert(0..from);
				self.release(&below);
			}
			let lost: RangeSet<u64> = lost
				.into_iter()
				.map(|run| *run.start()..run.end().saturating_add(1))
				.collect();
			if !lost.is_empty() {
				self.lose(&lost);
			}
			return;
		}
		let admitted = &admitted();
		let outside = |sequence: u64| sequence < from || !admitted.contains(&sequence);
		self.filled.retain(|sequence| !outside(*sequence));
		let mut kept = RangeSet::new();
		for run in self.wanted.iter() {
			for inside in clip(run.clone(), from, admitted) {
				kept.insert(inside);
			}
		}
		self.wanted = kept;
		// By everything held, not just what is unanswered: a filled group not taken yet is
		// held too.
		let mut lost = RangeSet::new();
		for (held, _) in self.holders.iter() {
			let inside: RangeSet<u64> = clip(held.clone(), from, admitted).into_iter().collect();
			lost.extend(inside.gaps(held));
		}
		self.release(&lost);
	}

	/// Take in what the joined fills answered, waking `waiter` once the rest do. Returns the
	/// runs to ask again: refused as too old for a budget tighter than this reader's.
	///
	/// Looks only at the joins whose fills answered since the last call, and the joins new
	/// since, so a reader waiting on thousands of fills costs the ones that answered.
	pub fn resolve(
		&mut self,
		state: &kio::Consumer<TrackState>,
		max_delay: Option<Duration>,
		given_up: &mut GivenUp,
		waiter: &kio::Waiter,
	) -> RangeSet<u64> {
		let mut retry = RangeSet::new();
		{
			let mut task = self.answered.task.lock().unwrap_or_else(PoisonError::into_inner);
			if !task.as_ref().is_some_and(|task| task.will_wake(waiter.waker())) {
				*task = Some(waiter.waker().clone());
			}
		}
		// Taken after the task is set: an answer landing since wakes the task set above.
		let keys = std::mem::take(&mut *self.answered.keys.lock().unwrap_or_else(PoisonError::into_inner));
		let mut keys = keys.into_iter().peekable();
		while let Some(&key) = keys.peek() {
			let (Some(join), Some(parked)) = (self.joins.get(&key), self.parked.get(&key)) else {
				keys.next();
				continue;
			};
			let wanted: RangeSet<u64> = join
				.held()
				.iter()
				.flat_map(|held| {
					self.wanted
						.overlapping(held)
						.map(move |run| run.start.max(held.start)..run.end.min(held.end))
				})
				.collect();
			if wanted.is_empty() {
				keys.next();
				continue;
			}
			// Only what is unanswered: a join holding only filled groups until they are taken
			// has nothing to answer.
			let answers = match join.outcome().poll(parked, |outcome| {
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
					keys.next();
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
			self.release(&taken);
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
		view: Option<&Layered>,
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
		// A window that moved only its time ends' runs keeps its fixed runs, so only what those
		// runs moved over is judged again below.
		let layered = match (&self.view, view) {
			(Some(old), Some(view)) if old.shares_fixed(view) => Some((old.clone(), view)),
			_ => None,
		};
		let terms = self.at.is_none_or(|previous| {
			(previous.1 != at.1 && layered.is_none())
				|| previous.2 != at.2
				|| previous.4 != at.4
				|| previous.5 != at.5
				|| at.0 < previous.0
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
			// What the time ends' runs no longer cover is forgotten, and what they newly cover
			// judged, within what the probe judges: from the floor up to the newest group,
			// below the cap.
			let last = key
				.2
				.map_or(Some(top), |cap| cap.checked_sub(1).map(|cap| cap.min(top)));
			if let Some((old, view)) = &layered
				&& let Some(last) = last
				&& self.at.is_some_and(|previous| previous.1 != at.1)
			{
				let half = |run: RangeInclusive<u64>| *run.start()..run.end().saturating_add(1);
				for run in old.gained(view, low, last).into_iter().flatten().map(half) {
					for set in [
						&mut self.judged,
						&mut self.refused,
						&mut self.given_up,
						&mut self.refilled,
					] {
						set.remove(run.clone());
					}
					self.deferred.remove(run);
				}
				for run in view.gained(old, low, last).into_iter().flatten().map(half) {
					for fresh in self.judged.gaps(&run) {
						candidates.insert(fresh);
					}
				}
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
		// Only what overlaps it, so a reader still asking thousands of fills pays nothing here.
		let settled: Vec<ops::Range<u64>> = again
			.iter()
			.flat_map(|run| {
				let wanted = self.wanted.overlapping(run);
				wanted
					.chain(self.given_up.overlapping(run))
					.map(move |settled| settled.start.max(run.start)..settled.end.min(run.end))
			})
			.collect();
		for run in settled {
			again.remove(run);
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
				self.release(&RangeSet::from_iter(std::iter::once(sequence..sequence + 1)));
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
		self.view = view.cloned();
		for run in candidates.iter() {
			self.deferred.remove(run.clone());
			self.judged.insert(run.clone());
		}
		let start = state.start_sequence.unwrap_or(0);
		// Below a start its publisher resolved from a budget, which an update may have changed
		// in flight, every older group is history a fill may still find. A feed that began at
		// the live edge or the floor the copy asked for leaves older groups as history only a
		// reader naming them asked for.
		let unasked = state.fills_from_start
			|| !state.start_resolved && state.received && named.is_none_or(|named| named >= start);
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
	///
	/// Each stretch of a wanted run between the groups the live feed cached inside it is
	/// judged whole, from its start. Every stretch starting below the first group the live
	/// feed has after a stretch shares its successors, so one check judges all of them:
	/// thousands of runs a fill still owes below the live edge cost one, not one each.
	pub fn expire(&mut self, state: &TrackState, drift: &Drift, given_up: &mut GivenUp, waiter: &kio::Waiter) {
		let (stale, arrived) = self.expired(state, drift, waiter);
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
		self.release(&released);
		for run in stale.iter() {
			self.give_up(run.clone(), given_up);
		}
	}

	/// What [`Self::expire`] gives up as stale, and what arrived live, parking `waiter`.
	fn expired(&self, state: &TrackState, drift: &Drift, waiter: &kio::Waiter) -> (RangeSet<u64>, RangeSet<u64>) {
		let (mut stale, mut arrived) = (RangeSet::new(), RangeSet::new());
		let servable = &drift.edge.servable;
		let mut next = self.wanted.iter().next().map(|run| run.start);
		while let Some(from) = next {
			let bound = state.arrival(from, servable).map(|slot| slot.group.sequence);
			let drifted = state.poll_drifted(from, servable, drift.budget, None, waiter);
			next = self.stretches(state, from, bound, drifted, &mut stale, &mut arrived);
		}
		(stale, arrived)
	}

	/// Visit the stretches of the wanted runs starting at `from` and below `bound`, all judged
	/// `drifted`, putting them in `stale` if so, and the groups the live feed cached inside a
	/// run in `arrived`. Returns where the next stretch starts, at or past `bound`.
	///
	/// Groups arrive out of order, so a fresh late group keeps the stretch below it owed while
	/// the ones around it go stale. Only the live feed's arrivals split a run: an aborted
	/// group is a hole, and a backfill is a fill's answer, reaching the reader once that fill
	/// publishes it. A fresh stretch with no such group before the next live one is passed
	/// in one step, however many runs lie between.
	fn stretches(
		&self,
		state: &TrackState,
		from: u64,
		bound: Option<u64>,
		drifted: bool,
		stale: &mut RangeSet<u64>,
		arrived: &mut RangeSet<u64>,
	) -> Option<u64> {
		let walked = |lo: u64, hi: u64| {
			state
				.visible
				.range(lo..hi)
				.copied()
				.find(|sequence| state.lookup.get(sequence).is_some_and(|slot| slot.serves_end()))
		};
		// Where the visit is, and whether a stretch started below it is still running there.
		let (mut at, mut within) = (from, false);
		loop {
			let run = self.wanted.overlapping(&(at..u64::MAX)).next()?.clone();
			if run.start > at {
				(at, within) = (run.start, false);
			}
			if !within && bound.is_some_and(|bound| at >= bound) {
				return Some(at);
			}
			match walked(at, run.end) {
				// What arrived live is the feed's to serve, as the reader serves any cached group.
				Some(group) => {
					if drifted && at < group {
						stale.insert(at..group);
					}
					arrived.insert(group..group + 1);
					(at, within) = (group.checked_add(1)?, false);
				}
				None if drifted => {
					stale.insert(at..run.end);
					(at, within) = (run.end, false);
				}
				// The stretch began below the bound and ran past it: the next starts past it too.
				None if bound.is_some_and(|bound| bound <= run.end) => (at, within) = (run.end, false),
				None => {
					// Fresh up to the next group the live feed holds, or the bound: every stretch
					// starting below either is fresh too, holding no arrival.
					let next = state.visible.range(run.end..).next().copied();
					let leap = match (next, bound) {
						(Some(next), Some(bound)) => next.min(bound),
						(next, bound) => next.or(bound)?,
					};
					// A run holding the leap from below it is a stretch already running there.
					within = self.wanted.get(&leap).is_some_and(|held| held.start < leap);
					at = leap;
				}
			}
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
			self.hold(joins);
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

	#[kio::test]
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

	#[kio::test]
	async fn releasing_both_pieces_of_a_split_join_drops_it_once() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast.create_track("split", crate::track::Info::default()).unwrap();
		let fills = track.state.read().fills.clone();
		fills.lock().add_handler();
		let mut misses = Misses::default();
		let mut given_up = Vec::new();
		misses.ask(
			&track.state.consume(),
			&fills,
			&RangeSet::from_iter(std::iter::once(0..3)),
			&Asking {
				priority: 0,
				max_delay: None,
				order: Order::Ascending,
			},
			&mut given_up,
		);
		assert_eq!(misses.joins.len(), 1, "one fill covers the whole run");

		// Refusing the middle group splits the join's holder in two.
		misses.release(&RangeSet::from_iter(std::iter::once(1..2)));
		assert_eq!(misses.joins.len(), 1);
		misses.release(&RangeSet::from_iter([0..1, 2..3]));
		assert!(misses.joins.is_empty(), "the join goes with its last piece");
		assert!(misses.holders.is_empty());
	}

	#[kio::test]
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
	#[kio::test]
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
	#[kio::test]
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

	/// Judging the stretches a fill still owes by their shared successor gives up exactly
	/// what judging each stretch alone would, as groups arrive, start, abort, or leave the
	/// cache in any order, on either clock, against a window of one run or several.
	#[test]
	fn expiring_by_successor_matches_each_stretch_judged_alone() {
		use super::super::super::subscription::{Layered, Runs};
		use crate::Timestamp;

		/// The old judgement: each stretch between the live groups cached in a wanted run, by
		/// its own start.
		fn each(misses: &Misses, state: &TrackState, drift: &Drift) -> (RangeSet<u64>, RangeSet<u64>) {
			let waiter = kio::Waiter::noop();
			let (mut stale, mut arrived) = (RangeSet::new(), RangeSet::new());
			for run in misses.wanted.iter() {
				let mut from = run.start;
				let cached = state
					.lookup
					.range(run.clone())
					.filter(|(_, slot)| slot.is_walked())
					.map(|(&sequence, _)| sequence);
				for at in cached.chain(std::iter::once(run.end)) {
					if from < at && state.poll_drifted(from, &drift.edge.servable, drift.budget, None, &waiter) {
						stale.insert(from..at);
					}
					if at < run.end {
						arrived.insert(at..at + 1);
					}
					from = at.saturating_add(1);
				}
			}
			(stale, arrived)
		}

		let seed = std::cell::Cell::new(0x2545_f491_4f6c_dd1d_u64);
		let next = |bound: u64| {
			let mut x = seed.get();
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			seed.set(x);
			x % bound
		};
		const SEQUENCES: u64 = 32;
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let mut judged = 0;
		for round in 0..400 {
			let broadcast = crate::broadcast::Info::default().produce();
			let track = broadcast
				.create_track(
					"expire",
					crate::track::Info::default().with_timescale(crate::Timescale::MILLI),
				)
				.unwrap();
			let mut groups = Vec::new();
			let budget = Duration::from_millis(50 + next(200));
			let runs = |count: u64| {
				Runs::new((0..1 + next(count)).map(|_| {
					let start = next(SEQUENCES);
					start..=start + next(6)
				}))
			};
			let servable = match next(3) {
				0 => Servable::ALL,
				1 => Servable::Run {
					start: next(8),
					end: Some(SEQUENCES - next(8)),
				},
				_ => Servable::layered(
					Layered::new(Arc::new(runs(6)), runs(2)),
					(next(3) == 0).then(|| next(SEQUENCES + 2)),
					&Servable::NONE,
				),
			};
			let mut misses = Misses::default();
			for _ in 0..1 + next(6) {
				let start = next(SEQUENCES);
				misses.wanted.insert(start..start + 1 + next(6));
			}
			for step in 0..12 {
				let sequence = next(SEQUENCES);
				match next(6) {
					0..=2 => {
						if let Ok(mut group) = track.create_group(crate::group::Info { sequence }) {
							if next(4) > 0 {
								group
									.write_frame(at(sequence * 20), bytes::Bytes::from_static(b"x"))
									.unwrap();
							}
							groups.push(group);
						}
					}
					3 => {
						if let Some(index) = groups.iter().position(|group| group.sequence == sequence) {
							let _ = groups.swap_remove(index).abort(Error::Cancel);
						}
					}
					4 => {
						let _ = track.modify().unwrap().uncache(sequence);
					}
					_ => broadcast.info().pool.step(Duration::from_millis(next(120))),
				}
				let state = track.state.read();
				let drift = Drift {
					budget,
					bounded: false,
					edge: state.drift_edge(&servable),
				};
				let expected = each(&misses, &state, &drift);
				let found = misses.expired(&state, &drift, &kio::Waiter::noop());
				judged += expected.0.iter().count();
				assert_eq!(
					(
						found.0.iter().cloned().collect::<Vec<_>>(),
						found.1.iter().cloned().collect::<Vec<_>>()
					),
					(
						expected.0.iter().cloned().collect::<Vec<_>>(),
						expected.1.iter().cloned().collect::<Vec<_>>()
					),
					"round {round} step {step}: wanted {:?}",
					misses.wanted.iter().collect::<Vec<_>>()
				);
			}
		}
		assert!(judged > 100, "only {judged} stale runs judged");
	}

	/// A window whose time ends move only their runs over the same fixed runs is followed by
	/// what those runs moved over, and ends up judging and holding exactly what judging the
	/// whole window again would: the second reader below is handed an unshared copy of the
	/// same fixed runs on each move, so it always judges the window whole.
	#[kio::test]
	async fn a_moved_layer_is_judged_as_the_whole_window_would_be() {
		use super::super::super::subscription::{Layered, Runs};

		let seed = std::cell::Cell::new(0x2545_f491_4f6c_dd1d_u64);
		let next = |bound: u64| {
			let mut x = seed.get();
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			seed.set(x);
			x % bound
		};
		const SEQUENCES: u64 = 40;
		let runs = |count: u64| {
			Runs::new((0..next(count + 1)).map(|_| {
				let start = next(SEQUENCES);
				start..=start + next(5)
			}))
		};
		let snapshot = |misses: &Misses| {
			let sets = [
				&misses.wanted,
				&misses.judged,
				&misses.refused,
				&misses.given_up,
				&misses.refilled,
			]
			.map(|set| set.iter().cloned().collect::<Vec<_>>());
			let deferred: Vec<_> = misses.deferred.overlapping(0..u64::MAX).collect();
			let filled: Vec<_> = misses.filled.iter().copied().collect();
			(sets, deferred, filled, misses.joins.len())
		};
		let mut asked = 0;
		for round in 0..150 {
			let broadcast = crate::broadcast::Info::default().produce();
			let mut track = broadcast.create_track("moved", crate::track::Info::default()).unwrap();
			let budget = Duration::from_secs(5);
			let _reader = track.subscribe(Subscription::default().with_max_delay(budget));
			// A feed with holes, some of them under where it starts.
			for sequence in 0..SEQUENCES {
				if next(3) > 0 {
					track
						.create_group(crate::group::Info { sequence })
						.unwrap()
						.finish()
						.unwrap();
				}
			}
			if next(2) == 0 {
				track.start_at(next(SEQUENCES / 2)).unwrap();
			}
			let fills = track.state.read().fills.clone();
			fills.lock().add_handler();
			let fixed = Arc::new(runs(6));
			let cap = (next(3) == 0).then(|| next(SEQUENCES + 4));
			let asking = Asking {
				priority: 0,
				max_delay: Some(budget),
				order: Order::Ascending,
			};
			let (mut moved, mut whole) = (Misses::default(), Misses::default());
			let (mut given_moved, mut given_whole) = (Vec::new(), Vec::new());
			for revision in 0..8 {
				let layer = runs(3);
				let shared = Layered::new(fixed.clone(), layer.clone());
				let unshared = Layered::new(Arc::new((*fixed).clone()), layer);
				let key = (0, revision, cap);
				for (misses, view, given_up) in [
					(&mut moved, &shared, &mut given_moved),
					(&mut whole, &unshared, &mut given_whole),
				] {
					let target = |span: RangeInclusive<u64>| -> RangeInclusiveSet<u64> {
						let end = cap.map_or(*span.end(), |cap| (*span.end()).min(cap.saturating_sub(1)));
						if cap == Some(0) || *span.start() > end {
							return RangeInclusiveSet::new();
						}
						view.runs(*span.start())
							.take_while(|run| *run.start() <= end)
							.map(|run| *run.start()..=(*run.end()).min(end))
							.collect()
					};
					misses.retain(key, Some(view), || target(0..=u64::MAX - 1));
					let state = track.state.read();
					let drift = Drift {
						budget,
						bounded: false,
						edge: state.drift_edge(&Servable::ALL),
					};
					let asks = misses.judge(
						&state,
						key,
						Some(view),
						None,
						target,
						&Delivered::default(),
						&drift,
						given_up,
						&kio::Waiter::noop(),
					);
					drop(state);
					asked += asks.iter().count();
					misses.ask(&track.state.consume(), &fills, &asks, &asking, given_up);
				}
				assert_eq!(
					snapshot(&moved),
					snapshot(&whole),
					"round {round} revision {revision}: {fixed:?} under {:?}",
					shared_layer(&moved)
				);
				assert_eq!(given_moved, given_whole, "round {round} revision {revision}");
			}
		}
		assert!(asked > 100, "only {asked} runs asked");

		fn shared_layer(misses: &Misses) -> Option<Runs> {
			misses.view.as_ref().map(|view| view.layer().clone())
		}
	}

	/// A reader waiting on many fills looks only at the ones that answered, yet each still
	/// wakes it, and wakes the task that read it last: a reader read by another task since
	/// parked learns of the answer too.
	#[kio::test]
	async fn an_answered_fill_wakes_the_task_that_read_last() {
		use std::sync::atomic::{AtomicUsize, Ordering};

		#[derive(Default)]
		struct Count(AtomicUsize);
		impl Wake for Count {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}

		let broadcast = crate::broadcast::Info::default().produce();
		let mut track = broadcast.create_track("fills", crate::track::Info::default()).unwrap();
		let requests = track.requests();
		track.start_at(8).unwrap();
		track
			.create_group(crate::group::Info { sequence: 8 })
			.unwrap()
			.finish()
			.unwrap();
		// Four groups below where the feed starts, each a fill of its own.
		let ranges = [0..1, 2..3, 4..5, 6..7].map(crate::track::Range::groups);
		let mut reader = track.subscribe(
			Subscription::default()
				.with_max_delay(Duration::from_secs(60))
				.with_ranges(ranges.into_iter().chain([crate::track::Range::groups(8..)])),
		);
		let (first, second) = (Arc::new(Count::default()), Arc::new(Count::default()));
		let waiter = |count: &Arc<Count>| kio::Waiter::new(Waker::from(count.clone()));
		let first_waiter = waiter(&first);
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&first_waiter) {}
		let second_waiter = waiter(&second);
		assert!(reader.poll_recv_group(&second_waiter).is_pending());
		let super::super::Inner::Plain(cursor) = &reader.inner else {
			unreachable!("a local subscriber reads its own cursor");
		};
		let misses = cursor.misses.as_ref().expect("the reader asked fills");
		assert_eq!(misses.joins.len(), 4, "one join per fill");
		assert!(
			misses.answered.keys.lock().unwrap().is_empty(),
			"every join was looked at once and parked"
		);

		// One fill answers: only its join is looked at again, and the later task wakes.
		let fill = std::future::poll_fn(|cx| requests.poll_requested_fill(&kio::Waiter::new(cx.waker().clone())))
			.await
			.unwrap();
		let woken = second.0.load(Ordering::Relaxed);
		fill.drop_groups(fill.start().group..fill.end());
		assert!(
			second.0.load(Ordering::Relaxed) > woken,
			"the task that read last wakes"
		);
		let super::super::Inner::Plain(cursor) = &reader.inner else {
			unreachable!("a local subscriber reads its own cursor");
		};
		let answered = cursor.misses.as_ref().unwrap().answered.keys.lock().unwrap().len();
		assert_eq!(answered, 1, "only the join that answered is looked at again");
		let _ = reader.poll_recv_group(&second_waiter);
		let super::super::Inner::Plain(cursor) = &reader.inner else {
			unreachable!("a local subscriber reads its own cursor");
		};
		assert_eq!(
			cursor.misses.as_ref().unwrap().joins.len(),
			3,
			"the answered fill's join is gone"
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

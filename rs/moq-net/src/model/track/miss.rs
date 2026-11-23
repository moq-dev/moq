//! A reader's misses: the keys it reads that the cache doesn't hold and the live feed
//! will never deliver, asked of the track's fills until each arrives or is given up.
//!
//! Nothing above the newest group the feed produced is a miss, since it arrives live, so a
//! fill is bounded by construction. Below it, a hole is a miss once the feed is past it:
//! under where the feed starts, or accounted for by the arrival ledger without being cached
//! (evicted, or a gap the feed skipped). A hole the ledger still waits for is deferred and
//! judged again as the feed moves, or once its wait ends: the arrival grace, bounded by
//! [`ledger::GRACE`] when nothing else bounds it, so every hole is asked in finite time.
//!
//! Each key is judged once, except a group evicted after the reader judged it cached or a
//! fill answered it, and before it took it: a miss again, asked again however often it is
//! evicted, since a key a reader is owed is never lost to the cache. Each refill reaches a
//! reader: a fill holds the first group it answered until a reader takes it (see
//! [`super::held`]).
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
use super::super::subscription::Cover;
pub(super) use super::fill::Asking;
use super::fill::{Ask, FillState, Join};
use super::{Drift, Hole, Order, Position, TrackState};
use crate::{Error, ledger};

/// What a probe judges against: the span read, the feed's newest group, where the feed
/// and readers start, and the drops revision.
type JudgedAt = (u64, u64, Option<u64>, Option<u64>, Option<u64>, super::FeedStart, u64);

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
	/// The keys judged already, within what the reader reads up to the newest group.
	judged: RangeSet<u64>,
	/// Holes the live feed may still deliver, revisited on a ledger change or deadline.
	deferred: Deferred,
	/// Asked under the feed's start and refused, never asked again: given up once the feed
	/// gives up on them, unless they arrive first.
	refused: RangeSet<u64>,
	/// The fills this reader waits on, under the ids `holders` names them by.
	joins: BTreeMap<u64, Join>,
	/// The join holding each key: a reader's joins hold disjoint groups, so a change
	/// touches only the joins holding what it changes, never every fill the reader asked.
	holders: RangeMap<u64, u64>,
	/// What each join parks on, under its id: a waiter of its own, which records that its
	/// fill answered before waking the reader, so [`Self::resolve`] looks only at the fills
	/// that answered, never at every fill still out.
	parked: BTreeMap<u64, kio::Waiter>,
	/// The joins whose fills answered since the reader last looked.
	answered: Arc<Answered>,
	/// The id the next join takes.
	next_join: u64,
	/// What the reader asked its fills with last.
	asking: Option<Asking>,
	/// The keys of `joins` still unanswered.
	wanted: RangeSet<u64>,
	/// Answered by a fill and cached, not handed out yet.
	filled: BTreeSet<u64>,
	/// What each join's fill answered that a drop poll took in, told to the fill only once a
	/// read picks it up (see [`Self::pick_up`]).
	picked: BTreeMap<u64, RangeSet<u64>>,
	/// Never asked again: named dropped to the reader.
	given_up: RangeSet<u64>,
	/// The part of `given_up` a fill answered was never produced: reported skipped, not
	/// dropped, whatever the cache still records of it.
	skipped: RangeSet<u64>,
	/// The part of `given_up` judged stale, with the budget each run was judged under: a
	/// wider budget asks it again (see [`Self::widen`]).
	stale: RangeMap<u64, Duration>,
	/// No wider than the narrowest budget in `stale`, so [`Self::widen`] looks only when a
	/// budget wider than it could take a run back.
	narrowest: Option<Duration>,
	/// The window revision and cap what this holds was last clipped to, with the window's
	/// missable groups then, `None` while every group was.
	clipped: Option<(u64, Option<u64>, Option<Cover>)>,
	/// The window's missable groups the last probe judged against, `None` while every group
	/// was, so a moved window diffs this snapshot against its own and judges only what moved.
	view: Option<Cover>,
	/// Where the track leaves what it evicts that this reader judged cached, once it judged.
	inbox: Option<Inbox>,
	/// The track's eviction count this reader last emptied its inbox at.
	evictions: u64,
	/// The last key of the highest received stale run above the newest group that this
	/// reader cannot trust, as of [`Self::unknown_at`]; see [`Self::unknown`].
	unknown: Option<u64>,
	/// The drops revision, budget, and coverage [`Self::unknown`] was found under: while the
	/// terms hold, only the runs changed since are looked at.
	unknown_at: Option<(u64, Duration, super::Coverage)>,
	/// The [`Forgotten`] revision this reader judged again up to.
	forgotten: u64,
}

/// A run of misses the reader gives up, for the drop cursor to name, with the budget it was
/// judged stale under when that is why.
pub(super) type GivenUp = Vec<(ops::Range<u64>, Hole)>;

/// The joins whose fills changed their answer, recorded by each join's own waiter before it
/// wakes the reader's tasks.
///
/// Every waiter the reader resolved with since the last answer wakes, as each would had it
/// polled the fills itself: a read parked behind its idle gate and a drop poll each resolve
/// with a waiter of their own, and an answer only one of them saw would leave the other
/// parked on a want already settled.
#[derive(Default)]
struct Answered {
	ids: Mutex<BTreeSet<u64>>,
	tasks: Mutex<kio::WaiterList>,
}

/// Wakes the reader once join `id`'s fill answers, recording which join it was.
struct JoinWake {
	id: u64,
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
			.ids
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(self.id);
		// Woken with no lock held: an executor may poll inline.
		let mut tasks = answered.tasks.lock().unwrap_or_else(PoisonError::into_inner).take();
		tasks.wake();
	}
}

/// How many runs [`Forgotten`] keeps apart before it coalesces the oldest.
const FORGOTTEN_KEPT: usize = 4;

/// The runs a track reclaimed from its drop set, by revision, for each reader to judge again
/// what it judged inside them. Past [`FORGOTTEN_KEPT`] runs the oldest two coalesce into the
/// span covering both under the newer revision, so this stays a few entries however much is
/// forgotten, and a reader that fell behind judges a wider span again, never a narrower one.
#[derive(Default)]
pub(super) struct Forgotten {
	runs: std::collections::VecDeque<(u64, ops::Range<u64>)>,
	revision: u64,
}

impl Forgotten {
	/// Record `run` as forgotten.
	pub fn record(&mut self, run: ops::Range<u64>) {
		self.revision += 1;
		self.runs.push_back((self.revision, run));
		if self.runs.len() > FORGOTTEN_KEPT
			&& let Some((_, oldest)) = self.runs.pop_front()
			&& let Some((_, next)) = self.runs.front_mut()
		{
			*next = oldest.start.min(next.start)..oldest.end.max(next.end);
		}
	}

	/// The runs forgotten after `revision`, possibly wider, with the revision they bring a
	/// reader to.
	fn since(&self, revision: u64) -> (impl Iterator<Item = ops::Range<u64>> + '_, u64) {
		let runs = self
			.runs
			.iter()
			.filter(move |(at, _)| *at > revision)
			.map(|(_, run)| run.clone());
		(runs, self.revision)
	}
}

/// The current deferred runs, indexed by key and by their next judgement deadline.
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

/// The part of `hole` the track's drops leave open for a reader under `drift`: a received
/// stale run that does not settle it is no verdict for it (see `Drift::settles`), so it asks
/// the group again.
pub(super) fn unsettled(state: &TrackState, hole: ops::Range<u64>, drift: &Drift) -> RangeSet<u64> {
	let mut left = RangeSet::new();
	left.insert(hole.clone());
	// Only what overlaps the hole, so judging every run costs each one the same.
	for (run, gone) in state.drops.overlapping(&hole) {
		if drift.settles(run.start.max(hole.start), gone) {
			left.remove(run.clone());
		}
	}
	left
}

impl Misses {
	/// The lowest key still to come from a fill: asked, or answered and not handed out.
	pub fn lowest(&self) -> Option<u64> {
		let wanted = self.wanted.iter().next().map(|run| run.start);
		let filled = self.filled.first().copied();
		match (wanted, filled) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		}
	}

	/// The highest key still to come from a fill.
	fn highest(&self) -> Option<u64> {
		let wanted = self.wanted.iter().next_back().map(|run| run.end - 1);
		wanted.max(self.filled.last().copied())
	}

	/// Forget `lost`, which the reader no longer reads: its fills no longer owe it those
	/// groups.
	fn lose(&mut self, lost: &RangeSet<u64>) {
		for run in lost.iter() {
			let filled: Vec<u64> = self.filled.range(run.clone()).copied().collect();
			for key in filled {
				self.filled.remove(&key);
			}
			self.wanted.remove(run.clone());
		}
		self.release(lost);
	}

	/// Whether no fill can answer this reader and no gap is timed: nothing joined, nothing
	/// deferred. Its probe then has nothing to resolve.
	pub fn is_idle(&self) -> bool {
		self.joins.is_empty() && self.deferred.runs.is_empty()
	}

	/// Whether a fill still owes this reader a group.
	pub fn is_waiting(&self) -> bool {
		!self.wanted.is_empty()
	}

	/// Whether a fill answered a group not handed out yet.
	pub fn has_filled(&self) -> bool {
		!self.filled.is_empty()
	}

	/// The lowest key asked and not answered yet.
	pub fn lowest_wanted(&self) -> Option<u64> {
		self.wanted.iter().next().map(|run| run.start)
	}

	/// Everything given up so far, for a drop cursor created after.
	pub fn given_up(&self) -> &RangeSet<u64> {
		&self.given_up
	}

	/// The part of [`Self::given_up`] never produced, reported skipped.
	pub fn skipped(&self) -> &RangeSet<u64> {
		&self.skipped
	}

	/// The exclusive end of the run from `key` this reader's fills refused or it gave
	/// up, `None` when it may still be read.
	pub fn gone(&self, key: u64) -> Option<u64> {
		[&self.refused, &self.given_up]
			.into_iter()
			.find_map(|runs| runs.get(&key))
			.map(|run| run.end)
	}

	/// The next answered group still cached in `order`. One evicted before it was read is
	/// added to `lost`, for the reader to ask again, since a key it is owed is never lost to
	/// the cache; one aborted since is given up.
	pub fn next_filled(
		&mut self,
		state: &TrackState,
		order: Order,
		lost: &mut RangeSet<u64>,
		given_up: &mut GivenUp,
	) -> Option<crate::group::Handle> {
		loop {
			let key = match order {
				Order::Ascending => self.filled.first(),
				Order::Descending => self.filled.last(),
			}
			.copied()?;
			if let Some(slot) = state.lookup.get(&key).filter(|slot| slot.serves_end()) {
				return Some(slot.serving().clone());
			}
			self.filled.remove(&key);
			let mut gone = RangeSet::new();
			gone.insert(key..key + 1);
			self.release(&gone);
			match state.lookup.contains_key(&key) {
				true => self.give_up(key..key + 1, given_up),
				false => lost.insert(key..key + 1),
			}
		}
	}

	/// Take `key` as handed out, if a fill answered it.
	pub fn take_filled(&mut self, key: u64) {
		if self.filled.remove(&key) {
			let mut lost = RangeSet::new();
			lost.insert(key..key + 1);
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
			let id = self.next_join;
			self.next_join += 1;
			self.holders.insert(join.groups.clone(), id);
			self.joins.insert(id, join);
			let wake = Arc::new(JoinWake {
				id,
				answered: self.answered.clone(),
			});
			self.parked.insert(id, kio::Waiter::new(Waker::from(wake)));
			// Looked at once, which parks it.
			self.answered
				.ids
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.insert(id);
		}
	}

	/// Stop holding `lost` in the joins holding any of it, so their fills no longer owe it
	/// this reader, and drop each join left holding nothing.
	fn release(&mut self, lost: &RangeSet<u64>) {
		for run in lost.iter() {
			while let Some((held, &id)) = self.holders.overlapping(run).next() {
				// Only this piece: a join split by an earlier release has other pieces in
				// `lost`, and each must still find the join when its turn comes.
				let piece = held.start.max(run.start)..held.end.min(run.end);
				self.holders.remove(piece.clone());
				let join = self.joins.get_mut(&id).expect("a holder names a join");
				join.release(&RangeSet::from_iter([piece]));
				if join.is_empty() {
					self.joins.remove(&id);
					self.parked.remove(&id);
					self.picked.remove(&id);
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
		view: Option<&Cover>,
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
				view.map(|view| old.gained(view, from, highest))
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
		let outside = |key: u64| key < from || !admitted.contains(&key);
		self.filled.retain(|key| !outside(*key));
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
	/// runs to ask again: refused as too old for a budget tighter than this reader's, or
	/// accepted and evicted before this reader took them in.
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
		self.answered
			.tasks
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.register(waiter);
		// Taken after registering: an answer landing since wakes the waiter registered above.
		let ids = std::mem::take(&mut *self.answered.ids.lock().unwrap_or_else(PoisonError::into_inner));
		let mut ids = ids.into_iter().peekable();
		while let Some(&id) = ids.peek() {
			let (Some(join), Some(parked)) = (self.joins.get(&id), self.parked.get(&id)) else {
				ids.next();
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
				ids.next();
				continue;
			}
			// Only what is unanswered: a join holding only filled groups until they are taken
			// has nothing to answer.
			// What a fill that ended left unanswered, rather than what it accepted.
			let mut unanswered = RangeSet::new();
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
					unanswered = left;
					answers
				}
				Poll::Pending => {
					ids.next();
					continue;
				}
			};
			// Read only after the outcome's lock is released.
			let state = state.read();
			// What is answered is no longer owed this reader, except a filled group: held until
			// taken, so its source keeps writing it while the reader picks it up.
			let mut taken = RangeSet::new();
			let mut seen = RangeSet::new();
			for (groups, answer) in answers {
				self.wanted.remove(groups.clone());
				taken.insert(groups.clone());
				match answer {
					Ok(()) => {
						seen.insert(groups.clone());
						// What the cache holds is filled. The rest of what a fill that ended left
						// unanswered is settled by run, since it may span far more groups than
						// were ever cached; an accepted group evicted before this reader took it
						// in is asked again, since a key it is owed is never lost to the cache.
						// Walked by the groups cached, not by key: a group accepted below the fill's
						// start answers every key up to it.
						let mut evicted = RangeSet::new();
						if !unanswered.overlaps(&groups) {
							let mut from = groups.start;
							for &key in state.lookup.range(groups.clone()).map(|(key, _)| key) {
								if from < key {
									evicted.insert(from..key);
								}
								from = key + 1;
							}
							if from < groups.end {
								evicted.insert(from..groups.end);
							}
						}
						for run in evicted.gaps(&groups) {
							self.settle(&state, run, given_up);
						}
						for (&key, slot) in state.lookup.range(groups) {
							if slot.serves_end() {
								self.filled.insert(key);
								taken.remove(key..key + 1);
							}
						}
						retry.extend(evicted);
						// Seen cached, though judging may not run again before it is evicted.
						if let Some(inbox) = &self.inbox {
							inbox.saw(state.cached);
						}
					}
					Err((Error::Old, asked)) if looser(max_delay, asked) => retry.insert(groups),
					// Stale under the budget it was asked with, which this reader's is no wider
					// than: a stale verdict a wider budget asks again, never a refusal spent for good.
					Err((Error::Old, Some(asked))) => self.give_up_stale(groups, asked, given_up),
					// Never produced: passed over, not lost, however soon the cache forgets the skip.
					Err((Error::Skipped, _)) => {
						for hole in holes(&state, groups) {
							self.give_up_skipped(hole, given_up);
						}
					}
					// A drop the track recorded is named by the drop cursor already.
					Err(_) => self.settle(&state, groups, given_up),
				}
			}
			drop(state);
			if !seen.is_empty() {
				self.picked.entry(id).or_default().extend(seen);
			}
			self.release(&taken);
			// Judged again: the same join may have more to answer, or park the waiter now.
		}
		// A refusal held while the feed may still deliver its groups is judged again once
		// the feed gives up on them, which nothing else wakes this reader for. Registered
		// whether or not the deadline moved here: `ask` settles what no handler can take
		// before this runs, and the judge registered only the deadline it saw. One already
		// passed is the judge's to take up, which runs once the feed's start resolves: armed
		// again, it would wake this reader at once, every poll, while nothing can change.
		if let Some(at) = self.deferred.deadline() {
			let state = state.read();
			if at > state.cache.pool().now_wall() {
				state.cache.watch_wall(at, waiter);
			}
		}
		retry
	}

	/// Tell each fill this reader joined that a read picked up what it answered, so its
	/// handler may serve the next group. Only a read: a drop poll takes in answers too, from
	/// a publisher that may be unable to send the groups, which must not pace the fill.
	pub fn pick_up(&mut self) {
		for (id, groups) in std::mem::take(&mut self.picked) {
			self.joins[&id].saw(&groups);
		}
	}

	/// The last key of the highest received stale run above the newest group that `drift`
	/// does not settle. While the terms hold and the run it found still stands, only the runs
	/// changed since are looked at, so a drop costs each reader the runs it touches.
	fn unknown(&mut self, state: &TrackState, drift: &Drift) -> Option<u64> {
		let above = |run: &ops::Range<u64>| state.newest_produced.is_none_or(|newest| run.end - 1 > newest);
		let unsettled = |run: &ops::Range<u64>, hole| above(run) && !drift.settles(run.start, hole);
		let revision = state.drops_changes.revision();
		let since = self
			.unknown_at
			.as_ref()
			.filter(|(_, budget, coverage)| *budget == drift.budget && *coverage == drift.coverage)
			.map(|(seen, ..)| *seen)
			// The newest group only rises, so the run found stays the highest unless it fell
			// below it or changed.
			.filter(|_| {
				self.unknown.is_none_or(|top| {
					state
						.drops
						.get_key_value(&top)
						.is_some_and(|(run, hole)| unsettled(run, hole))
				})
			});
		self.unknown = match since {
			Some(seen) => {
				let mut unknown = self.unknown;
				for changed in state.drops_changes.since(seen) {
					for (run, hole) in state.drops.overlapping(&changed) {
						if unsettled(run, hole) {
							unknown = unknown.max(Some(run.end - 1));
						}
					}
				}
				unknown
			}
			None => state
				.drops
				.iter()
				.rev()
				.take_while(|(run, _)| above(run))
				.find(|(run, hole)| !drift.settles(run.start, hole))
				.map(|(run, _)| run.end - 1),
		};
		self.unknown_at = Some((revision, drift.budget, drift.coverage.clone()));
		self.unknown
	}

	/// Judge the holes in `target` the last probe has not, returning the runs to ask a
	/// fill for. Gives up a run already stale under this reader's budget. `named` is the
	/// lowest group the reader's ranges name, which may reach below the feed's start,
	/// `bound` the exclusive end of a window whose ranges all end, and `taken` what the
	/// reader handed out, which an eviction never makes a miss again.
	#[expect(
		clippy::too_many_arguments,
		reason = "one probe's inputs, borrowed apart from its cursor"
	)]
	pub fn judge(
		&mut self,
		state: &TrackState,
		id: (u64, u64, Option<u64>),
		view: Option<&Cover>,
		named: Option<u64>,
		bound: Option<u64>,
		mut target: impl FnMut(RangeInclusive<u64>) -> RangeInclusiveSet<u64>,
		taken: &Delivered,
		drift: &Drift,
		given_up: &mut GivenUp,
		waiter: &kio::Waiter,
	) -> RangeSet<u64> {
		let mut asks = RangeSet::new();
		// On a copy no subscription feeds, a bounded window's holes are misses up to its
		// end, since no feed will deliver them nor produce anything to judge them below.
		let bound = bound.filter(|_| state.unfed).and_then(|end| end.checked_sub(1));
		// The feed's start is unknown until the serving session resolves it, and readers
		// take nothing from a cache that does not reflect the feed.
		if matches!(state.start, super::FeedStart::Pending(_)) || !state.readable() && bound.is_none() {
			return asks;
		}
		// A received stale run this reader cannot trust is still asked for, though nothing above
		// it arrived: the subscription that dropped it never delivers it, and a reader narrower
		// than the upstream's budget changes none. See `TrackState::unfed`. Only runs above the newest
		// group need a look.
		let unknown = self.unknown(state, drift);
		let Some(newest) = state.newest_produced.max(bound).max(unknown) else {
			return asks;
		};
		let at = (
			id.0,
			id.1,
			id.2,
			Some(newest),
			state.live_floor,
			// Whole, not just its key: a start resolving from a budget turns the history
			// below it from unasked into fillable.
			state.start,
			state.drops_revision,
		);
		let ended = state.nothing_arrives();
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
		let grace = state.miss_grace();
		if self.judge_arrivals(state, id, at, view, &mut target, taken, drift, grace, ended) {
			self.ledger_revision = ledger.revision();
			return asks;
		}
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
		// A window that moved keeps most of its runs, so only what it moved over is judged again
		// below.
		let diffed = match (&self.view, view) {
			(Some(old), Some(view)) => Some((old.clone(), view)),
			_ => None,
		};
		let terms = self.at.is_none_or(|previous| {
			(previous.1 != at.1 && diffed.is_none())
				|| previous.2 != at.2
				|| previous.4 != at.4
				|| previous.5 != at.5
				|| at.0 < previous.0
		}) || self.budget != Some(drift.budget)
			|| self.grace != Some(grace)
			|| self.ended != ended;
		let low = id.0;
		let top = newest;
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
			self.skipped = clip(&self.skipped);
			if !self.stale.is_empty() {
				let mut stale = RangeMap::new();
				for (run, budget) in self.stale.iter() {
					for kept in self.given_up.overlapping(run) {
						stale.insert(kept.start.max(run.start)..kept.end.min(run.end), *budget);
					}
				}
				self.stale = stale;
			}
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
				// A received stale run settles this reader only under the terms it judged, so
				// what it settled is judged again under new ones.
				for (dropped, hole) in state.drops.overlapping(run) {
					if hole.stale_budget().is_some() {
						let dropped = dropped.start.max(run.start)..dropped.end.min(run.end);
						for judged in self.judged.overlapping(&dropped) {
							candidates.insert(judged.start.max(dropped.start)..judged.end.min(dropped.end));
						}
					}
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
					&mut self.skipped,
				] {
					set.remove(0..low);
				}
				self.stale.remove(0..low);
				self.deferred.remove(0..low);
			}
			// What the window no longer covers is forgotten, and what it newly covers
			// judged, within what the probe judges: from the floor up to the newest group,
			// below the cap.
			let last = id.2.map_or(Some(top), |cap| cap.checked_sub(1).map(|cap| cap.min(top)));
			if let Some((old, view)) = &diffed
				&& let Some(last) = last
				&& self.at.is_some_and(|previous| previous.1 != at.1)
			{
				let half = |run: RangeInclusive<u64>| *run.start()..run.end().saturating_add(1);
				for run in old.gained(view, low, last).into_iter().map(half) {
					for set in [
						&mut self.judged,
						&mut self.refused,
						&mut self.given_up,
						&mut self.skipped,
					] {
						set.remove(run.clone());
					}
					self.stale.remove(run.clone());
					self.deferred.remove(run);
				}
				for run in view.gained(old, low, last).into_iter().map(half) {
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
		// A run the track forgot is unknown again, so what this reader judged inside it and has
		// not taken is judged afresh: asked of a fill, or given up by its budget.
		let (forgotten, revision) = state.forgotten.since(self.forgotten);
		for run in forgotten {
			let run = run.start.max(low)..run.end.min(top.saturating_add(1));
			if run.is_empty() {
				continue;
			}
			for judged in self.judged.overlapping(&run) {
				let judged = judged.start.max(run.start)..judged.end.min(run.end);
				for fresh in taken.gaps(judged) {
					candidates.insert(fresh);
				}
			}
		}
		self.forgotten = revision;
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
						.map(|(&key, _)| key)
				})
				.collect();
			for key in arrived {
				self.refused.remove(key..key + 1);
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
		let evicted = inbox.judge(state.cached, inbox_floor(low, taken), top);
		self.evictions = state.evictions;
		let mut refills = RangeSet::new();
		for run in evicted.iter() {
			let run = run.start.max(low)..run.end.min(top.saturating_add(1));
			if run.is_empty() {
				continue;
			}
			for part in target(run.start..=run.end - 1).iter() {
				for gap in taken.gaps(*part.start()..*part.end() + 1) {
					for judged in self.judged.overlapping(&gap) {
						refills.insert(judged.start.max(gap.start)..judged.end.min(gap.end));
					}
				}
			}
		}
		// Still asked, its fill settles it; given up already, it was named. Only what overlaps
		// it, so a reader still asking thousands of fills pays nothing here.
		let settled: Vec<ops::Range<u64>> = refills
			.iter()
			.flat_map(|run| {
				let wanted = self.wanted.overlapping(run);
				wanted
					.chain(self.given_up.overlapping(run))
					.map(move |settled| settled.start.max(run.start)..settled.end.min(run.end))
			})
			.collect();
		for run in settled {
			refills.remove(run);
		}
		for run in refills.iter() {
			// A fill's answer evicted before it was taken is no longer held for this reader,
			// which asks for it again however often it is evicted: a fill holds what it fetched
			// until a reader takes it, so each refill reaches one.
			let filled: Vec<u64> = self.filled.range(run.clone()).copied().collect();
			for key in filled {
				self.filled.remove(&key);
				self.release(&RangeSet::from_iter(std::iter::once(key..key + 1)));
			}
			candidates.insert(run.clone());
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
		}
		let start = state.start.key().unwrap_or(0);
		// Below a start its publisher resolved from a budget, which an update may have changed
		// in flight, every older group is history a fill may still find. A feed that began at
		// the live edge or the floor the copy asked for leaves older groups as history only a
		// reader naming them asked for.
		let unasked = state.fills_from_start
			|| !matches!(state.start, super::FeedStart::Resolved(_))
				&& state.anchors.received()
				&& named.is_none_or(|named| named >= start);
		if unasked && start > 0 {
			candidates.remove(0..start);
		}
		// History left unasked stays unjudged, so a start that changes judges it again.
		for run in candidates.iter() {
			self.judged.insert(run.clone());
		}
		for run in candidates.iter() {
			// Backfill another reader's fill cached is no hole, yet the arrival walk never
			// reaches it: this reader takes it as filled, as it would its own fill's answer,
			// even while the live feed may still deliver the group, so a warm cache serves
			// what a cold one fills. A live copy landing later is the same group handed out.
			for held in backfill(state, run.clone()).iter() {
				self.filled.extend(held.clone());
			}
			for hole in holes(state, run.clone()) {
				for hole in self.unclaimed(state, hole, drift) {
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
		// A gap the feed still waits for becomes a miss when it folds.
		if let Some(at) = self.deferred.deadline() {
			state.cache.watch_wall(at, waiter);
		}

		// An old cache's floor is a boundary inside a missing run, not a drift verdict
		// for that whole run. Discard only its prefix before asking for the fresh suffix.
		let floor = state.cache_floor(drift.budget);
		let old: Vec<_> = asks
			.overlapping(&(0..floor))
			.map(|run| run.start..run.end.min(floor))
			.collect();
		for run in old {
			asks.remove(run.clone());
			self.give_up(run, given_up);
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
			self.give_up_stale(run, drift.budget, given_up);
		}
		asks
	}

	/// Judge what changed when only the feed's newest group moved up, and the reader's floor
	/// with it, returning whether that was all. Each group arriving live changes nothing else,
	/// so a reader that takes them as they come judges every one here.
	///
	/// With nothing deferred or refused, the arrival ledger's changes touch nothing this
	/// reader holds, and with no eviction counted, its inbox holds none. A new group the
	/// arrival walk serves is then neither a hole nor backfill: judged, with nothing to ask.
	/// Anything else, a group not walked included, is left to [`Self::judge`] whole.
	#[expect(
		clippy::too_many_arguments,
		reason = "the judgement's inputs, as Self::judge takes them"
	)]
	fn judge_arrivals(
		&mut self,
		state: &TrackState,
		id: (u64, u64, Option<u64>),
		at: JudgedAt,
		view: Option<&Cover>,
		target: &mut impl FnMut(RangeInclusive<u64>) -> RangeInclusiveSet<u64>,
		taken: &Delivered,
		drift: &Drift,
		grace: Duration,
		ended: bool,
	) -> bool {
		let Some(previous) = self.at else {
			return false;
		};
		let (Some(judged_to), Some(newest)) = (previous.3, at.3) else {
			return false;
		};
		let only_arrivals = previous.0 <= at.0
			&& judged_to <= newest
			&& (previous.1, previous.2, previous.4, previous.5, previous.6) == (at.1, at.2, at.4, at.5, at.6);
		if !only_arrivals
			|| !self.deferred.runs.is_empty()
			|| !self.refused.is_empty()
			|| self.budget != Some(drift.budget)
			|| self.grace != Some(grace)
			|| self.ended != ended
			|| self.drops_revision != state.drops_changes.revision()
			|| self.evictions != state.evictions
			|| self.view.as_ref() != view
		{
			return false;
		}
		let Some(inbox) = &self.inbox else {
			return false;
		};
		// The reader's floor, as the whole judgement takes it: a resumed copy's live floor
		// can sit above history the reader still reads.
		let low = id.0;
		let top = newest;
		let first = judged_to.saturating_add(1).max(low);
		let mut fresh = RangeSet::new();
		if first <= top {
			for run in target(first..=top).iter() {
				for gap in self.judged.gaps(&(*run.start()..*run.end() + 1)) {
					fresh.insert(gap);
				}
			}
		}
		let walked = fresh.iter().all(|run| {
			let mut next = run.start;
			for (&key, slot) in state.lookup.range(run.clone()) {
				if key != next || !slot.is_walked() {
					return false;
				}
				next = key + 1;
			}
			next == run.end
		});
		if !walked {
			return false;
		}
		if low > 0 {
			for set in [
				&mut self.judged,
				&mut self.refused,
				&mut self.given_up,
				&mut self.skipped,
			] {
				set.remove(0..low);
			}
			self.stale.remove(0..low);
		}
		let evicted = inbox.judge(state.cached, inbox_floor(low, taken), top);
		debug_assert!(evicted.is_empty(), "an eviction landed without counting");
		for run in fresh.iter() {
			self.judged.insert(run.clone());
		}
		self.at = Some(at);
		true
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
		// Below an old cache's floor is a boundary, not a drift verdict; above it, a stretch
		// went stale under this reader's budget.
		let floor = state.cache_floor(drift.budget);
		for run in stale.iter() {
			self.give_up(run.start..run.end.min(floor), given_up);
			self.give_up_stale(run.start.max(floor)..run.end, drift.budget, given_up);
		}
	}

	/// What [`Self::expire`] gives up as stale, and what arrived live, parking `waiter`.
	fn expired(&self, state: &TrackState, drift: &Drift, waiter: &kio::Waiter) -> (RangeSet<u64>, RangeSet<u64>) {
		let floor = state.cache_floor(drift.budget);
		// A floor raised after this fill was asked can cut through its wanted run.
		let mut stale: RangeSet<u64> = self
			.wanted
			.overlapping(&(0..floor))
			.map(|run| run.start..run.end.min(floor))
			.collect();
		let mut arrived = RangeSet::new();
		let servable = &drift.edge.servable;
		let mut next = self
			.wanted
			.overlapping(&(floor..u64::MAX))
			.next()
			.map(|run| run.start.max(floor));
		while let Some(from) = next {
			let bound = state.arrival(from, servable).map(|slot| slot.group.start);
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
				.find(|key| state.lookup.get(key).is_some_and(|slot| slot.serves_end()))
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
				fetch: false,
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

	/// The part of `hole` nobody has a verdict on yet: a received stale run that does not
	/// settle this reader is no verdict for it (see `Drift::settles`).
	fn unclaimed(&self, state: &TrackState, hole: ops::Range<u64>, drift: &Drift) -> Vec<ops::Range<u64>> {
		let mut left = self.unanswered(state, hole.clone(), drift);
		for run in self.wanted.overlapping(&hole) {
			left.remove(run.clone());
		}
		left.iter().cloned().collect()
	}

	/// The part of `hole` with no verdict for this reader and no answer from its fills: still
	/// to ask, or asked and not answered.
	pub(super) fn unanswered(&self, state: &TrackState, hole: ops::Range<u64>, drift: &Drift) -> RangeSet<u64> {
		let mut left = unsettled(state, hole.clone(), drift);
		let mut given: RangeSet<u64> = self.given_up.overlapping(&hole).cloned().collect();
		// A run given up stale under a narrower budget is no verdict for this reader, even
		// before its next probe takes it back (see `Self::widen`).
		for (run, judged) in self.stale.overlapping(&hole) {
			if *judged < drift.budget {
				given.remove(run.clone());
			}
		}
		for run in given.iter() {
			left.remove(run.clone());
		}
		for key in self.filled.range(hole) {
			left.remove(*key..*key + 1);
		}
		left
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
					.insert(run.clone(), state.arrivals.folds_at(run.start, state.miss_grace()));
			}
		}
	}

	pub(super) fn give_up(&mut self, groups: ops::Range<u64>, given_up: &mut GivenUp) {
		self.give_up_judged(groups, None, given_up);
	}

	/// Give up `groups` as stale under `budget`, a verdict a wider budget reverses.
	fn give_up_stale(&mut self, groups: ops::Range<u64>, budget: Duration, given_up: &mut GivenUp) {
		self.give_up_judged(groups, Some(budget), given_up);
	}

	fn give_up_judged(&mut self, groups: ops::Range<u64>, stale: Option<Duration>, given_up: &mut GivenUp) {
		if groups.is_empty() {
			return;
		}
		self.given_up.insert(groups.clone());
		self.skipped.remove(groups.clone());
		match stale {
			Some(budget) => {
				self.stale.insert(groups.clone(), budget);
				self.narrowest = Some(self.narrowest.map_or(budget, |narrowest| narrowest.min(budget)));
			}
			None => self.stale.remove(groups.clone()),
		}
		given_up.push((groups, Hole::missed(stale)));
	}

	/// Give up `groups` as never produced, as a fill answered: reported skipped.
	fn give_up_skipped(&mut self, groups: ops::Range<u64>, given_up: &mut GivenUp) {
		if groups.is_empty() {
			return;
		}
		self.given_up.insert(groups.clone());
		self.skipped.insert(groups.clone());
		self.stale.remove(groups.clone());
		given_up.push((groups, Hole::Skip));
	}

	/// Take back the runs given up as stale under a budget narrower than `budget`, returning
	/// them for the caller to reopen with [`Self::reopen`]: only a widening reverses a stale
	/// verdict. Looks only when `budget` is wider than the narrowest it holds, whichever
	/// budget a verdict was judged under and whenever it arrived.
	pub fn widen(&mut self, budget: Duration) -> RangeSet<u64> {
		if self.narrowest.is_none_or(|narrowest| budget <= narrowest) {
			return RangeSet::new();
		}
		let widened: RangeSet<u64> = self
			.stale
			.iter()
			.filter(|(_, judged)| **judged < budget)
			.map(|(run, _)| run.clone())
			.collect();
		for run in widened.iter() {
			self.stale.remove(run.clone());
		}
		self.narrowest = self.stale.iter().map(|(_, judged)| *judged).min();
		widened
	}

	/// Judge `run` afresh: no longer given up, and judged again by the next probe.
	pub fn reopen(&mut self, run: ops::Range<u64>) {
		self.given_up.remove(run.clone());
		self.judged.remove(run);
		// The next probe judges under new terms, which looks at every key it has not judged.
		self.at = None;
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

	/// Leave `key`, cached as count `cached`, with each reader that judged it cached.
	/// Returns whether any did.
	pub(super) fn evicted(&mut self, key: u64, cached: u64) -> bool {
		let Some(end) = key.checked_add(1) else {
			return false;
		};
		let mut landed = false;
		// Every inbox is visited anyway, so a dropped reader's goes here too.
		self.inboxes.retain(|inbox| {
			if Arc::strong_count(&inbox.0) == 1 {
				return false;
			}
			let mut judging = inbox.0.lock().unwrap_or_else(PoisonError::into_inner);
			if cached <= judging.cached && judging.span.as_ref().is_some_and(|span| span.contains(&key)) {
				judging.evicted.insert(key..end);
				landed = true;
			}
			true
		});
		self.kept = self.inboxes.len();
		landed
	}
}

impl TrackState {
	/// How long a hole the live feed may still deliver is waited for before it is a miss:
	/// the arrival grace, or [`ledger::GRACE`] when nothing bounds that, so every hole is
	/// asked or given up in finite time. A reader with no budget on a track with no max age
	/// would otherwise wait out an unbounded grace, and a read of the hole would never end.
	pub(super) fn miss_grace(&self) -> Duration {
		match self.arrival_grace() {
			Duration::MAX => ledger::GRACE,
			grace => grace,
		}
	}

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
		// Nothing more arrives after an abort, a seal, once the live edge reached a declared
		// end with no tail owed, or while no subscription feeds the copy: until then, a group
		// below the end may still arrive.
		if self.nothing_arrives() {
			unfed.insert(hole);
			return (unfed, awaited);
		}
		let grace = self.miss_grace();
		let mut gap =
			|groups: ops::Range<u64>, unfed: &mut RangeSet<u64>| match self.arrivals.folds_at(groups.start, grace) {
				Some(at) if at <= now => unfed.insert(groups),
				folds => awaited.push((groups, folds)),
			};
		// The upstream accounted for a received stale run, so the feed brings none of it unless
		// a new upstream subscription supersedes the run.
		let mut fed = RangeSet::new();
		fed.insert(hole.clone());
		for (run, given) in self.drops.overlapping(&hole) {
			if given.stale_budget().is_some() {
				let run = run.start.max(hole.start)..run.end.min(hole.end);
				fed.remove(run.clone());
				unfed.insert(run);
			}
		}
		for hole in fed.iter() {
			let mut from = hole.start;
			for arrived in self.arrivals.accounted(hole.clone()) {
				if from < arrived.start {
					gap(from..arrived.start, &mut unfed);
				}
				from = arrived.end;
				unfed.insert(arrived);
			}
			if from < hole.end {
				gap(from..hole.end, &mut unfed);
			}
		}
		(unfed, awaited)
	}
}

/// The runs of `run` the cache holds as backfill: servable, but out of the arrival walk.
/// Where a reader's inbox starts taking evictions: no lower than what it handed out every
/// group below, since an evicted group it took is never a miss again. A live reader that
/// reads by arrival never raises its floor, so without this every old group the cache
/// evicts would land with it and count as an eviction to judge again.
fn inbox_floor(low: u64, taken: &Delivered) -> u64 {
	low.max(taken.floor())
}

fn backfill(state: &TrackState, run: ops::Range<u64>) -> RangeSet<u64> {
	state
		.lookup
		.range(run)
		.filter(|(_, slot)| !slot.is_walked() && slot.serves_end())
		.map(|(&key, _)| key..key + 1)
		.collect()
}

/// The runs of `run` the cache holds nothing servable for.
pub(super) fn holes(state: &TrackState, run: ops::Range<u64>) -> Vec<ops::Range<u64>> {
	let mut holes = Vec::new();
	let mut from = run.start;
	for (&key, slot) in state.lookup.range(run.clone()) {
		if !slot.serves_end() {
			continue;
		}
		if from < key {
			holes.push(from..key);
		}
		from = key + 1;
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

/// The misses a reader gave up, billed with its record and forgotten where it folds: it
/// settled those keys, so never asks a fill for them again.
impl super::super::record::Beside for Misses {
	fn runs(&self) -> u64 {
		(self.given_up.len() + self.skipped.len() + self.stale.len()) as u64
	}

	fn forget(&mut self, folded: ops::Range<u64>) {
		if !folded.is_empty() {
			self.given_up.remove(folded.clone());
			self.skipped.remove(folded.clone());
			self.stale.remove(folded);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::super::{Delay, Servable, Subscription};
	use super::*;

	/// A refusal starts a deferred grace after the judge returned: either no handler
	/// could take the fill, or one refused it later. Both must arm the gap's own deadline.
	#[test]
	fn a_refused_fill_wakes_at_the_live_gaps_deadline() {
		use std::sync::atomic::{AtomicBool, Ordering};

		#[derive(Default)]
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::Relaxed);
			}
		}

		for handled in [false, true] {
			let broadcast = crate::broadcast::Info::default().produce();
			let track = broadcast.create_track("grace", crate::track::Info::default()).unwrap();
			let pool = &broadcast.info().pool;
			let opened = track.consume().now();
			let noop = kio::Waiter::noop();
			pool.poll(opened, &noop);
			let budget = Duration::from_secs(1);
			let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
			for key in [0, 2] {
				track
					.create_group(crate::group::Info { start: key })
					.unwrap()
					.finish()
					.unwrap();
			}
			let requests = handled.then(|| track.requests());
			let state = track.state.consume();
			let fills = state.read().fills.clone();
			let mut misses = Misses::default();
			let mut given_up = Vec::new();
			misses.ask(
				&state,
				&fills,
				&RangeSet::from_iter(std::iter::once(1..2)),
				&Asking {
					priority: 0,
					max_delay: Some(budget),
					order: Order::Ascending,
				},
				&mut given_up,
			);
			let flag = Arc::new(Flag::default());
			let waiter = kio::Waiter::new(flag.clone().into());
			assert!(misses.resolve(&state, Some(budget), &mut given_up, &waiter).is_empty());
			if let Some(requests) = requests {
				pool.poll(opened.saturating_add(budget / 2), &noop);
				let Poll::Ready(Ok(fill)) = requests.poll_requested_fill_group(&noop) else {
					panic!("the queued fill");
				};
				fill.reject(Error::NotFound);
				assert!(misses.resolve(&state, Some(budget), &mut given_up, &waiter).is_empty());
			}
			assert!(misses.refused.contains(&1));
			assert!(given_up.is_empty(), "the live feed may still deliver the group");
			flag.0.store(false, Ordering::Relaxed);
			let deadline = opened.saturating_add(budget);
			assert_eq!(
				pool.poll(deadline.saturating_sub(Duration::from_micros(1)), &noop),
				Some(deadline)
			);
			assert!(!flag.0.load(Ordering::Relaxed));
			pool.poll(deadline, &noop);
			assert!(
				flag.0.load(Ordering::Relaxed),
				"handled={handled}: the refusal's grace wakes the reader"
			);
		}
	}

	#[kio::test]
	async fn a_recreated_accounted_group_clears_its_earlier_fill_refusal() {
		let broadcast = crate::broadcast::Info::default().produce();
		let mut track = broadcast
			.create_track("recreated", crate::track::Info::default())
			.unwrap();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
		// Newest first, so the publisher skips nothing below it: group 2 is a live gap a fill
		// may answer.
		for key in [4, 0] {
			track
				.create_group(crate::group::Info { start: key })
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
			coverage: None,
		};
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
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
			.create_group(crate::group::Info { start: 2 })
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
			.create_group(crate::group::Info { start: 2 })
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
		let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
		// Newest first, so the publisher skips nothing below it: group 2 is a live gap a fill
		// may answer.
		for key in [4, 0] {
			track
				.create_group(crate::group::Info { start: key })
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
			coverage: None,
		};
		let asks = misses.judge(
			&state,
			(0, 0, None),
			None,
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
			.create_group(crate::group::Info { start: 2 })
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
			None,
			|_| panic!("the edge has not moved"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(!asks.contains(&2), "a refused hole is not asked again");
		assert_eq!(
			given_up,
			vec![(2..3, Hole::missed(None))],
			"a refusal is no stale verdict"
		);
	}
	#[kio::test]
	async fn unchanged_probes_and_late_arrivals_keep_other_gap_deadlines() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast.create_track("sparse", crate::track::Info::default()).unwrap();
		// A received copy, whose live gaps are filled or waited for, not skipped.
		track.received();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
		for key in [0, 2, 4] {
			track
				.create_group(crate::group::Info { start: key })
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
			coverage: None,
		};
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
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
			.create_group(crate::group::Info { start: 1 })
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
			None,
			|_| panic!("a due gap extends no span"),
			&Delivered::default(),
			&drift,
			&mut given_up,
			&waiter,
		);
		assert!(asks.is_empty(), "the successor has aged past this reader's budget too");
		assert_eq!(
			given_up,
			vec![(3..4, Hole::stale(drift.budget))],
			"judged stale under the reader's budget"
		);
		assert!(misses.deferred.runs.is_empty());
	}

	/// A grace change with nothing deferred has no deadline to move, so an unchanged probe
	/// skips it; the next gap is still timed by the new grace.
	#[kio::test]
	async fn a_grace_change_with_nothing_deferred_times_the_next_gap() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast
			.create_track("grace", crate::track::Info::default().with_max_age(None))
			.unwrap();
		// A received copy, whose live gaps are filled or waited for, not skipped.
		track.received();
		let budget = Duration::from_secs(5);
		let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
		for key in 0..3 {
			track
				.create_group(crate::group::Info { start: key })
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
			coverage: None,
		};
		let full = |span| [span].into_iter().collect();
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
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
		let _wide = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(wider)));
		let state = track.state.read();
		assert_eq!(state.arrival_grace(), wider);
		assert!(
			misses
				.judge(
					&state,
					(0, 0, None),
					None,
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
			.create_group(crate::group::Info { start: 4 })
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
			Some(opened.saturating_add(wider)),
			"timed by the new grace"
		);
	}

	/// Judging the stretches a fill still owes by their shared successor gives up exactly
	/// what judging each stretch alone would, as groups arrive, start, abort, or leave the
	/// cache in any order, on either clock, against a window of one run or several.
	#[test]
	fn expiring_by_successor_matches_each_stretch_judged_alone() {
		use super::super::super::subscription::Cover;
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
					.map(|(&key, _)| key);
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
		const KEYS: u64 = 32;
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
			let runs = |count: u64| -> Vec<_> {
				(0..1 + next(count))
					.map(|_| {
						let start = next(KEYS);
						start..=start + next(6)
					})
					.collect()
			};
			let servable = match next(3) {
				0 => Servable::ALL,
				1 => Servable::Run {
					start: next(8),
					end: Some(KEYS - next(8)),
				},
				_ => Servable::covering(
					Cover::new(runs(6).into_iter().chain(runs(2))),
					(next(3) == 0).then(|| next(KEYS + 2)),
					&Default::default(),
				),
			};
			let mut misses = Misses::default();
			for _ in 0..1 + next(6) {
				let start = next(KEYS);
				misses.wanted.insert(start..start + 1 + next(6));
			}
			for step in 0..12 {
				let key = next(KEYS);
				match next(6) {
					0..=2 => {
						if let Ok(mut group) = track.create_group(crate::group::Info { start: key }) {
							if next(4) > 0 {
								group
									.write_frame(at(key * 20), bytes::Bytes::from_static(b"x"))
									.unwrap();
							}
							groups.push(group);
						}
					}
					3 => {
						if let Some(index) = groups.iter().position(|group| group.start == key) {
							let _ = groups.swap_remove(index).abort(Error::Cancel);
						}
					}
					4 => {
						let _ = track.modify().unwrap().uncache(key);
					}
					_ => broadcast.info().pool.step(Duration::from_millis(next(120))),
				}
				let state = track.state.read();
				let drift = Drift {
					budget,
					bounded: false,
					edge: state.drift_edge(&servable),
					coverage: None,
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

	/// A window edited from the one before is followed by what the edit moved over, and ends
	/// up judging and holding exactly what judging the whole window again would: the second
	/// reader below forgets its last window before each move, so it always judges the window
	/// whole.
	#[kio::test]
	async fn a_moved_window_is_judged_as_the_whole_window_would_be() {
		use super::super::super::subscription::Cover;

		let seed = std::cell::Cell::new(0x2545_f491_4f6c_dd1d_u64);
		let next = |bound: u64| {
			let mut x = seed.get();
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			seed.set(x);
			x % bound
		};
		const KEYS: u64 = 40;
		let runs = |count: u64| -> Vec<_> {
			(0..next(count + 1))
				.map(|_| {
					let start = next(KEYS);
					start..=start + next(5)
				})
				.collect()
		};
		let snapshot = |misses: &Misses| {
			let sets = [&misses.wanted, &misses.judged, &misses.refused, &misses.given_up]
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
			let _reader = track.subscribe(Subscription::default().with_delay(Delay::default().with_max(budget)));
			// A feed with holes, some of them under where it starts, created newest first so the
			// publisher skips none of them.
			let keys: Vec<_> = (0..KEYS).filter(|_| next(3) > 0).collect();
			for &key in keys.iter().rev() {
				track
					.create_group(crate::group::Info { start: key })
					.unwrap()
					.finish()
					.unwrap();
			}
			if next(2) == 0 {
				track.start_at(next(KEYS / 2)).unwrap();
			}
			let fills = track.state.read().fills.clone();
			fills.lock().add_handler();
			let fixed = runs(6);
			let mut shared = Cover::new(fixed.clone());
			let mut moving: Vec<RangeInclusive<u64>> = Vec::new();
			let cap = (next(3) == 0).then(|| next(KEYS + 4));
			let asking = Asking {
				priority: 0,
				max_delay: Some(budget),
				order: Order::Ascending,
			};
			let (mut moved, mut whole) = (Misses::default(), Misses::default());
			let (mut given_moved, mut given_whole) = (Vec::new(), Vec::new());
			for revision in 0..8 {
				let layer = runs(3);
				for run in std::mem::replace(&mut moving, layer.clone()) {
					shared.add(run, -1);
				}
				for run in &layer {
					shared.add(run.clone(), 1);
				}
				let unshared = Cover::new(fixed.iter().chain(&layer).cloned());
				(whole.view, whole.clipped) = (None, None);
				let id = (0, revision, cap);
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
					misses.retain(id, Some(view), || target(0..=u64::MAX - 1));
					let state = track.state.read();
					let drift = Drift {
						budget,
						bounded: false,
						edge: state.drift_edge(&Servable::ALL),
						coverage: None,
					};
					let asks = misses.judge(
						&state,
						id,
						Some(view),
						None,
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
					"round {round} revision {revision}: {fixed:?} under {moving:?}",
				);
				assert_eq!(given_moved, given_whole, "round {round} revision {revision}");
			}
		}
		assert!(asked > 100, "only {asked} runs asked");
	}

	/// A reader waiting on many fills looks only at the ones that answered, yet each still
	/// wakes it, and wakes the task that read it last: a reader read by another task since
	/// parked learns of the answer too. The read resolves through its idle gate, which wakes
	/// only the task that parked it last.
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
		let mut track = broadcast
			.create_track("fills", crate::track::Info::default().with_max_age(None))
			.unwrap();
		let requests = track.requests();
		track.start_at(8).unwrap();
		// Every range starts at a group held, so each knows where it starts without a lookup.
		for key in [0, 2, 4, 6, 8] {
			track
				.create_group(crate::group::Info { start: key })
				.unwrap()
				.finish()
				.unwrap();
		}
		// Four groups below where the feed starts, 1, 3, 5, and 7, each a fill of its own.
		let ranges = [0..2, 2..4, 4..6, 6..8].map(crate::track::Range::groups);
		let mut reader = track.subscribe(
			Subscription::default()
				.with_delay(Delay::default().with_max(Duration::from_secs(60)))
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
			misses.answered.ids.lock().unwrap().is_empty(),
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
		let answered = cursor.misses.as_ref().unwrap().answered.ids.lock().unwrap().len();
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

	/// A read parked on a fill wakes once it answers, though a drop poll resolved with a
	/// waiter of its own after the read parked: the read then sees the fill give up its
	/// group and ends with the track.
	#[kio::test]
	async fn an_answered_fill_wakes_a_read_a_drop_poll_resolved_after() {
		use std::sync::atomic::{AtomicUsize, Ordering};

		#[derive(Default)]
		struct Count(AtomicUsize);
		impl Wake for Count {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}

		let broadcast = crate::broadcast::Info::default().produce();
		let mut track = broadcast
			.create_track("fills", crate::track::Info::default().with_max_age(None))
			.unwrap();
		let requests = track.requests();
		track.start_at(8).unwrap();
		track
			.create_group(crate::group::Info { start: 8 })
			.unwrap()
			.finish()
			.unwrap();
		let track = track.finish().unwrap();
		// Group 0 lies below where the feed starts: only a fill may deliver it.
		let ranges = [crate::track::Range::groups(0..1), crate::track::Range::groups(8..)];
		let mut reader = track.subscribe(
			Subscription::default()
				.with_delay(Delay::default().with_max(Duration::from_secs(60)))
				.with_ranges(ranges),
		);
		let (read, drops) = (Arc::new(Count::default()), Arc::new(Count::default()));
		let read_waiter = kio::Waiter::new(Waker::from(read.clone()));
		let drops_waiter = kio::Waiter::new(Waker::from(drops.clone()));
		let mut handed = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&read_waiter) {
			handed.push(group.start);
		}
		assert_eq!(handed, [8]);
		assert!(
			reader.poll_recv_group(&read_waiter).is_pending(),
			"the read waits on the fill for group 0"
		);
		assert!(reader.poll_drop(&drops_waiter).is_pending(), "nothing given up yet");
		assert!(
			reader.poll_drop(&kio::Waiter::noop()).is_pending(),
			"a no-op poll leaves the real registrations in place"
		);

		let fill = std::future::poll_fn(|cx| requests.poll_requested_fill(&kio::Waiter::new(cx.waker().clone())))
			.await
			.unwrap();
		let (read_woken, drops_woken) = (read.0.load(Ordering::Relaxed), drops.0.load(Ordering::Relaxed));
		fill.drop_groups(fill.start().group..fill.end());
		assert!(read.0.load(Ordering::Relaxed) > read_woken, "the parked read wakes");
		assert!(drops.0.load(Ordering::Relaxed) > drops_woken, "the drop poll wakes");

		// The drop poll takes the answer first, as a publisher forwarding drops may.
		assert!(matches!(reader.poll_drop(&drops_waiter), Poll::Ready((groups, _)) if groups == (0..1)));
		assert!(
			matches!(reader.poll_recv_group(&read_waiter), Poll::Ready(Ok(None))),
			"the read ends once the fill gave up the last group it owed"
		);
	}

	/// The forget log stays a few entries, and a reader behind its coalesced entries judges a
	/// span covering every run it missed, while a current one judges only what is new.
	#[test]
	fn the_forget_log_coalesces_into_spans_covering_what_a_reader_missed() {
		let mut forgotten = Forgotten::default();
		for run in 0..10u64 {
			forgotten.record(10 * run..10 * run + 1);
		}
		assert_eq!(forgotten.runs.len(), FORGOTTEN_KEPT);
		let (runs, revision) = forgotten.since(0);
		let mut missed = RangeSet::new();
		for run in runs {
			missed.insert(run);
		}
		for run in 0..10u64 {
			assert!(missed.contains(&(10 * run)), "run {run} is judged again");
		}
		assert_eq!(revision, 10);
		let (runs, _) = forgotten.since(9);
		let runs: Vec<_> = runs.collect();
		assert_eq!(runs.len(), 1);
		assert_eq!(runs[0], 90..91);
		assert_eq!(forgotten.since(10).0.count(), 0);
	}

	#[test]
	fn deferred_splits_and_replacements_retire_the_old_age_entries() {
		let mut deferred = Deferred::default();
		let early = crate::time::Instant::ZERO.saturating_add(Duration::from_secs(1));
		let late = early.saturating_add(Duration::from_secs(1));
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

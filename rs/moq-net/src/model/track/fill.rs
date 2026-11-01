//! Fills: the groups readers miss, asked of a [`Requests`](super::Requests) handler by range.
//!
//! A miss is a sequence a reader wants that the cache doesn't hold and the live feed will
//! never deliver. A reader joins the fill already covering it, or queues a new one, and a
//! handler serves each fill it takes as one bounded run: [`Fill::accept`] for each group it
//! has, [`Fill::drop_groups`] for the rest.
//!
//! A fill serves under what its readers ask now: a reader changing its priority, budget, or
//! order moves every fill it waits on, queued or taken. Only the first group's start frame
//! stays as the handler took it, since a handler may have asked its source for it already;
//! a reader wanting earlier frames of that group asks a fill of its own.

use std::{
	collections::{BTreeMap, HashMap, VecDeque, btree_map},
	ops,
	sync::Arc,
	task::Poll,
	time::Duration,
};

use rangemap::{RangeMap, RangeSet};

use super::{Info, Order, Position, TrackState, clamp_max_delay};
use crate::{Error, Result, StreamError, group, util::SendCell};

/// The fills a track's readers asked for, queued or taken by a handler, under one lock.
#[derive(Default)]
pub(in crate::model) struct FillState {
	// Live `Requests` handles: a fill queues only while one can take it.
	handlers: usize,
	next: u64,
	// Every fill not yet ended, queued or taken, by id.
	pending: HashMap<u64, Pending>,
	// The queued subset, oldest first.
	queue: VecDeque<u64>,
	// The fill each unresolved sequence waits on, so a second reader joins it rather than
	// asking again.
	covers: RangeMap<u64, u64>,
}

/// One fill until it ends.
struct Pending {
	start: Position,
	end: u64,
	// Exclusive frame cap in the last group; None asks through the original end.
	frame_end: Option<u64>,
	// What its joins ask with now, shared with them and its handler.
	asks: kio::Shared<Asks>,
	queued: bool,
	// How many joins still read each sequence: one a reader narrowed away is no longer owed.
	wants: RangeMap<u64, u32>,
	// Readers hold consumers, so it counts the fill's demand; the handler's copy says what
	// became of each sequence.
	result: kio::Producer<Outcome>,
}

impl Pending {
	/// Count one more join reading `groups`.
	fn want(&mut self, groups: ops::Range<u64>) {
		let mut counts: Vec<(ops::Range<u64>, u32)> = self
			.wants
			.overlapping(&groups)
			.map(|(run, count)| (run.start.max(groups.start)..run.end.min(groups.end), count + 1))
			.collect();
		counts.extend(self.wants.gaps(&groups).map(|gap| (gap, 1)));
		for (run, count) in counts {
			self.wants.insert(run, count);
		}
	}

	/// Count one join out of reading `groups`, which it counted in. Returns the runs nobody
	/// reads any more.
	fn unwant(&mut self, groups: ops::Range<u64>) -> Vec<ops::Range<u64>> {
		let counts: Vec<(ops::Range<u64>, u32)> = self
			.wants
			.overlapping(&groups)
			.map(|(run, count)| (run.start.max(groups.start)..run.end.min(groups.end), count - 1))
			.collect();
		let mut unwanted = Vec::new();
		for (run, count) in counts {
			match count {
				0 => {
					self.wants.remove(run.clone());
					unwanted.push(run);
				}
				count => self.wants.insert(run, count),
			}
		}
		unwanted
	}

	/// The runs of `span` a join still reads.
	fn wanted(&self, span: &ops::Range<u64>) -> RangeSet<u64> {
		self.wants
			.overlapping(span)
			.map(|(run, _)| run.start.max(span.start)..run.end.min(span.end))
			.collect()
	}
}

/// What a reader asks its fills with, which it may change while they are out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Asking {
	pub priority: i8,
	pub max_delay: Option<Duration>,
	pub order: Order,
}

/// What a fill's joins ask with, counted so a join arriving, leaving, or changing its mind
/// costs its own entry rather than a pass over every join.
#[derive(Default)]
struct Asks {
	priorities: BTreeMap<i8, usize>,
	budgets: BTreeMap<Duration, usize>,
	// Joins with no budget of their own: only the publisher's max age, which none relaxes.
	unbounded: usize,
	descending: usize,
}

impl Asks {
	fn add(&mut self, asking: &Asking) {
		*self.priorities.entry(asking.priority).or_default() += 1;
		match asking.max_delay {
			Some(budget) => *self.budgets.entry(budget).or_default() += 1,
			None => self.unbounded += 1,
		}
		if asking.order == Order::Descending {
			self.descending += 1;
		}
	}

	fn remove(&mut self, asking: &Asking) {
		fn take<K: Ord>(counts: &mut BTreeMap<K, usize>, key: K) {
			let btree_map::Entry::Occupied(mut entry) = counts.entry(key) else {
				unreachable!("only a counted ask is removed");
			};
			*entry.get_mut() -= 1;
			if *entry.get() == 0 {
				entry.remove();
			}
		}
		take(&mut self.priorities, asking.priority);
		match asking.max_delay {
			Some(budget) => take(&mut self.budgets, budget),
			None => self.unbounded -= 1,
		}
		if asking.order == Order::Descending {
			self.descending -= 1;
		}
	}

	/// The most urgent priority asked.
	fn priority(&self) -> i8 {
		self.priorities.last_key_value().map_or(0, |(priority, _)| *priority)
	}

	/// The most tolerant budget asked: `None` is the publisher's max age alone, which no
	/// explicit budget relaxes.
	fn max_delay(&self) -> Option<Duration> {
		match self.unbounded {
			0 => self.budgets.last_key_value().map(|(budget, _)| *budget),
			_ => None,
		}
	}

	/// Newest first once any join asks for it, as any descending subscriber makes a
	/// subscription.
	fn order(&self) -> Order {
		match self.descending {
			0 => Order::Ascending,
			_ => Order::Descending,
		}
	}
}

/// What became of a fill's sequences, shared with every reader that joined it.
#[derive(Default)]
pub(crate) struct Outcome {
	accepted: RangeSet<u64>,
	// Why each refused run was refused; refusing a sequence again replaces why.
	refused: RangeMap<u64, Refusal>,
	refusals: u64,
	// Accepted or refused, kept as answers arrive so a fill answered group by group costs
	// the runs each answer touches, not every answer before it.
	resolved: RangeSet<u64>,
	// Each accepted copy short of the whole group, which a later copy may displace from the
	// cache before a reader takes it. A whole copy covers every other, so none displaces it.
	partial: BTreeMap<u64, group::Handle>,
}

impl Outcome {
	/// Why `sequence` was refused, if it was, and the budget its source was asked under.
	pub(crate) fn refused(&self, sequence: u64) -> Option<(&Error, Option<Duration>)> {
		self.refused.get(&sequence).map(|refusal| (&refusal.err, refusal.asked))
	}

	pub(super) fn is_accepted(&self, sequence: u64) -> bool {
		self.accepted.contains(&sequence)
	}

	/// The partial copy accepted for `sequence`, if it still holds `frames`.
	pub(super) fn partial(&self, sequence: u64, frames: group::Bounds) -> Option<group::Handle> {
		self.partial.get(&sequence).filter(|copy| copy.covers(frames)).cloned()
	}

	/// What was answered within `wanted`: accepted, or refused with why and the budget
	/// the fill asked with.
	#[expect(
		clippy::type_complexity,
		reason = "each answer pairs a run with its outcome; naming it would only move the type"
	)]
	pub(super) fn within(
		&self,
		wanted: &RangeSet<u64>,
	) -> Vec<(ops::Range<u64>, std::result::Result<(), (Error, Option<Duration>)>)> {
		let mut answers = Vec::new();
		for run in wanted.iter() {
			for accepted in self.accepted.overlapping(run) {
				answers.push((accepted.start.max(run.start)..accepted.end.min(run.end), Ok(())));
			}
			for (refused, refusal) in self.refused.overlapping(run) {
				let groups = refused.start.max(run.start)..refused.end.min(run.end);
				answers.push((groups, Err((refusal.err.clone(), refusal.asked))));
			}
		}
		answers
	}

	fn accept(&mut self, groups: ops::Range<u64>) {
		self.accepted.insert(groups.clone());
		self.resolved.insert(groups);
	}

	fn refuse(&mut self, groups: ops::Range<u64>, err: Error, asked: Option<Duration>) {
		if groups.is_empty() {
			return;
		}
		let id = self.refusals;
		self.refusals += 1;
		self.refused.insert(groups.clone(), Refusal { id, err, asked });
		self.resolved.insert(groups);
	}
}

/// One refusal, equal only to itself: the map keeps a run it refused whole, but never merges
/// two refusals whose errors may differ.
#[derive(Clone)]
struct Refusal {
	id: u64,
	err: Error,
	// The budget its source was asked under, so a looser reader can tell that an
	// [`Error::Old`] was not its own.
	asked: Option<Duration>,
}

impl PartialEq for Refusal {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}

impl Eq for Refusal {}

/// A reader's request: the run `start.group..end`, from `start.frame` in its first group.
pub(super) struct Ask {
	pub start: Position,
	pub end: u64,
	pub frame_end: Option<u64>,
	pub asking: Asking,
}

/// A reader's place in one fill: the part of it the reader wants.
pub(super) struct Join {
	fills: kio::Shared<FillState>,
	id: u64,
	/// The sequences of the fill this reader asked for.
	pub groups: ops::Range<u64>,
	// The part of `groups` it still reads, counted in the fill's wants.
	held: RangeSet<u64>,
	// Counts this reader in the fill's demand; `None` only while dropping.
	outcome: Option<kio::Consumer<Outcome>>,
	attempt: kio::ProducerWeak<Outcome>,
	// What this reader asks the fill with, counted in its asks.
	asking: Asking,
	asks: kio::Shared<Asks>,
}

impl Join {
	pub(super) fn outcome(&self) -> &kio::Consumer<Outcome> {
		self.outcome.as_ref().expect("held until dropped")
	}

	/// Ask the fill with `asking` from now on, queued or taken.
	pub(super) fn update(&mut self, asking: &Asking) {
		if self.asking == *asking {
			return;
		}
		let mut asks = self.asks.lock();
		asks.remove(&self.asking);
		asks.add(asking);
		self.asking = *asking;
	}
}

impl Drop for Join {
	fn drop(&mut self) {
		self.asks.lock().remove(&self.asking);
		// Joins take the lock, so leaving under it is atomic with one: a joiner either keeps
		// the fill wanted, or finds it withdrawn and asks afresh rather than joining a fill
		// its handler is about to drop.
		let mut fills = self.fills.lock();
		drop(self.outcome.take());
		if !fills.pending.get(&self.id).is_some_and(|pending| self.is_ours(pending)) {
			return;
		}
		for run in std::mem::take(&mut self.held).iter() {
			fills.unwant(self.id, run.clone());
		}
		if !self.attempt.wanted() {
			fills.withdraw(self.id);
		}
	}
}

impl Join {
	fn is_ours(&self, pending: &Pending) -> bool {
		pending.result.weak().same_channel(&self.attempt)
	}

	/// Whether the reader holds none of the fill any more.
	pub(super) fn is_empty(&self) -> bool {
		self.held.is_empty()
	}

	/// Stop reading `lost`, so the fill no longer owes this reader any of it.
	pub(super) fn release(&mut self, lost: &RangeSet<u64>) {
		let released: Vec<ops::Range<u64>> = lost
			.overlapping(&self.groups)
			.flat_map(|run| {
				self.held
					.overlapping(run)
					.map(move |held| held.start.max(run.start)..held.end.min(run.end))
			})
			.collect();
		if released.is_empty() {
			return;
		}
		for run in &released {
			self.held.remove(run.clone());
		}
		let mut fills = self.fills.lock();
		if !fills.pending.get(&self.id).is_some_and(|pending| self.is_ours(pending)) {
			return;
		}
		for run in released {
			fills.unwant(self.id, run);
		}
	}
}

impl FillState {
	pub(super) fn add_handler(&mut self) {
		self.handlers += 1;
	}

	/// Count a handler out. The last one leaving drops what it never took, so every reader
	/// waiting on those fills finds them gone; a fill already taken stays with its handler.
	pub(super) fn remove_handler(&mut self) {
		self.handlers -= 1;
		if self.handlers > 0 {
			return;
		}
		for id in std::mem::take(&mut self.queue) {
			self.end(id);
		}
	}

	pub(super) fn has_handlers(&self) -> bool {
		self.handlers > 0
	}

	pub(super) fn has_queued(&self) -> bool {
		!self.queue.is_empty()
	}

	/// Whether no fill is pending, queued or taken.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn is_empty(&self) -> bool {
		self.pending.is_empty() && self.queue.is_empty() && self.covers.is_empty()
	}

	/// Join every fill covering `ask`, queueing a new one for each run none covers. Returns
	/// the joins, and the runs nobody can fill: no handler is alive to take a new fill.
	pub(super) fn join(&mut self, shared: &kio::Shared<FillState>, ask: &Ask) -> (Vec<Join>, Vec<ops::Range<u64>>) {
		let span = ask.start.group..ask.end;
		let mut joins = Vec::new();
		let mut unfillable = Vec::new();
		if span.is_empty() {
			return (joins, unfillable);
		}
		let covered: Vec<(ops::Range<u64>, u64)> = self
			.covers
			.overlapping(&span)
			.map(|(groups, id)| (groups.start.max(span.start)..groups.end.min(span.end), *id))
			.collect();
		let mut gaps: Vec<ops::Range<u64>> = self.covers.gaps(&span).collect();
		for (mut groups, id) in covered {
			let pending = self
				.pending
				.get_mut(&id)
				.expect("a covered sequence names a pending fill");
			if groups.start == pending.start.group {
				// A reader starting in an earlier group wants this one whole.
				let frame = match ask.start.group == groups.start {
					true => ask.start.frame,
					false => 0,
				};
				if frame < pending.start.frame {
					match pending.queued {
						true => pending.start.frame = frame,
						// Taken: its handler may have asked its source from the later frame
						// already, so the earlier frames are a fill of their own.
						false => {
							gaps.push(groups.start..groups.start + 1);
							groups.start += 1;
						}
					}
				}
			}
			if !groups.is_empty() && groups.end == pending.end {
				let end = if groups.end == ask.end { ask.frame_end } else { None };
				if pending.frame_end.is_some_and(|cap| end.is_none_or(|end| end > cap)) {
					if pending.queued {
						pending.frame_end = end;
					} else {
						// The source already saw the cap: the missing suffix asks for its own copy.
						gaps.push(groups.end - 1..groups.end);
						groups.end -= 1;
					}
				}
			}
			if groups.is_empty() {
				continue;
			}
			pending.want(groups.clone());
			pending.asks.lock().add(&ask.asking);
			joins.push(Join {
				fills: shared.clone(),
				id,
				held: RangeSet::from_iter([groups.clone()]),
				groups,
				outcome: Some(pending.result.consume()),
				attempt: pending.result.weak(),
				asking: ask.asking,
				asks: pending.asks.clone(),
			});
		}
		// Queued in the reader's order, so a descending reader's newest fragment goes first.
		gaps.sort_unstable_by_key(|gap| gap.start);
		if ask.asking.order == Order::Descending {
			gaps.reverse();
		}
		for groups in gaps {
			if self.handlers == 0 {
				unfillable.push(groups);
				continue;
			}
			let id = self.next;
			self.next += 1;
			let start = match groups.start == ask.start.group {
				true => ask.start,
				false => Position::group(groups.start),
			};
			let result = kio::Producer::<Outcome>::default();
			let mut asks = Asks::default();
			asks.add(&ask.asking);
			let asks = kio::Shared::new(asks);
			joins.push(Join {
				fills: shared.clone(),
				id,
				groups: groups.clone(),
				held: RangeSet::from_iter([groups.clone()]),
				outcome: Some(result.consume()),
				attempt: result.weak(),
				asking: ask.asking,
				asks: asks.clone(),
			});
			// A taken fill's first group split off above keeps that fill, routed here instead.
			self.covers.insert(groups.clone(), id);
			self.pending.insert(
				id,
				Pending {
					start,
					end: groups.end,
					frame_end: if groups.end == ask.end { ask.frame_end } else { None },
					asks,
					queued: true,
					wants: RangeMap::from_iter([(groups.clone(), 1)]),
					result,
				},
			);
			self.queue.push_back(id);
		}
		(joins, unfillable)
	}

	/// Count a join out of reading `groups` of fill `id`. What nobody reads any more is no
	/// longer routed to it: a reader wanting it later asks afresh rather than joining a fetch
	/// being cancelled, so a sequence a fill stops owing stays unowed. Its answers are
	/// forgotten too, so a wide fill holds the answers its readers have yet to take, not one
	/// per group its source answered.
	fn unwant(&mut self, id: u64, groups: ops::Range<u64>) {
		let Some(pending) = self.pending.get_mut(&id) else {
			return;
		};
		let unwanted = pending.unwant(groups);
		let answered = {
			let outcome = pending.result.read();
			unwanted
				.iter()
				.any(|run| outcome.refused.overlaps(run) || outcome.accepted.overlaps(run))
		};
		if answered && let Ok(mut outcome) = pending.result.write() {
			for run in &unwanted {
				outcome.refused.remove(run.clone());
				outcome.accepted.remove(run.clone());
			}
		}
		for run in unwanted {
			for range in self.routed(id, &run) {
				self.covers.remove(range);
			}
		}
	}

	/// Whether a join of fill `id` still reads `sequence`.
	fn wants(&self, id: u64, sequence: u64) -> bool {
		self.pending
			.get(&id)
			.is_some_and(|pending| pending.wants.contains_key(&sequence))
	}

	/// Whether a join of fill `id` still reads any of `groups`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn wants_any(&self, id: u64, groups: &ops::Range<u64>) -> bool {
		self.pending
			.get(&id)
			.is_some_and(|pending| pending.wants.overlaps(groups))
	}

	/// Take the oldest queued fill, still pending (joinable) until it ends.
	fn pop(&mut self) -> Option<u64> {
		let id = self.queue.pop_front()?;
		let pending = self.pending.get_mut(&id).expect("a queued fill is pending");
		pending.queued = false;
		Some(id)
	}

	/// Drop fill `id`: nobody wants it, so it records nothing.
	fn withdraw(&mut self, id: u64) {
		let Some(pending) = self.pending.get(&id) else {
			return;
		};
		if pending.queued {
			self.queue.retain(|queued| *queued != id);
		}
		self.end(id);
	}

	/// The parts of `groups` routed to fill `id`.
	fn routed(&self, id: u64, groups: &ops::Range<u64>) -> Vec<ops::Range<u64>> {
		self.covers
			.overlapping(groups)
			.filter(|(_, covering)| **covering == id)
			.map(|(range, _)| range.start.max(groups.start)..range.end.min(groups.end))
			.collect()
	}

	/// Forget fill `id`, closing it for its readers.
	fn end(&mut self, id: u64) {
		let Some(pending) = self.pending.remove(&id) else {
			return;
		};
		for range in self.routed(id, &(pending.start.group..pending.end)) {
			self.covers.remove(range);
		}
	}
}

/// Stop routing readers wanting `groups` to fill `id`: they were answered. Reads first, so
/// an answer that changes nothing wakes no handler parked on the queue.
fn uncover(fills: &kio::Shared<FillState>, id: u64, groups: ops::Range<u64>) {
	let mut fills = fills.lock();
	let routed = fills.routed(id, &groups);
	for range in routed {
		fills.covers.remove(range);
	}
}

/// Pop the next queued fill, or surface the track's abort once none is queued.
pub(super) fn poll_requested(
	state: &kio::Producer<TrackState>,
	fills: &kio::Shared<FillState>,
	waiter: &kio::Waiter,
) -> Poll<Result<Fill>> {
	// Prefer serving a queued fill, even if the track has since aborted.
	if let Poll::Ready(mut guard) = fills.poll(waiter, |fills| match fills.has_queued() {
		true => Poll::Ready(()),
		false => Poll::Pending,
	}) {
		let id = guard.pop().expect("predicate guaranteed a fill");
		let pending = guard.pending.get(&id).expect("popped fill is pending");
		let (start, end, frame_end) = (pending.start, pending.end, pending.frame_end);
		let (asks, result) = (pending.asks.clone(), pending.result.clone());
		drop(guard);
		// Built with the fills unlocked, so no other lock nests under theirs.
		let inner = Inner {
			state: state.clone(),
			fills: fills.clone(),
			id,
			start,
			end,
			frame_end,
			asks,
			result,
		};
		return Poll::Ready(Ok(Fill {
			inner: Arc::new(SendCell::new(inner)),
			prev: None,
		}));
	}

	// No fill queued: surface a track abort so the handler loop can exit.
	match state.poll_ref(waiter, |state| match &state.abort {
		Some(err) => Poll::Ready(err.clone()),
		None => Poll::Pending,
	}) {
		Poll::Ready(Ok(err)) => Poll::Ready(Err(err)),
		Poll::Ready(Err(closed)) => Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
		Poll::Pending => Poll::Pending,
	}
}

/// A run of groups readers miss, for a [`Requests`](super::Requests) handler to serve.
///
/// It names `start.group` up to the exclusive [`end`](Self::end), the first group from
/// [`start`](Self::start)'s frame. Its [`priority`](Self::priority),
/// [`max_delay`](Self::max_delay), and [`order`](Self::order) follow its readers as they
/// change, so read them as each group is served. The handler calls [`accept`](Self::accept) for each group
/// it has and [`drop_groups`](Self::drop_groups) for the rest. Dropping the fill names
/// whatever is left as dropped while a reader still wants it; once nobody does, the fill is
/// withdrawn and records nothing, so stop serving it once [`wanted`](Self::wanted) is `None`.
/// A reader narrowing its ranges may leave a single group, so stop serving that one when
/// [`poll_unwanted`](Self::poll_unwanted) is ready and move on to [`wanted`](Self::wanted).
///
/// A clone serves the same fill, and the fill is settled once the last clone drops. Each
/// clone watches on its own, so its first [`poll_wanted_changed`](Self::poll_wanted_changed)
/// reports the current value.
pub struct Fill {
	// Its track state reaches every group of the track, so the cell proves `Send` once
	// here instead of inside every session machine holding a fill.
	inner: Arc<SendCell<Inner>>,
	// The value `poll_wanted_changed` last returned, or `None` before it returned any.
	prev: Option<Option<u64>>,
}

impl Clone for Fill {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			prev: None,
		}
	}
}

/// The state a [`Fill`] and its per-group handles share. The last one dropping settles what
/// was left.
struct Inner {
	state: kio::Producer<TrackState>,
	fills: kio::Shared<FillState>,
	id: u64,
	start: Position,
	end: u64,
	// Exclusive frame cap in the last group; None asks through the original end.
	frame_end: Option<u64>,
	asks: kio::Shared<Asks>,
	result: kio::Producer<Outcome>,
}

impl Fill {
	/// The first group to fill, and the frame within it to start from.
	pub fn start(&self) -> Position {
		self.inner.start
	}

	/// The frame group `sequence` starts from: [`start`](Self::start)'s frame for the first
	/// group, 0 for the rest.
	pub fn frame_start(&self, sequence: u64) -> u64 {
		self.inner.frame_start(sequence)
	}

	/// The exclusive end group.
	pub fn end(&self) -> u64 {
		self.inner.end
	}

	/// The most urgent delivery priority among the readers waiting on it now.
	pub fn priority(&self) -> i8 {
		self.inner.asks.read().priority()
	}

	/// The most tolerant age budget among the readers waiting on it now, or `None` for the
	/// publisher's max age alone. Each reader still enforces its own.
	pub fn max_delay(&self) -> Option<Duration> {
		self.inner.max_delay()
	}

	/// Which end to fill first now: [`Order::Descending`] is newest first.
	pub fn order(&self) -> Order {
		self.inner.asks.read().order()
	}

	/// Insert group `sequence` into the track's cache, handing it to the readers waiting on
	/// it, and return a [`group::Producer`] to fill.
	///
	/// The group starts at [`start`](Self::start)'s frame when it is the first group, so the
	/// frames written keep the indices they have in the group. `info` sets the track's
	/// [`Info`] if it hasn't been accepted yet (a fetch with no live subscription), and is
	/// ignored otherwise. Errors with [`Error::InvalidRange`] outside the fill,
	/// [`Error::Duplicate`] if the group was already answered or cached, or the track's abort
	/// error.
	pub fn accept(&self, sequence: u64, info: impl Into<Option<Info>>) -> Result<group::Producer> {
		self.inner.accept(sequence, info.into())
	}

	/// Name `groups` as never to be delivered: the source no longer has them. Readers stop
	/// waiting for them, and the track records them so nobody asks again. Only the groups a
	/// reader still reads count; the rest of the fill was never owed.
	pub fn drop_groups(&self, groups: ops::Range<u64>) {
		self.inner.drop_groups(groups, Error::NotFound, self.max_delay());
	}

	/// Refuse `groups` with `err` without saying the source lost them, as when serving them
	/// failed for now or this request asks what the source can't serve. The readers waiting
	/// now stop waiting, and a later reader asks again. Only the groups a reader still reads
	/// count, as with [`Self::drop_groups`].
	pub fn refuse_groups(&self, groups: ops::Range<u64>, err: Error) {
		let owed = self.inner.owed(groups);
		self.inner.refuse(&owed, err, self.max_delay());
	}

	/// Poll until no reader waits on this fill: it is withdrawn, so stop serving it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_withdrawn(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.result.poll_wanted_changed(waiter, true).map(drop)
	}

	/// Whether a reader still waits for group `sequence` from this fill: false once every
	/// reader took it or narrowed past it, or nobody waits on the fill. A group stops being
	/// wanted for good; once accepted and taken, [`group::Producer::demand`](crate::group::Producer::demand)
	/// says who reads it.
	pub fn is_wanted(&self, sequence: u64) -> bool {
		self.inner.is_wanted(sequence)
	}

	/// Wait until no reader waits for group `sequence` from this fill, so stop serving it.
	#[cfg(feature = "async")]
	pub async fn unwanted(&self, sequence: u64) {
		kio::wait(|waiter| self.poll_unwanted(sequence, waiter)).await
	}

	/// Poll until no reader waits for group `sequence` from this fill.
	pub fn poll_unwanted(&self, sequence: u64, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_unwanted(sequence, waiter)
	}

	/// How many answer runs the fill still holds for its readers.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn answers(&self) -> usize {
		let outcome = self.inner.result.read();
		outcome.refused.iter().count() + outcome.accepted.iter().count()
	}

	/// The next group still owed in the fill's order: a reader still reads it, and it was
	/// neither answered nor delivered live since it was asked. `None` once none is owed or
	/// no reader waits on the fill, so stop serving it.
	pub fn wanted(&self) -> Option<u64> {
		if !self.inner.result.wanted() {
			return None;
		}
		loop {
			let wanted = self.inner.wanted();
			let sequence = {
				let outcome = self.inner.result.read();
				let mut gaps = wanted.iter().flat_map(|run| outcome.resolved.gaps(run));
				match self.order() {
					Order::Ascending => gaps.next().map(|gap| gap.start),
					Order::Descending => gaps.last().map(|gap| gap.end - 1),
				}
			}?;
			// The live feed may have delivered it since: answered by the cache, not the source.
			let cached = self
				.inner
				.state
				.read()
				.covering_range(sequence, self.inner.bounds(sequence))
				.is_some();
			if !cached {
				return Some(sequence);
			}
			let Ok(mut outcome) = self.inner.result.write() else {
				return Some(sequence);
			};
			outcome.accept(sequence..sequence + 1);
			drop(outcome);
			uncover(&self.inner.fills, self.inner.id, sequence..sequence + 1);
		}
	}

	/// Return [`wanted`](Self::wanted) once it differs from what this handle last returned:
	/// immediately on the first call, then on each change.
	///
	/// So `while let Some(sequence) = fill.wanted_changed().await` serves each group as it
	/// becomes owed, and ends once the fill is withdrawn or answered.
	#[cfg(feature = "async")]
	pub async fn wanted_changed(&mut self) -> Option<u64> {
		kio::wait(|waiter| self.poll_wanted_changed(waiter)).await
	}

	/// Poll for [`wanted`](Self::wanted) to differ from what this handle last returned.
	pub fn poll_wanted_changed(&mut self, waiter: &kio::Waiter) -> Poll<Option<u64>> {
		// Arm every input of `wanted` before reading it, so a change in between still wakes:
		// the readers leaving, narrowing, or taking a group, the answers, the order, and a
		// group arriving through the live feed, which the cache then answers.
		let _ = self.inner.result.poll_wanted_changed(waiter, true);
		let _ = self.inner.result.poll_ref(waiter, |_| Poll::<()>::Pending);
		let _ = self.inner.state.poll_ref(waiter, |_| Poll::<()>::Pending);
		let _ = self.inner.fills.poll(waiter, |_| Poll::Pending);
		let _ = self.inner.asks.poll(waiter, |_| Poll::Pending);
		let wanted = self.wanted();
		if self.prev == Some(wanted) {
			return Poll::Pending;
		}
		self.prev = Some(wanted);
		Poll::Ready(wanted)
	}

	/// The next run of groups still owed, in the fill's order: [`Self::wanted`]'s group,
	/// extended over the owed groups beside it up to the nearest one the cache holds, so a
	/// session asks its source for the whole run at once.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn next_run(&self) -> Option<ops::Range<u64>> {
		let sequence = self.wanted()?;
		let owed = self.inner.owed_around(sequence);
		let state = self.inner.state.read();
		let held = |(_, slot): &(&u64, &super::Slot)| !slot.is_aborted();
		Some(match self.order() {
			Order::Ascending => {
				let end = state
					.lookup
					.range(sequence + 1..owed.end)
					.find(held)
					.map_or(owed.end, |(held, _)| *held);
				sequence..end
			}
			Order::Descending => {
				let start = state
					.lookup
					.range(owed.start..sequence)
					.rev()
					.find(held)
					.map_or(owed.start, |(held, _)| held + 1);
				start..sequence + 1
			}
		})
	}

	/// The newest group the track's live feed produced, while a feed still writes the track:
	/// its source may still be writing that group, so a fetch including it would cut it short.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn live_group(&self) -> Option<u64> {
		let state = self.inner.state.read();
		match state.feed.is_live() && !state.is_complete() {
			true => state.latest_live(),
			false => None,
		}
	}

	/// Poll until no reader waits for any group in `groups` from this fill.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_unwanted_groups(&self, groups: ops::Range<u64>, waiter: &kio::Waiter) -> Poll<()> {
		if self.poll_withdrawn(waiter).is_ready() {
			return Poll::Ready(());
		}
		let id = self.inner.id;
		self.inner
			.fills
			.poll(waiter, |fills| match fills.wants_any(id, &groups) {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
			.map(drop)
	}

	/// Poll until the track is aborted, with its abort error.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_aborted(&self, waiter: &kio::Waiter) -> Poll<Error> {
		TrackState::poll_aborted(&self.inner.state, waiter)
	}

	/// The track the fill serves.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn track(&self) -> kio::Producer<TrackState> {
		self.inner.state.clone()
	}

	/// The fill's answers, kept open as long as this handle lives.
	#[cfg(all(test, feature = "async"))]
	pub(crate) fn joined(&self) -> kio::Producer<Outcome> {
		self.inner.result.clone()
	}

	/// Refuse `groups` with the source's answer to a request for them: only a lone datagram
	/// group, or [`Error::Old`] under the publisher's max age (with every older group of the
	/// fill), is named dropped.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn reject_groups(&self, groups: ops::Range<u64>, err: Error) {
		self.inner.reject(groups, err, self.max_delay());
	}

	/// Declare the track's exclusive final sequence, as the source answering a request
	/// reported it. A no-op once the track declared an end, or holds a later group.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn finish_track_at(&self, final_sequence: u64) {
		self.inner.finish_track_at(final_sequence);
	}

	/// A handle on group `sequence` alone, for a session serving the fill one group at a time.
	/// It keeps what the readers ask now, which is what its source gets asked.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn group(&self, sequence: u64) -> GroupFill {
		GroupFill {
			inner: self.inner.clone(),
			sequence,
			frame_start: self.inner.frame_start(sequence),
			frame_end: self.inner.bounds(sequence).end,
			priority: self.priority(),
			max_delay: self.max_delay(),
		}
	}
}

impl Inner {
	fn contains(&self, sequence: u64) -> bool {
		(self.start.group..self.end).contains(&sequence)
	}

	fn is_wanted(&self, sequence: u64) -> bool {
		self.result.wanted() && self.fills.read().wants(self.id, sequence)
	}

	fn max_delay(&self) -> Option<Duration> {
		self.asks.read().max_delay()
	}

	fn poll_unwanted(&self, sequence: u64, waiter: &kio::Waiter) -> Poll<()> {
		if self.result.poll_wanted_changed(waiter, true).is_ready() {
			return Poll::Ready(());
		}
		self.fills
			.poll(waiter, |fills| match fills.wants(self.id, sequence) {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
			.map(drop)
	}

	/// The runs of the fill a reader still reads, read before any other lock.
	fn wanted(&self) -> RangeSet<u64> {
		let span = self.start.group..self.end;
		let fills = self.fills.lock();
		fills
			.pending
			.get(&self.id)
			.map_or_else(RangeSet::new, |pending| pending.wanted(&span))
	}

	fn frame_start(&self, sequence: u64) -> u64 {
		match sequence == self.start.group {
			true => self.start.frame,
			false => 0,
		}
	}

	fn bounds(&self, sequence: u64) -> group::Bounds {
		group::Bounds {
			start: self.frame_start(sequence),
			end: if sequence.checked_add(1) == Some(self.end) {
				self.frame_end
			} else {
				None
			},
		}
	}

	fn accept(&self, sequence: u64, info: Option<Info>) -> Result<group::Producer> {
		self.accept_from(sequence, info, self.bounds(sequence))
	}

	fn accept_from(&self, sequence: u64, info: Option<Info>, frames: group::Bounds) -> Result<group::Producer> {
		if !self.contains(sequence) {
			return Err(Error::InvalidRange);
		}
		let Some(end) = sequence.checked_add(1) else {
			return Err(Error::InvalidRange);
		};
		if self.result.read().resolved.contains(&sequence) {
			return Err(Error::Duplicate);
		}
		// Cache the group before answering: readers resolve an accept through the cache.
		let res = TrackState::modify(&self.state).and_then(|mut state| {
			let res = state.insert_group_request(sequence, frames, info);
			let deferred = state.deferred();
			drop(state);
			deferred.wake();
			res
		});
		if let Ok(mut outcome) = self.result.write() {
			outcome.accept(sequence..end);
			if let Ok(group) = &res
				&& (frames.start > 0 || frames.end.is_some())
			{
				outcome.partial.insert(sequence, group.handle());
			}
		}
		uncover(&self.fills, self.id, sequence..end);
		res
	}

	/// Refuse `groups` with `err`, as the source answered under `asked`, recording them in
	/// the track's drops when the source no longer has them, which binds every reader. Only
	/// what a reader still reads: a handler refusing the whole fill never checked the rest.
	fn drop_groups(&self, groups: ops::Range<u64>, err: Error, asked: Option<Duration>) {
		let unresolved = self.owed(groups);
		if unresolved.is_empty() {
			return;
		}
		if let Ok(mut state) = TrackState::modify(&self.state) {
			for groups in unresolved.iter() {
				state.drop_unfed(groups.clone(), StreamError::from(&err));
			}
		}
		self.refuse(&unresolved, err, asked);
	}

	/// The part of `groups` inside the fill that a reader still reads and nothing answered yet.
	fn owed(&self, groups: ops::Range<u64>) -> RangeSet<u64> {
		let groups = groups.start.max(self.start.group)..groups.end.min(self.end);
		let mut owed = RangeSet::new();
		if groups.is_empty() {
			return owed;
		}
		let wanted = self.wanted();
		let outcome = self.result.read();
		for run in wanted.overlapping(&groups) {
			for gap in outcome
				.resolved
				.gaps(&(run.start.max(groups.start)..run.end.min(groups.end)))
			{
				owed.insert(gap);
			}
		}
		owed
	}

	/// The part of `groups` nothing answered yet.
	fn unresolved(&self, groups: ops::Range<u64>) -> RangeSet<u64> {
		self.result.read().resolved.gaps(&groups).collect()
	}

	fn refuse(&self, groups: &RangeSet<u64>, err: Error, asked: Option<Duration>) {
		if groups.is_empty() {
			return;
		}
		if let Ok(mut outcome) = self.result.write() {
			for groups in groups.iter() {
				outcome.refuse(groups.clone(), err.clone(), asked);
			}
		}
		for groups in groups.iter() {
			uncover(&self.fills, self.id, groups.clone());
		}
	}

	/// Whether an [`Error::Old`] answer under `asked` is the publisher's max age, which binds
	/// every reader, rather than that budget, which a more tolerant reader may exceed.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn old_binds_everyone(&self, asked: Option<Duration>) -> bool {
		let Some(max_delay) = asked else {
			return true;
		};
		let bound = self.state.read().max_age_bound();
		bound.is_some_and(|bound| clamp_max_delay(max_delay, Some(bound)) >= bound)
	}

	/// Refuse `groups` with `err`, as their source answered a request for them under `asked`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn reject(&self, groups: ops::Range<u64>, err: Error, asked: Option<Duration>) {
		match err {
			// A datagram is never cached, so no request ever gets it. Only a request for that
			// one group says which group it was.
			Error::NotFetchable if groups.end - groups.start == 1 => self.drop_groups(groups, err, asked),
			// Stale under the fill's budget, and so is every group older than it: a timed
			// track's starts never rewind. Only the publisher's max age binds every reader;
			// under the fill's own budget they are withdrawn, for a reader with a wider one.
			Error::Old => {
				let older = self.start.group..groups.end;
				match self.old_binds_everyone(asked) {
					true => self.drop_groups(older, Error::Old, asked),
					false => {
						let unresolved = self.unresolved(older);
						self.refuse(&unresolved, Error::Old, asked);
					}
				}
			}
			// Anything else, a miss included, answers only this request: one starting at
			// another frame may still be served, such as the tail of a group whose head
			// expired. Refused for now, recorded nowhere.
			err => {
				let unresolved = self.unresolved(groups);
				self.refuse(&unresolved, err, asked);
			}
		}
	}

	/// The run of owed sequences around `sequence`: read by a reader and unanswered.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn owed_around(&self, sequence: u64) -> ops::Range<u64> {
		let wanted = self.wanted();
		let outcome = self.result.read();
		wanted
			.overlapping(&(sequence..sequence + 1))
			.flat_map(|run| outcome.resolved.gaps(run))
			.find(|gap| gap.contains(&sequence))
			.unwrap_or(sequence..sequence + 1)
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn finish_track_at(&self, final_sequence: u64) {
		if let Ok(mut state) = TrackState::modify(&self.state)
			&& state.end.is_none()
		{
			let _ = state.set_end(Position::group(final_sequence), None);
		}
	}
}

impl Drop for Inner {
	fn drop(&mut self) {
		// Whatever the handler left unanswered is named dropped, but only while a reader still
		// reads it: once demand left, the fill was withdrawn and records nothing. A group cached
		// since (the live feed delivered it) is no drop at all.
		let wanted = self.wanted();
		let left: RangeSet<u64> = {
			let outcome = self.result.read();
			wanted.iter().flat_map(|run| outcome.resolved.gaps(run)).collect()
		};
		if !left.is_empty() && self.result.wanted() {
			let asked = self.max_delay();
			if let Ok(mut state) = TrackState::modify(&self.state) {
				// Unanswered is a miss downstream, as an explicit `drop_groups` would say.
				for groups in left.iter() {
					state.drop_unfed(groups.clone(), StreamError::NotFound);
				}
			}
			if let Ok(mut outcome) = self.result.write() {
				for groups in left.iter() {
					outcome.refuse(groups.clone(), Error::Dropped, asked);
				}
			}
		}
		self.fills.lock().end(self.id);
	}
}

impl TrackState {
	/// Record `groups` as dropped, as a fill's source answered, except what the cache holds
	/// or the live feed may still deliver: a fill refused is no verdict on the feed.
	fn drop_unfed(&mut self, groups: ops::Range<u64>, err: StreamError) {
		// Until the serving session resolves where the feed starts, it may deliver anything.
		if self.start_pending {
			return;
		}
		let now = self.cache.pool().now_wall();
		let mut dropped = RangeSet::new();
		for hole in super::miss::holes(self, groups) {
			for groups in self.unfed(hole, now).0.iter() {
				dropped.insert(groups.clone());
			}
		}
		for groups in dropped.iter() {
			self.drop_groups(groups.clone(), err.clone());
		}
	}
}

/// The readers of one group of a fill, outliving its [`GroupFill`].
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct Readers {
	fills: kio::Shared<FillState>,
	id: u64,
	sequence: u64,
	outcome: kio::Producer<Outcome>,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl Readers {
	/// Poll until no reader waits on the group. Once the fill ends its per-group demand goes
	/// with it, so then until none of its readers is left.
	pub(crate) fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.outcome.poll_wanted_changed(waiter, true).is_ready() {
			return Poll::Ready(());
		}
		self.fills
			.poll(waiter, |fills| match fills.pending.get(&self.id) {
				Some(pending) if !pending.wants.contains_key(&self.sequence) => Poll::Ready(()),
				_ => Poll::Pending,
			})
			.map(drop)
	}
}

/// One group of a [`Fill`], for a session serving it a group at a time. Answering it answers
/// that group of the fill; dropping it unanswered leaves the group to the fill.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct GroupFill {
	inner: Arc<SendCell<Inner>>,
	sequence: u64,
	frame_start: u64,
	frame_end: Option<u64>,
	priority: i8,
	max_delay: Option<Duration>,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl GroupFill {
	/// The group sequence to fetch.
	pub(crate) fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The delivery priority the readers asked for when the group was taken.
	pub(crate) fn priority(&self) -> i8 {
		self.priority
	}

	/// The widest budget among the readers when the group was taken, for a handler that
	/// forwards it.
	pub(crate) fn max_delay(&self) -> Option<Duration> {
		self.max_delay
	}

	/// The first frame of the group the readers want; 0 is the whole group.
	pub(crate) fn frame_start(&self) -> u64 {
		self.frame_start
	}

	/// The requested exclusive last frame, if this copy is bounded at the back.
	pub(crate) fn frame_end(&self) -> Option<u64> {
		self.frame_end
	}

	/// Accept the actual wire range, which an older or compressed upstream may have widened.
	pub(crate) fn accept_from(&self, info: Info, frames: group::Bounds) -> Result<group::Producer> {
		self.inner.accept_from(self.sequence, Some(info), frames)
	}

	/// Poll until no reader waits on this group, as [`Fill::poll_unwanted`].
	pub(crate) fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_unwanted(self.sequence, waiter)
	}

	/// Whether a reader still waits on the fill.
	#[cfg(test)]
	pub(crate) fn is_wanted(&self) -> bool {
		self.inner.result.wanted()
	}

	/// The group's readers, for a session still writing it once accepted. Holds the fill's
	/// answers open, as the group does not answer its readers until they take it.
	pub(crate) fn readers(&self) -> Readers {
		Readers {
			fills: self.inner.fills.clone(),
			id: self.inner.id,
			sequence: self.sequence,
			outcome: self.inner.result.clone(),
		}
	}

	/// Insert the group into the cache, as [`Fill::accept`].
	#[cfg(test)]
	pub(crate) fn accept(self, info: impl Into<Option<Info>>) -> Result<group::Producer> {
		self.inner.accept(self.sequence, info.into())
	}

	/// Refuse the group with the source's answer, for the readers waiting now. Only a
	/// datagram, or [`Error::Old`] under the publisher's max age (with every older group of
	/// the fill), names it dropped.
	pub(crate) fn reject(self, err: Error) {
		self.inner.reject(self.sequence..self.sequence + 1, err, self.max_delay);
	}
}

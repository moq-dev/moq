//! Fills: the groups readers miss, asked of a [`Dynamic`](super::Dynamic) handler by range.
//!
//! A miss is a sequence a reader wants that the cache doesn't hold and the live feed will
//! never deliver. A reader joins the fill already covering it, or queues a new one, and a
//! handler serves each fill it takes as one bounded run: [`Fill::accept`] for each group it
//! has, [`Fill::drop_groups`] for the rest.

use std::{
	collections::{HashMap, VecDeque},
	ops,
	sync::Arc,
	task::Poll,
	time::Duration,
};

use rangemap::{RangeMap, RangeSet};

use super::{Info, Order, Position, TrackState, clamp_max_delay};
use crate::{Error, Result, StreamError, group};

/// The fills a track's readers asked for, queued or taken by a handler, under one lock.
#[derive(Default)]
pub(in crate::model) struct FillState {
	// Live `Dynamic` handles: a fill queues only while one can take it.
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
	priority: i8,
	max_delay: Option<Duration>,
	order: Order,
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

/// What became of a fill's sequences, shared with every reader that joined it.
#[derive(Default)]
// pending: /quest/m1/public-model/deferred.md
pub(crate) struct Outcome {
	accepted: RangeSet<u64>,
	// Why each refused run was refused; refusing a sequence again replaces why.
	refused: RangeMap<u64, Refusal>,
	refusals: u64,
	// Accepted or refused, kept as answers arrive so a fill answered group by group costs
	// the runs each answer touches, not every answer before it.
	resolved: RangeSet<u64>,
	// The budget the fill asked its handler for, so a looser reader can tell that an
	// [`Error::Old`] was not its own.
	asked: Option<Duration>,
}

impl Outcome {
	/// Why `sequence` was refused, if it was.
	// private: capability gap
	pub(crate) fn refused(&self, sequence: u64) -> Option<&Error> {
		self.refused.get(&sequence).map(|refusal| &refusal.err)
	}

	pub(super) fn is_accepted(&self, sequence: u64) -> bool {
		self.accepted.contains(&sequence)
	}

	pub(super) fn asked(&self) -> Option<Duration> {
		self.asked
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
				answers.push((groups, Err((refusal.err.clone(), self.asked))));
			}
		}
		answers
	}

	fn accept(&mut self, groups: ops::Range<u64>) {
		self.accepted.insert(groups.clone());
		self.resolved.insert(groups);
	}

	fn refuse(&mut self, groups: ops::Range<u64>, err: Error) {
		if groups.is_empty() {
			return;
		}
		let id = self.refusals;
		self.refusals += 1;
		self.refused.insert(groups.clone(), Refusal { id, err });
		self.resolved.insert(groups);
	}
}

/// One refusal, equal only to itself: the map keeps a run it refused whole, but never merges
/// two refusals whose errors may differ.
#[derive(Clone)]
struct Refusal {
	id: u64,
	err: Error,
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
	pub priority: i8,
	pub max_delay: Option<Duration>,
	pub order: Order,
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
}

impl Join {
	pub(super) fn outcome(&self) -> &kio::Consumer<Outcome> {
		self.outcome.as_ref().expect("held until dropped")
	}
}

impl Drop for Join {
	fn drop(&mut self) {
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
		if !self.attempt.is_used() {
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
		// Queued in the reader's order, so a descending reader's newest fragment goes first.
		if ask.order == Order::Descending {
			gaps.reverse();
		}
		for (groups, id) in covered {
			let pending = self
				.pending
				.get_mut(&id)
				.expect("a covered sequence names a pending fill");
			// Widened only while queued: once taken, the handler holds what it was asked.
			if pending.queued {
				pending.priority = pending.priority.max(ask.priority);
				pending.max_delay = looser(pending.max_delay, ask.max_delay);
				// Any descending reader makes it newest first, as it does a subscription.
				if ask.order == Order::Descending {
					pending.order = Order::Descending;
				}
				// A reader starting in an earlier group wants this one whole.
				if groups.start == pending.start.group {
					let frame = match ask.start.group == groups.start {
						true => ask.start.frame,
						false => 0,
					};
					pending.start.frame = pending.start.frame.min(frame);
				}
			}
			pending.want(groups.clone());
			joins.push(Join {
				fills: shared.clone(),
				id,
				held: RangeSet::from_iter([groups.clone()]),
				groups,
				outcome: Some(pending.result.consume()),
				attempt: pending.result.weak(),
			});
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
			joins.push(Join {
				fills: shared.clone(),
				id,
				groups: groups.clone(),
				held: RangeSet::from_iter([groups.clone()]),
				outcome: Some(result.consume()),
				attempt: result.weak(),
			});
			self.covers.insert(groups.clone(), id);
			self.pending.insert(
				id,
				Pending {
					start,
					end: groups.end,
					priority: ask.priority,
					max_delay: ask.max_delay,
					order: ask.order,
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

/// The more tolerant of two budgets: `None` is publisher max-age alone, which no explicit
/// budget can relax.
fn looser(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.max(b)),
		_ => None,
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
		let (start, end, priority, max_delay, order) = (
			pending.start,
			pending.end,
			pending.priority,
			pending.max_delay,
			pending.order,
		);
		let result = pending.result.clone();
		drop(guard);
		// Built with the fills unlocked, so no other lock nests under theirs.
		let inner = Inner {
			state: state.clone(),
			fills: fills.clone(),
			id,
			start,
			end,
			priority,
			max_delay,
			order,
			result,
		};
		// What it asked for decides whose `Old` it is.
		if let Ok(mut outcome) = inner.result.write() {
			outcome.asked = inner.max_delay;
		}
		return Poll::Ready(Ok(Fill { inner: Arc::new(inner) }));
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

/// A run of groups readers miss, for a [`Dynamic`](super::Dynamic) handler to serve.
///
/// It names `start.group` up to the exclusive [`end`](Self::end), the first group from
/// [`start`](Self::start)'s frame. The handler calls [`accept`](Self::accept) for each group
/// it has and [`drop_groups`](Self::drop_groups) for the rest. Dropping the fill names
/// whatever is left as dropped while a reader still wants it; once nobody does, the fill is
/// withdrawn and records nothing, so stop serving it when [`unused`](Self::unused) resolves.
/// A reader narrowing its ranges may leave a single group, so stop serving that one when
/// [`unwanted`](Self::unwanted) resolves and move on to [`next`](Self::next).
pub struct Fill {
	inner: Arc<Inner>,
}

/// The state a [`Fill`] and its per-group handles share. The last one dropping settles what
/// was left.
struct Inner {
	state: kio::Producer<TrackState>,
	fills: kio::Shared<FillState>,
	id: u64,
	start: Position,
	end: u64,
	priority: i8,
	max_delay: Option<Duration>,
	order: Order,
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

	/// The most urgent delivery priority among the readers waiting on it.
	pub fn priority(&self) -> i8 {
		self.inner.priority
	}

	/// The most tolerant age budget among the readers waiting on it, or `None` for the
	/// publisher's max age alone. Each reader still enforces its own.
	pub fn max_delay(&self) -> Option<Duration> {
		self.inner.max_delay
	}

	/// Which end to fill first: [`Order::Descending`] is newest first.
	pub fn order(&self) -> Order {
		self.inner.order
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
		self.inner.drop_groups(groups, Error::NotFound);
	}

	/// Whether a reader still waits on this fill.
	pub fn is_used(&self) -> bool {
		self.inner.result.is_used()
	}

	/// Wait until no reader waits on this fill: it is withdrawn, so stop serving it.
	#[cfg(feature = "async")]
	pub async fn unused(&self) {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll until no reader waits on this fill.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.result.poll_unused(waiter).map(|_| ())
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
	#[cfg(test)]
	pub(super) fn answers(&self) -> usize {
		let outcome = self.inner.result.read();
		outcome.refused.iter().count() + outcome.accepted.iter().count()
	}

	/// The next group still owed in the fill's order: a reader still reads it, and it was
	/// neither answered nor delivered live since it was asked. `None` once none is owed.
	pub fn next(&self) -> Option<u64> {
		loop {
			let wanted = self.inner.wanted();
			let sequence = {
				let outcome = self.inner.result.read();
				let mut gaps = wanted.iter().flat_map(|run| outcome.resolved.gaps(run));
				match self.inner.order {
					Order::Ascending => gaps.next().map(|gap| gap.start),
					Order::Descending => gaps.last().map(|gap| gap.end - 1),
				}
			}?;
			// The live feed may have delivered it since: answered by the cache, not the source.
			let cached = self
				.inner
				.state
				.read()
				.covering_group(sequence, self.inner.frame_start(sequence))
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

	/// Refuse every group still owed with `err`, which says nothing about whether the source
	/// has them, so nothing is recorded dropped.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: capability gap
	pub(crate) fn refuse(&self, err: Error) {
		let unresolved = self.inner.unresolved(self.inner.start.group..self.inner.end);
		self.inner.refuse(&unresolved, err);
	}

	/// A handle on group `sequence` alone, for a session serving the fill one group at a time.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: capability gap
	pub(crate) fn group(&self, sequence: u64) -> GroupFill {
		GroupFill {
			inner: self.inner.clone(),
			sequence,
			frame_start: match sequence == self.inner.start.group {
				true => self.inner.start.frame,
				false => 0,
			},
		}
	}
}

impl Inner {
	fn contains(&self, sequence: u64) -> bool {
		(self.start.group..self.end).contains(&sequence)
	}

	fn is_wanted(&self, sequence: u64) -> bool {
		self.result.is_used() && self.fills.read().wants(self.id, sequence)
	}

	fn poll_unwanted(&self, sequence: u64, waiter: &kio::Waiter) -> Poll<()> {
		if self.result.poll_unused(waiter).is_ready() {
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

	fn accept(&self, sequence: u64, info: Option<Info>) -> Result<group::Producer> {
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
		let res = TrackState::modify(&self.state)
			.and_then(|mut state| state.insert_group_request(sequence, self.frame_start(sequence), info));
		if let Ok(mut outcome) = self.result.write() {
			outcome.accept(sequence..end);
		}
		uncover(&self.fills, self.id, sequence..end);
		res
	}

	/// Refuse `groups` with `err`, recording them in the track's drops when the source no
	/// longer has them, which binds every reader. Only what a reader still reads: a handler
	/// refusing the whole fill never checked the rest.
	fn drop_groups(&self, groups: ops::Range<u64>, err: Error) {
		let groups = groups.start.max(self.start.group)..groups.end.min(self.end);
		if groups.is_empty() {
			return;
		}
		let wanted = self.wanted();
		let unresolved: RangeSet<u64> = {
			let outcome = self.result.read();
			let mut unresolved = RangeSet::new();
			for run in wanted.overlapping(&groups) {
				for gap in outcome
					.resolved
					.gaps(&(run.start.max(groups.start)..run.end.min(groups.end)))
				{
					unresolved.insert(gap);
				}
			}
			unresolved
		};
		if unresolved.is_empty() {
			return;
		}
		if let Ok(mut state) = TrackState::modify(&self.state) {
			for groups in unresolved.iter() {
				state.drop_unfed(groups.clone(), StreamError::from(&err));
			}
		}
		self.refuse(&unresolved, err);
	}

	/// The part of `groups` nothing answered yet.
	fn unresolved(&self, groups: ops::Range<u64>) -> RangeSet<u64> {
		self.result.read().resolved.gaps(&groups).collect()
	}

	fn refuse(&self, groups: &RangeSet<u64>, err: Error) {
		if groups.is_empty() {
			return;
		}
		if let Ok(mut outcome) = self.result.write() {
			for groups in groups.iter() {
				outcome.refuse(groups.clone(), err.clone());
			}
		}
		for groups in groups.iter() {
			uncover(&self.fills, self.id, groups.clone());
		}
	}

	/// Whether an [`Error::Old`] answer is the publisher's max age, which binds every reader,
	/// rather than this fill's own budget, which a more tolerant reader may exceed.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn old_binds_everyone(&self) -> bool {
		let Some(max_delay) = self.max_delay else {
			return true;
		};
		let bound = self.state.read().max_age_bound();
		bound.is_some_and(|bound| clamp_max_delay(max_delay, Some(bound)) >= bound)
	}

	/// Refuse group `sequence` with `err`, as its source answered.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn reject(&self, sequence: u64, err: Error) {
		let Some(end) = sequence.checked_add(1) else {
			return;
		};
		match err {
			// The source no longer has it.
			Error::NotFound | Error::NotFetchable => self.drop_groups(sequence..end, err),
			// Stale under the fill's budget, and so is every group older than it: a timed
			// track's starts never rewind. Only the publisher's max age binds every reader;
			// under the fill's own budget they are withdrawn, for a reader with a wider one.
			Error::Old => {
				let older = self.start.group..end;
				match self.old_binds_everyone() {
					true => self.drop_groups(older, Error::Old),
					false => {
						let unresolved = self.unresolved(older);
						self.refuse(&unresolved, Error::Old);
					}
				}
			}
			// Anything else says nothing about the group: refused for now, recorded nowhere.
			err => {
				let unresolved = self.unresolved(sequence..end);
				self.refuse(&unresolved, err);
			}
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
		if !left.is_empty() && self.result.is_used() {
			if let Ok(mut state) = TrackState::modify(&self.state) {
				// Unanswered is a miss downstream, as an explicit `drop_groups` would say.
				for groups in left.iter() {
					state.drop_unfed(groups.clone(), StreamError::NotFound);
				}
			}
			if let Ok(mut outcome) = self.result.write() {
				for groups in left.iter() {
					outcome.refuse(groups.clone(), Error::Dropped);
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
// private: capability gap
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
	// private: capability gap
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.outcome.poll_unused(waiter).is_ready() {
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
// private: capability gap
pub(crate) struct GroupFill {
	inner: Arc<Inner>,
	sequence: u64,
	frame_start: u64,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl GroupFill {
	/// The group sequence to fetch.
	// private: capability gap
	pub(crate) fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The delivery priority the readers asked for.
	// private: capability gap
	pub(crate) fn priority(&self) -> i8 {
		self.inner.priority
	}

	/// The widest budget among the readers, for a handler that forwards it.
	// private: capability gap
	pub(crate) fn max_delay(&self) -> Option<Duration> {
		self.inner.max_delay
	}

	/// The first frame of the group the readers want; 0 is the whole group.
	// private: capability gap
	pub(crate) fn frame_start(&self) -> u64 {
		self.frame_start
	}

	/// Poll until no reader waits on this group, as [`Fill::poll_unwanted`].
	// private: capability gap
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_unwanted(self.sequence, waiter)
	}

	/// Poll until the track closes, with its abort error.
	// private: capability gap
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		TrackState::poll_closed(&self.inner.state, waiter)
	}

	/// Whether a reader still waits on the fill.
	#[cfg(test)]
	// private: capability gap
	pub(crate) fn is_used(&self) -> bool {
		self.inner.result.is_used()
	}

	/// The fill's answers, kept open as long as this handle lives.
	#[cfg(test)]
	// private: capability gap
	pub(crate) fn joined(&self) -> kio::Producer<Outcome> {
		self.inner.result.clone()
	}

	/// The group's readers, for a session still writing it once accepted. Holds the fill's
	/// answers open, as the group does not answer its readers until they take it.
	// private: capability gap
	pub(crate) fn readers(&self) -> Readers {
		Readers {
			fills: self.inner.fills.clone(),
			id: self.inner.id,
			sequence: self.sequence,
			outcome: self.inner.result.clone(),
		}
	}

	/// The track the fill serves.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: capability gap
	pub(crate) fn track(&self) -> kio::Producer<TrackState> {
		self.inner.state.clone()
	}

	/// Insert the group into the cache, as [`Fill::accept`].
	// private: capability gap
	pub(crate) fn accept(self, info: impl Into<Option<Info>>) -> Result<group::Producer> {
		self.inner.accept(self.sequence, info.into())
	}

	/// Refuse the group with the source's answer. A miss names it dropped, and
	/// [`Error::Old`] every older group of the fill too.
	// private: capability gap
	pub(crate) fn reject(self, err: Error) {
		self.inner.reject(self.sequence, err);
	}

	/// Declare the track's exclusive final sequence, as the source answering this group
	/// reported it. A no-op once the track declared an end, or holds a later group.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// private: capability gap
	pub(crate) fn finish_track_at(&self, final_sequence: u64) {
		if let Ok(mut state) = TrackState::modify(&self.inner.state)
			&& state.end.is_none()
		{
			let _ = state.set_end(Position::group(final_sequence));
		}
	}
}

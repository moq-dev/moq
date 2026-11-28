//! Fills: the groups readers miss, asked of a [`Requests`](super::Requests) handler by range.
//!
//! A miss is a key a reader wants that the cache doesn't hold and the live feed will
//! never deliver. A reader joins the fill already covering it, or queues a new one, and a
//! handler serves each fill it takes as one bounded run: [`FillGroups::accept`] for each group it
//! has, [`FillGroups::drop_groups`] for the rest.
//!
//! A fill serves under what its readers ask now: a reader changing its priority or
//! order moves every fill it waits on, queued or taken. Only the first group's start frame
//! stays as the handler took it, since a handler may have asked its source for it already;
//! a reader wanting earlier frames of that group asks a fill of its own.

use std::{
	collections::{BTreeMap, HashMap, VecDeque, btree_map},
	ops,
	sync::Arc,
	task::Poll,
};

use rangemap::{RangeMap, RangeSet};

use super::{Info, Order, Position, TrackState};
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
	// The fill each unresolved key waits on, so a second reader joins it rather than
	// asking again.
	covers: RangeMap<u64, u64>,
}

/// One fill until it ends.
struct Pending {
	start: Position,
	end: u64,
	// Exclusive frame cap in the last group; None asks through the original end.
	frame_end: Option<u64>,
	// For a time start, the oldest key its group, the newest starting at or before the
	// time, may start at.
	floor: Option<u64>,
	// What its joins ask with now, shared with them and its handler.
	asks: kio::Shared<Asks>,
	queued: bool,
	// A flight: a separate copy of one group from its start frame, which no other reader
	// joins by key; see [`FillState::fly`].
	flight: bool,
	// How many joins still read each key: one a reader narrowed away is no longer owed.
	wants: RangeMap<u64, u32>,
	// What it answered that no join has picked up yet, which holds back the next group: see
	// [`FillGroups::poll_wanted_changed`].
	unseen: RangeSet<u64>,
	// What it answered that no join took yet: the first in its order is held in the cache for
	// them (see [`super::held`]). A join taking a group, or no longer reading it, takes it for
	// all.
	untaken: RangeSet<u64>,
	// How many joins are their reader's front, the fill holding the next group that reader
	// takes: only a fill some reader fronts holds a group, so each reader holds one at most.
	fronts: u32,
	// Readers hold consumers, so it counts the fill's demand; the handler's copy says what
	// became of each key.
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
		self.unseen.remove(groups.clone());
		self.untaken.remove(groups.clone());
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
pub(in crate::model) struct Asking {
	pub priority: i8,
	pub order: Order,
}

/// What a fill's joins ask with, counted so a join arriving, leaving, or changing its mind
/// costs its own entry rather than a pass over every join.
#[derive(Default)]
struct Asks {
	priorities: BTreeMap<i8, usize>,
	descending: usize,
}

impl Asks {
	fn add(&mut self, asking: &Asking) {
		*self.priorities.entry(asking.priority).or_default() += 1;
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
		if asking.order == Order::Descending {
			self.descending -= 1;
		}
	}

	/// The most urgent priority asked.
	fn priority(&self) -> i8 {
		self.priorities.last_key_value().map_or(0, |(priority, _)| *priority)
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

/// What became of a fill's keys, shared with every reader that joined it.
#[derive(Default)]
pub(crate) struct Outcome {
	accepted: RangeSet<u64>,
	// Why each refused run was refused; refusing a key again replaces why.
	refused: RangeMap<u64, Refusal>,
	refusals: u64,
	// Accepted or refused, kept as answers arrive so a fill answered group by group costs
	// the runs each answer touches, not every answer before it.
	resolved: RangeSet<u64>,
	// Each accepted copy short of the whole group, which a later copy may displace from the
	// cache before a reader takes it. A whole copy covers every other, so none displaces it.
	partial: BTreeMap<u64, group::Handle>,
	// The group a time start's fill accepted below the time, created or already cached, held so
	// where it ends is known even once the cache lets it go.
	answer: Option<group::Handle>,
	// What it held, answered or still landing, that a debt evicted with no room to hold it
	// again beside the pins and the other holds: a refill would be evicted the same way, so a
	// reader finding the eviction gives it up, and a lite-07 recovery is not asked again.
	// Kept until no join reads it, so neither a grace nor a debt forgets it.
	unfit: RangeSet<u64>,
}

impl Outcome {
	/// Why `key` was refused, if it was.
	pub(crate) fn refused(&self, key: u64) -> Option<&Error> {
		self.refused.get(&key).map(|refusal| &refusal.err)
	}

	pub(super) fn is_accepted(&self, key: u64) -> bool {
		self.accepted.contains(&key)
	}

	/// The keys of `groups` it answered that a debt evicted with no room to hold them again.
	pub(super) fn unfit(&self, groups: &ops::Range<u64>) -> RangeSet<u64> {
		self.unfit
			.overlapping(groups)
			.map(|run| run.start.max(groups.start)..run.end.min(groups.end))
			.collect()
	}

	/// The group a time start's fill accepted below the time.
	pub(super) fn answer(&self) -> Option<&group::Handle> {
		self.answer.as_ref()
	}

	/// What a time start's fill answered, once `time`'s key is: `None` while it is out.
	pub(super) fn time_start(&self, time: u64) -> Option<TimeStart> {
		if let Some(run) = self.accepted.iter().next() {
			return Some(TimeStart::Group(run.start));
		}
		match self.refused.get(&time) {
			Some(refusal) if refusal.hole.is_some() => Some(TimeStart::Dropped),
			Some(refusal) => Some(TimeStart::Refused(refusal.err.clone())),
			// Answered and forgotten: nobody reads it any more.
			None if self.resolved.contains(&time) => Some(TimeStart::Unanswered),
			None => None,
		}
	}

	/// The partial copy accepted for `key`, if it still holds `frames`.
	pub(super) fn partial(&self, key: u64, frames: group::Bounds) -> Option<group::Handle> {
		self.partial.get(&key).filter(|copy| copy.covers(frames)).cloned()
	}

	/// What was answered within `wanted`: accepted, or refused with why and the hole its source
	/// named, if any.
	#[expect(
		clippy::type_complexity,
		reason = "each answer pairs a run with its outcome; naming it would only move the type"
	)]
	pub(super) fn within(
		&self,
		wanted: &RangeSet<u64>,
	) -> Vec<(ops::Range<u64>, std::result::Result<(), (Error, Option<super::Hole>)>)> {
		let mut answers = Vec::new();
		for run in wanted.iter() {
			for accepted in self.accepted.overlapping(run) {
				answers.push((accepted.start.max(run.start)..accepted.end.min(run.end), Ok(())));
			}
			for (refused, refusal) in self.refused.overlapping(run) {
				let groups = refused.start.max(run.start)..refused.end.min(run.end);
				answers.push((groups, Err((refusal.err.clone(), refusal.hole.clone()))));
			}
		}
		answers
	}

	fn accept(&mut self, groups: ops::Range<u64>) {
		self.unfit.remove(groups.clone());
		self.accepted.insert(groups.clone());
		self.resolved.insert(groups);
	}

	fn refuse(&mut self, groups: ops::Range<u64>, err: Error, hole: Option<super::Hole>) {
		if groups.is_empty() {
			return;
		}
		let id = self.refusals;
		self.refusals += 1;
		self.refused.insert(groups.clone(), Refusal { id, err, hole });
		self.resolved.insert(groups);
	}
}

/// What a time start's fill answered.
pub(super) enum TimeStart {
	/// The group containing the time.
	Group(u64),
	/// No group from the fill's floor through the time.
	Dropped,
	/// Refused with this error, saying nothing of where the time falls.
	Refused(Error),
	/// Nothing answers it: no handler is left to.
	Unanswered,
}

/// One refusal, equal only to itself: the map keeps a run it refused whole, but never merges
/// two refusals whose errors may differ.
#[derive(Clone)]
struct Refusal {
	id: u64,
	err: Error,
	// The hole the source named, its word that it has none of them (or never produced them),
	// rather than refused for now.
	hole: Option<super::Hole>,
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
	/// The keys of the fill this reader asked for.
	pub groups: ops::Range<u64>,
	// The part of `groups` it still reads, counted in the fill's wants.
	held: RangeSet<u64>,
	// Counts this reader in the fill's demand; `None` only while dropping.
	outcome: Option<kio::Consumer<Outcome>>,
	attempt: kio::ProducerWeak<Outcome>,
	// What this reader asks the fill with, counted in its asks.
	asking: Asking,
	asks: kio::Shared<Asks>,
	// Whether this is its reader's front, counted in the fill's fronts.
	front: bool,
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
		if self.front {
			fills.pending.get_mut(&self.id).expect("checked above").fronts -= 1;
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

	/// The part of the fill the reader still reads.
	pub(super) fn held(&self) -> &RangeSet<u64> {
		&self.held
	}

	/// Whether the reader holds none of the fill any more.
	pub(super) fn is_empty(&self) -> bool {
		self.held.is_empty()
	}

	/// Make this its reader's front, the fill holding the next group the reader takes, or
	/// stop: a reader fronts one fill at a time, so it holds one group at most.
	pub(super) fn set_front(&mut self, front: bool) {
		if self.front == front {
			return;
		}
		self.front = front;
		let mut fills = self.fills.lock();
		if let Some(pending) = fills.pending.get_mut(&self.id)
			&& self.is_ours(pending)
		{
			match front {
				true => pending.fronts += 1,
				false => pending.fronts -= 1,
			}
		}
	}

	/// The reader picked up `groups` the fill answered, so its handler may serve the next one.
	pub(super) fn saw(&self, groups: &RangeSet<u64>) {
		let mut fills = self.fills.lock();
		// Read first: a mutable borrow wakes every poll parked on the fills.
		if !fills
			.pending
			.get(&self.id)
			.is_some_and(|pending| self.is_ours(pending) && groups.iter().any(|run| pending.unseen.overlaps(run)))
		{
			return;
		}
		let unseen = &mut fills.pending.get_mut(&self.id).expect("checked above").unseen;
		for run in groups.iter() {
			unseen.remove(run.clone());
		}
	}

	/// The caller took `key`, which it still reads through the fill: the fill holds it no
	/// longer and may serve its next group.
	pub(super) fn took(&self, key: u64) {
		let Some(end) = key.checked_add(1) else {
			return;
		};
		let mut fills = self.fills.lock();
		// Read first: a mutable borrow wakes every poll parked on the fills.
		if !fills.pending.get(&self.id).is_some_and(|pending| {
			self.is_ours(pending) && (pending.unseen.contains(&key) || pending.untaken.contains(&key))
		}) {
			return;
		}
		let pending = fills.pending.get_mut(&self.id).expect("checked above");
		pending.unseen.remove(key..end);
		pending.untaken.remove(key..end);
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
			let pending = self.pending.get_mut(&id).expect("a covered key names a pending fill");
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
				front: false,
			});
		}
		// Queued in the reader's order, so a descending reader's newest fragment goes first.
		gaps.sort_unstable_by_key(|gap| gap.start);
		if ask.asking.order == Order::Descending {
			gaps.reverse();
		}
		let takes = self.has_handlers();
		for groups in gaps {
			if !takes {
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
				front: false,
			});
			// A taken fill's first group split off above keeps that fill, routed here instead.
			self.covers.insert(groups.clone(), id);
			self.pending.insert(
				id,
				Pending {
					start,
					end: groups.end,
					frame_end: if groups.end == ask.end { ask.frame_end } else { None },
					floor: None,
					asks,
					queued: true,
					flight: false,
					wants: RangeMap::from_iter([(groups.clone(), 1)]),
					unseen: RangeSet::new(),
					untaken: RangeSet::new(),
					fronts: 0,
					result,
				},
			);
			self.queue.push_back(id);
		}
		(joins, unfillable)
	}

	/// Join or queue a flight of group `key` from `frame`: a fill answered with a copy
	/// of its own, which no reader joins by key, since a copy shed past its readers
	/// holds nothing for one asking earlier. Readers asking from the same frame before it is
	/// answered share it. `None` while no handler can take it.
	pub(super) fn fly(
		&mut self,
		shared: &kio::Shared<FillState>,
		key: u64,
		frame: u64,
		asking: Asking,
	) -> Option<Join> {
		let groups = key..key.checked_add(1)?;
		if self.handlers == 0 {
			return None;
		}
		let boarding = self.pending.iter_mut().find(|(_, pending)| {
			pending.flight
				&& pending.start == Position { group: key, frame }
				&& !pending.result.read().resolved.contains(&key)
		});
		let id = match boarding {
			Some((id, pending)) => {
				pending.want(groups.clone());
				pending.asks.lock().add(&asking);
				*id
			}
			None => {
				let id = self.next;
				self.next += 1;
				let mut asks = Asks::default();
				asks.add(&asking);
				self.pending.insert(
					id,
					Pending {
						start: Position { group: key, frame },
						end: groups.end,
						frame_end: None,
						floor: None,
						asks: kio::Shared::new(asks),
						queued: true,
						flight: true,
						wants: RangeMap::from_iter([(groups.clone(), 1)]),
						unseen: RangeSet::new(),
						untaken: RangeSet::new(),
						fronts: 0,
						result: Default::default(),
					},
				);
				self.queue.push_back(id);
				id
			}
		};
		let pending = &self.pending[&id];
		Some(Join {
			fills: shared.clone(),
			id,
			held: RangeSet::from_iter([groups.clone()]),
			groups,
			outcome: Some(pending.result.consume()),
			attempt: pending.result.weak(),
			asking,
			asks: pending.asks.clone(),
			front: false,
		})
	}

	/// Queue a fill of the group containing `time`, a reader's time start no held group
	/// resolves, with the join waiting on its answer: the group starting at `time`, or else the
	/// newest one before it, starting at or after `floor`. It joins nothing and nothing joins
	/// it, since an answer below `time` says nothing of the keys another reader asks. `None`
	/// while no handler can take it.
	pub(super) fn time_start(
		&mut self,
		shared: &kio::Shared<FillState>,
		time: u64,
		floor: u64,
		asking: Asking,
	) -> Option<Join> {
		let groups = time..time.checked_add(1)?;
		if self.handlers == 0 {
			return None;
		}
		let id = self.next;
		self.next += 1;
		let result = kio::Producer::<Outcome>::default();
		let mut asks = Asks::default();
		asks.add(&asking);
		let asks = kio::Shared::new(asks);
		let join = Join {
			fills: shared.clone(),
			id,
			groups: groups.clone(),
			held: RangeSet::from_iter([groups.clone()]),
			outcome: Some(result.consume()),
			attempt: result.weak(),
			asking,
			asks: asks.clone(),
			front: true,
		};
		self.pending.insert(
			id,
			Pending {
				start: Position::group(time),
				end: groups.end,
				frame_end: None,
				floor: Some(floor.min(time)),
				asks,
				queued: true,
				flight: false,
				wants: RangeMap::from_iter([(groups, 1)]),
				unseen: RangeSet::new(),
				untaken: RangeSet::new(),
				fronts: 1,
				result,
			},
		);
		self.queue.push_back(id);
		Some(join)
	}

	/// Count a join out of reading `groups` of fill `id`. What nobody reads any more is no
	/// longer routed to it: a reader wanting it later asks afresh rather than joining a fetch
	/// being cancelled, so a key a fill stops owing stays unowed. Its answers are
	/// forgotten too, so a wide fill holds the answers its readers have yet to take, not one
	/// per group its source answered.
	fn unwant(&mut self, id: u64, groups: ops::Range<u64>) {
		let Some(pending) = self.pending.get_mut(&id) else {
			return;
		};
		let unwanted = pending.unwant(groups);
		let answered = {
			let outcome = pending.result.read();
			unwanted.iter().any(|run| {
				outcome.refused.overlaps(run) || outcome.accepted.overlaps(run) || outcome.unfit.overlaps(run)
			})
		};
		if answered && let Ok(mut outcome) = pending.result.write() {
			for run in &unwanted {
				outcome.refused.remove(run.clone());
				outcome.accepted.remove(run.clone());
				outcome.unfit.remove(run.clone());
			}
		}
		for run in unwanted {
			for range in self.routed(id, &run) {
				self.covers.remove(range);
			}
		}
	}

	/// A debt evicted `key`, which fill `id` held, with no room to hold it again: see
	/// `Outcome::unfit`. Only while a join still reads it, which bounds it by the fill's wants.
	pub(super) fn unfit(&self, id: u64, key: u64) {
		if let Some(pending) = self.pending.get(&id)
			&& pending.wants.contains_key(&key)
			&& let Ok(mut outcome) = pending.result.write()
		{
			outcome.unfit.insert(key..key + 1);
		}
	}

	/// Whether a join of fill `id` still reads `key`.
	pub(super) fn wants(&self, id: u64, key: u64) -> bool {
		self.pending
			.get(&id)
			.is_some_and(|pending| pending.wants.contains_key(&key))
	}

	/// Whether fill `id` holds `key` for a reader: a join still reads it, and some reader
	/// fronts the fill.
	pub(super) fn holds_for(&self, id: u64, key: u64) -> bool {
		self.pending
			.get(&id)
			.is_some_and(|pending| pending.fronts > 0 && pending.wants.contains_key(&key))
	}

	/// Whether some reader fronts fill `id`.
	fn fronted(&self, id: u64) -> bool {
		self.pending.get(&id).is_some_and(|pending| pending.fronts > 0)
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

/// The pieces of `gap` once `skip` is taken out of it.
fn without(gap: ops::Range<u64>, skip: Option<u64>) -> impl Iterator<Item = ops::Range<u64>> {
	let pieces = match skip {
		Some(skip) if gap.contains(&skip) => [gap.start..skip, skip + 1..gap.end],
		_ => [gap, 0..0],
	};
	pieces.into_iter().filter(|piece| !piece.is_empty())
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
) -> Poll<Result<FillGroups>> {
	// Prefer serving a queued fill, even if the track has since aborted.
	if let Poll::Ready(mut guard) = fills.poll(waiter, |fills| match fills.has_queued() {
		true => Poll::Ready(()),
		false => Poll::Pending,
	}) {
		let id = guard.pop().expect("predicate guaranteed a fill");
		let pending = guard.pending.get(&id).expect("popped fill is pending");
		let (start, end, frame_end, floor, flight) = (
			pending.start,
			pending.end,
			pending.frame_end,
			pending.floor,
			pending.flight,
		);
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
			floor,
			flight,
			asks,
			result,
		};
		return Poll::Ready(Ok(FillGroups {
			inner: Arc::new(SendCell::new(inner)),
			prev: None,
		}));
	}

	// No fill queued: surface a track abort so the handler loop can exit.
	match state.poll_ref(waiter, |state| match state.outcome.aborted() {
		Some(err) => Poll::Ready(err.clone()),
		None => Poll::Pending,
	}) {
		Poll::Ready(Ok(err)) => Poll::Ready(Err(err)),
		Poll::Ready(Err(closed)) => Poll::Ready(Err(closed.outcome.aborted().cloned().unwrap_or(Error::Dropped))),
		Poll::Pending => Poll::Pending,
	}
}

/// A run of groups readers miss, for a [`Requests`](super::Requests) handler to serve.
///
/// It names `start.group` up to the exclusive [`end`](Self::end), the first group from
/// [`start`](Self::start)'s frame. A reader's time start that no group the cache holds places
/// asks for the group containing that time as a fill one key wide, from the time, whose first
/// group may start below it: see [`floor`](Self::floor). Its [`priority`](Self::priority)
/// and [`order`](Self::order) follow its readers as they
/// change, so read them as each group is served. The handler calls [`accept`](Self::accept) for each group
/// it has and [`drop_groups`](Self::drop_groups) for the rest, one group at a time as
/// [`poll_wanted_changed`](Self::poll_wanted_changed) yields them, so it writes no faster than its
/// readers read. Serve the fills you take concurrently: each waits on its own readers, so
/// serving them in turn holds every fill behind the readers of the one before. Dropping the fill names
/// whatever is left as dropped while a reader still wants it; once nobody does, the fill is
/// withdrawn and records nothing, so stop serving it once [`wanted`](Self::wanted) is `None`.
/// A reader narrowing its ranges may leave a single group, so stop serving that one when
/// [`poll_unwanted`](Self::poll_unwanted) is ready and move on to [`wanted`](Self::wanted).
///
/// A clone serves the same fill, and the fill is settled once the last clone drops. Each
/// clone watches on its own, so its first [`poll_wanted_changed`](Self::poll_wanted_changed)
/// reports the current value.
pub struct FillGroups {
	// Its track state reaches every group of the track, so the cell proves `Send` once
	// here instead of inside every session machine holding a fill.
	inner: Arc<SendCell<Inner>>,
	// The value `poll_wanted_changed` last returned, or `None` before it returned any.
	prev: Option<Option<u64>>,
}

impl Clone for FillGroups {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			prev: None,
		}
	}
}

/// The state a fill and its per-group handles share. The last one dropping settles what
/// was left.
struct Inner {
	state: kio::Producer<TrackState>,
	fills: kio::Shared<FillState>,
	id: u64,
	start: Position,
	end: u64,
	// Exclusive frame cap in the last group; None asks through the original end.
	frame_end: Option<u64>,
	// A time start's floor; see [`FillGroups::floor`].
	floor: Option<u64>,
	// Answered with a copy of its own rather than the cache's; see [`FillState::fly`].
	flight: bool,
	asks: kio::Shared<Asks>,
	result: kio::Producer<Outcome>,
}

impl FillGroups {
	/// The first group to fill, and the frame within it to start from.
	pub fn start(&self) -> Position {
		self.inner.start
	}

	/// The oldest group the first group may start at: [`start`](Self::start)'s, unless that is a
	/// reader's time start no group the cache holds places. Then the first group is the one
	/// containing that time: the group starting at it, or else the newest one starting before
	/// it, at or after the floor; once that group ends whole at or before the time, the reader
	/// starts past it, at its End. A handler knowing its groups only by key serves that key as
	/// any other; one that can tell which group holds the time, as from an index, may
	/// [`accept`](Self::accept) that group instead, or [`drop_groups`](Self::drop_groups) the
	/// key when none does. The reader starts where the answer says, never where the groups
	/// the cache holds happen to say: a drop, or the fill closed unanswered, says no group
	/// starts from the floor through the time, so it starts at the group just below the floor
	/// (at its End once the time is past that group's span), or past the time when the floor is
	/// 0. A [refusal](Self::refuse_groups) fails the reader
	/// with its error until its ranges change.
	pub fn floor(&self) -> u64 {
		self.inner.floor.unwrap_or(self.inner.start.group)
	}

	/// Whether it answers a reader's time start, whose first group is the one containing the
	/// time from the [`floor`](Self::floor) through it, even when the floor is the time
	/// itself: a drop then says no group starts there, and a refusal fails the reader.
	pub fn is_time_start(&self) -> bool {
		self.inner.floor.is_some()
	}

	/// Whether this fill asks for a separate flight copy of one group rather than the shared
	/// cache slot.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn is_flight(&self) -> bool {
		self.inner.flight
	}

	/// The frame group `key` starts from: [`start`](Self::start)'s frame for the first
	/// group, 0 for the rest.
	pub fn frame_start(&self, key: u64) -> u64 {
		self.inner.frame_start(key)
	}

	/// The exclusive end group.
	pub fn end(&self) -> u64 {
		self.inner.end
	}

	/// The most urgent delivery priority among the readers waiting on it now.
	pub fn priority(&self) -> i8 {
		self.inner.asks.read().priority()
	}

	/// Which end to fill first now: [`Order::Descending`] is newest first.
	pub fn order(&self) -> Order {
		self.inner.asks.read().order()
	}

	/// Insert group `key` into the track's cache, handing it to the readers waiting on
	/// it, and return a [`group::Producer`] to fill.
	///
	/// On a track this publisher accepted, a group past every one it produced is its live feed,
	/// which its subscriptions deliver too; one below is history, served only to whoever misses
	/// it. A relay's copy leaves that to its upstream's subscription.
	///
	/// The group starts at [`start`](Self::start)'s frame when it is the first group, so the
	/// frames written keep the indices they have in the group. A key from the
	/// [`floor`](Self::floor) up to the start's answers the start with that group, from its
	/// first frame. `info` sets the track's [`Info`] if it hasn't been accepted yet (a fetch
	/// with no live subscription), and is ignored otherwise. Returns `None` if the group was
	/// already answered or cached. Errors with [`Error::InvalidRange`] outside the fill,
	/// [`Error::Skipped`] if the track skipped it, or the track's abort error.
	pub fn accept(&self, key: u64, info: impl Into<Option<Info>>) -> Result<Option<group::Producer>> {
		self.inner.accept(key, info.into())
	}

	/// Name `groups` as never to be delivered: the source no longer has them. Readers stop
	/// waiting for them, and the track records them so nobody asks again. Only the groups a
	/// reader still reads count; the rest of the fill was never owed.
	pub fn drop_groups(&self, groups: ops::Range<u64>) {
		let owed = self.inner.owed(groups);
		self.inner
			.drop_groups(&owed, Error::NotFound, super::Hole::drop(StreamError::NotFound));
	}

	/// Refuse `groups` with `err` without saying the source lost them, as when serving them
	/// failed for now or this request asks what the source can't serve. The readers waiting
	/// now stop waiting, and a later reader asks again. Only the groups a reader still reads
	/// count, as with [`Self::drop_groups`].
	pub fn refuse_groups(&self, groups: ops::Range<u64>, err: Error) {
		let owed = self.inner.owed(groups);
		self.inner.refuse(&owed, err);
	}

	/// Name `groups` as never produced, as the source answered a request for them: readers
	/// pass over them, as over a skip the track recorded, however soon the cache forgets it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn skip_groups(&self, groups: ops::Range<u64>) {
		let owed = self.inner.owed(groups);
		self.inner.drop_groups(&owed, Error::Skipped, super::Hole::Skip);
	}

	/// Poll until no reader waits on this fill: it is withdrawn, so stop serving it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_withdrawn(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.result.poll_wanted_changed(waiter, true).map(drop)
	}

	/// Whether a reader still waits for group `key` from this fill: false once every
	/// reader took it or narrowed past it, or nobody waits on the fill. A group stops being
	/// wanted for good; once accepted and taken, [`group::Producer::demand`](crate::group::Producer::demand)
	/// says who reads it.
	pub fn is_wanted(&self, key: u64) -> bool {
		self.inner.is_wanted(key)
	}

	/// Resolves once no reader waits for group `key` from this fill, so stop serving it.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_unwanted(&self, key: u64, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_unwanted(key, waiter)
	}

	/// The fill's id, which its holds go by.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn id(&self) -> u64 {
		self.inner.id
	}

	/// How many answer runs the fill still holds for its readers.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn answers(&self) -> usize {
		let outcome = self.inner.result.read();
		outcome.refused.iter().count() + outcome.accepted.iter().count()
	}

	/// The runs answered so far, even once their readers consumed the answers.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn resolved_runs(&self) -> Vec<ops::Range<u64>> {
		self.inner.result.read().resolved.iter().cloned().collect()
	}

	/// The next group still owed in the fill's order: a reader still reads it, and it was
	/// neither answered nor delivered live since it was asked. `None` once none is owed or
	/// no reader waits on the fill, so stop serving it. It does not wait for the readers to
	/// take what was served before it; [`poll_wanted_changed`](Self::poll_wanted_changed) does.
	pub fn wanted(&self) -> Option<u64> {
		self.wanted_except(None)
	}

	/// [`Self::wanted`], passing over `skip`.
	fn wanted_except(&self, skip: Option<u64>) -> Option<u64> {
		if !self.inner.result.wanted() {
			return None;
		}
		loop {
			let wanted = self.inner.wanted();
			let key = {
				let outcome = self.inner.result.read();
				let mut gaps = wanted
					.iter()
					.flat_map(|run| outcome.resolved.gaps(run))
					.flat_map(|gap| without(gap, skip));
				match self.order() {
					Order::Ascending => gaps.next().map(|gap| gap.start),
					Order::Descending => gaps.last().map(|gap| gap.end - 1),
				}
			};
			let front = self.inner.untaken_front();
			let Some(key) = key else {
				self.inner.sync_holds(front, None);
				return None;
			};
			// The live feed may have delivered it since: answered by the cache, not the source.
			// Never a flight's, which the cached copy shed.
			let cached = !self.inner.flight
				&& self
					.inner
					.state
					.read()
					.covering_range(key, self.inner.bounds(key))
					.is_some();
			if !cached {
				// Waiting on a run, the fill holds its first group to land, unless it holds one
				// a reader has yet to take.
				let awaiting = front
					.is_none()
					.then(|| (self.run_from(key, self.inner.owed_around(key)), self.order()));
				self.inner.sync_holds(front, awaiting);
				return Some(key);
			}
			// Held as it is answered, if it is the first a reader has yet to take and a reader
			// fronts the fill: the pool may evict anything else before the reader gets to it.
			// Checked again under the lock that holds it, since an eviction may have taken it
			// since.
			if front.is_none() && self.inner.fills.read().fronted(self.inner.id) {
				let Ok(mut state) = TrackState::modify(&self.inner.state) else {
					return Some(key);
				};
				if state.covering_range(key, self.inner.bounds(key)).is_none() {
					continue;
				}
				state.hold_fill(self.inner.id, Some(key), None);
			}
			if !self.inner.answer_cached(key) {
				return Some(key);
			}
		}
	}

	/// The next group to serve: [`wanted`](Self::wanted) once it differs from what this handle
	/// last returned and every group this fill answered reached a reader, which picks it up
	/// when it next reads. `None` once the fill is withdrawn, or answered and taken: keep the
	/// fill until then, since the cache holds what a reader has yet to take only while the
	/// fill lives. The first call returns at once, unless a group already answered has yet to
	/// reach a reader.
	///
	/// So `while let Some(key) = fill.wanted_changed().await` writes one group at a time
	/// as the fastest reader reads on. The cache holds the first group in the fill's order that
	/// a reader has yet to take, which no eviction takes before it does: a fill holds at most
	/// one group, however far its readers fall behind, and a reader asks again for any other
	/// it loses. A reader reading in another order than the fill's picks up groups it cannot
	/// take yet, so the fill moves on.
	#[cfg_attr(feature = "async", kio::wrapper)]
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
		let waits = match wanted {
			Some(_) => self.inner.is_unseen(),
			None => self.inner.untaken_front().is_some(),
		};
		if self.prev == Some(wanted) || waits {
			return Poll::Pending;
		}
		self.prev = Some(wanted);
		Poll::Ready(wanted)
	}

	/// Poll until every group this fill answered reached a reader, or nobody waits on the
	/// fill, as [`Self::poll_wanted_changed`] paces.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_paced(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.poll_withdrawn(waiter).is_ready() {
			return Poll::Ready(());
		}
		let id = self.inner.id;
		self.inner
			.fills
			.poll(waiter, |fills| match fills.pending.get(&id) {
				Some(pending) if !pending.unseen.is_empty() => Poll::Pending,
				_ => Poll::Ready(()),
			})
			.map(drop)
	}

	/// Poll until a reader took every group this fill answered, or nobody waits on the fill:
	/// the cache holds what a reader has yet to take only while the fill lives.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_taken(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.poll_withdrawn(waiter).is_ready() {
			return Poll::Ready(());
		}
		let _ = self.inner.fills.poll(waiter, |_| Poll::<()>::Pending);
		let _ = self.inner.asks.poll(waiter, |_| Poll::<()>::Pending);
		match self.inner.untaken_front() {
			// The hold moves on as readers take what it answered.
			Some(front) => {
				self.inner.sync_holds(Some(front), None);
				Poll::Pending
			}
			None => Poll::Ready(()),
		}
	}

	/// What this fill held that a debt evicted with no room to hold it again, answered or
	/// still landing: asking for it again would see it evicted the same way.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn unfit(&self) -> RangeSet<u64> {
		self.inner.result.read().unfit.clone()
	}

	/// Whether a group this fill answered has yet to reach a reader, so it asks for nothing
	/// more for now.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn is_paced(&self) -> bool {
		self.inner.is_unseen()
	}

	/// Whether a reader has yet to take a group this fill answered, which the cache holds
	/// only while the fill lives.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn is_holding(&self) -> bool {
		self.inner.untaken_front().is_some()
	}

	/// Refuse every group still owed with `err`, which says nothing about whether the source
	/// has them, so nothing is recorded dropped.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn refuse(&self, err: Error) {
		let unresolved = self.inner.unresolved(self.inner.start.group..self.inner.end);
		self.inner.refuse(&unresolved, err);
	}

	/// The next run of groups still owed, in the fill's order: [`Self::wanted`]'s group,
	/// extended over the owed groups beside it up to the nearest one the cache holds, so a
	/// session asks its source for the whole run at once.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn next_run(&self) -> Option<ops::Range<u64>> {
		let key = self.wanted()?;
		Some(self.run_from(key, self.inner.owed_around(key)))
	}

	/// [`Self::next_run`], passing over `skip`: a group another request is already bringing,
	/// so the groups around it are served while it waits. `None` once nothing else is owed.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn next_run_except(&self, skip: u64) -> Option<ops::Range<u64>> {
		let key = self.wanted_except(Some(skip))?;
		let owed = self.inner.owed_around(key);
		let owed = match skip < key {
			true => owed.start.max(skip + 1)..owed.end,
			false => owed.start..owed.end.min(skip),
		};
		Some(self.run_from(key, owed))
	}

	/// The run from `key` over the `owed` groups beside it, in the fill's order, up to
	/// the nearest one the cache holds.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn run_from(&self, key: u64, owed: ops::Range<u64>) -> ops::Range<u64> {
		let state = self.inner.state.read();
		let held = |(_, slot): &(&u64, &super::Slot)| !slot.is_aborted();
		match self.order() {
			Order::Ascending => {
				let end = state
					.lookup
					.range(key + 1..owed.end)
					.find(held)
					.map_or(owed.end, |(held, _)| *held);
				key..end
			}
			Order::Descending => {
				let start = state
					.lookup
					.range(owed.start..key)
					.rev()
					.find(held)
					.map_or(owed.start, |(held, _)| held + 1);
				start..key + 1
			}
		}
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

	/// Refuse `groups` with the source's answer to a request for them: only [`Error::Old`]
	/// under the publisher's max age (with every older group of the fill) is recorded, as a
	/// stale verdict under that age.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn reject_groups(&self, groups: ops::Range<u64>, err: Error) {
		self.inner.reject(groups, err);
	}

	/// Declare the track's exclusive final key, as the source answering a request
	/// reported it. A no-op once the track declared an end, or holds a later group.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn finish_track_at(&self, final_key: u64) {
		self.inner.finish_track_at(final_key);
	}

	/// A handle on group `key` alone, for a session serving the fill one group at a time.
	/// It keeps what the readers ask now, which is what its source gets asked.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn group(&self, key: u64) -> GroupFill {
		GroupFill {
			inner: self.inner.clone(),
			key,
			frame_start: self.inner.frame_start(key),
			frame_end: self.inner.bounds(key).end,
			priority: self.priority(),
		}
	}
}

impl Inner {
	fn contains(&self, key: u64) -> bool {
		(self.floor.unwrap_or(self.start.group)..self.end).contains(&key)
	}

	fn is_wanted(&self, key: u64) -> bool {
		self.result.wanted() && self.fills.read().wants(self.id, key)
	}

	/// Whether a group this fill answered has yet to reach a reader.
	fn is_unseen(&self) -> bool {
		self.fills
			.read()
			.pending
			.get(&self.id)
			.is_some_and(|pending| !pending.unseen.is_empty())
	}

	/// The first group in the fill's order that it answered and no reader took yet, which the
	/// fill holds.
	fn untaken_front(&self) -> Option<u64> {
		let order = self.asks.read().order();
		let fills = self.fills.read();
		let untaken = &fills.pending.get(&self.id)?.untaken;
		match order {
			Order::Ascending => untaken.iter().next().map(|run| run.start),
			Order::Descending => untaken.iter().next_back().map(|run| run.end - 1),
		}
	}

	/// Hold `front` and await `awaiting` for the fill's readers from now on, or nothing while
	/// no reader fronts the fill, unless the track does already: a change takes the track's
	/// lock, which wakes everything parked on it. A flight's copy is its readers' own, which no
	/// eviction takes from them.
	fn sync_holds(&self, front: Option<u64>, awaiting: Option<(ops::Range<u64>, Order)>) {
		if self.flight {
			return;
		}
		let (front, awaiting) = match self.fills.read().fronted(self.id) {
			true => (front, awaiting),
			false => (None, None),
		};
		if self.state.read().fill_holds_as(self.id, front, &awaiting) {
			return;
		}
		if let Ok(mut state) = TrackState::modify(&self.state) {
			state.hold_fill(self.id, front, awaiting);
		}
	}

	/// Answer `key` from the cache, for the readers that still read it to take. Returns
	/// whether the answer went out.
	fn answer_cached(&self, key: u64) -> bool {
		// Recorded before the answer goes out, as `accept_from` does: a reader may pick it up
		// and take it the moment it does, which must clear it.
		let untaken = {
			let mut fills = self.fills.lock();
			match fills.pending.get_mut(&self.id) {
				Some(pending) if pending.wants.contains_key(&key) => {
					pending.unseen.insert(key..key + 1);
					pending.untaken.insert(key..key + 1);
					true
				}
				_ => false,
			}
		};
		let Ok(mut outcome) = self.result.write() else {
			if untaken && let Some(pending) = self.fills.lock().pending.get_mut(&self.id) {
				pending.unseen.remove(key..key + 1);
				pending.untaken.remove(key..key + 1);
			}
			return false;
		};
		outcome.accept(key..key + 1);
		drop(outcome);
		uncover(&self.fills, self.id, key..key + 1);
		true
	}

	fn poll_unwanted(&self, key: u64, waiter: &kio::Waiter) -> Poll<()> {
		if self.result.poll_wanted_changed(waiter, true).is_ready() {
			return Poll::Ready(());
		}
		self.fills
			.poll(waiter, |fills| match fills.wants(self.id, key) {
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

	fn frame_start(&self, key: u64) -> u64 {
		match key == self.start.group {
			true => self.start.frame,
			false => 0,
		}
	}

	fn bounds(&self, key: u64) -> group::Bounds {
		group::Bounds {
			start: self.frame_start(key),
			end: if key.checked_add(1) == Some(self.end) {
				self.frame_end
			} else {
				None
			},
		}
	}

	fn accept(&self, key: u64, info: Option<Info>) -> Result<Option<group::Producer>> {
		self.accept_from(key, info, self.bounds(key))
	}

	fn accept_from(&self, key: u64, info: Option<Info>, frames: group::Bounds) -> Result<Option<group::Producer>> {
		if !self.contains(key) {
			return Err(Error::InvalidRange);
		}
		let Some(end) = key.checked_add(1) else {
			return Err(Error::InvalidRange);
		};
		// A key below the start answers the start, so no other group starts from it through
		// the start.
		let answered = match key < self.start.group {
			true => key..self.start.group + 1,
			false => key..end,
		};
		if self.result.read().resolved.overlaps(&answered) {
			return Ok(None);
		}
		// Written for a reader that has yet to pick it up, which the next group waits on, and
		// to take it, which the cache holds it for.
		// Recorded before the group is cached, since a reader may take it from the cache and
		// release it before the answer goes out, which must clear it.
		let untaken = {
			let mut fills = self.fills.lock();
			match fills.pending.get_mut(&self.id) {
				Some(pending) if pending.wants.contains_key(&key) => {
					pending.unseen.insert(key..end);
					pending.untaken.insert(key..end);
					true
				}
				_ => false,
			}
		};
		let front = self.untaken_front();
		let fronted = self.fills.read().fronted(self.id);
		// Cache the group before answering: readers resolve an accept through the cache. Held
		// as it is cached, if it is the first a reader has yet to take and this is that
		// reader's front fill: the pool may evict anything else before the reader gets to it. A time start answered below its time
		// holds the group it names, created or cached, under the same lock, so its End stays
		// known once the cache lets it go.
		let mut answer = None;
		let res = TrackState::modify(&self.state).and_then(|mut state| {
			let res = match self.flight {
				true => state.insert_flight(key, frames, info).map(Some),
				false => state.insert_group_request(key, frames, info),
			};
			if !self.flight && fronted && matches!(res, Ok(Some(_))) {
				state.hold_fill(self.id, front, None);
			}
			if key < self.start.group {
				answer = match &res {
					Ok(Some(group)) => Some(group.handle()),
					_ => state.cached_handle(key),
				};
			}
			let deferred = state.deferred();
			drop(state);
			deferred.wake();
			res
		});
		if untaken
			&& !matches!(res, Ok(Some(_)))
			&& let Some(pending) = self.fills.lock().pending.get_mut(&self.id)
		{
			pending.unseen.remove(key..end);
			pending.untaken.remove(key..end);
		}
		if let Ok(mut outcome) = self.result.write() {
			outcome.accept(key..end);
			outcome.resolved.insert(answered);
			if let Ok(Some(group)) = &res
				&& (self.flight || frames.start > 0 || frames.end.is_some())
			{
				outcome.partial.insert(key, group.handle());
			}
			if answer.is_some() {
				outcome.answer = answer;
			}
		}
		uncover(&self.fills, self.id, key..end);
		res
	}

	/// Refuse `groups` with `err`, as the source answered, recording them in the track's drops
	/// as `hole` says, so the later readers it settles do not ask again. A time-start lookup
	/// (`floor.is_some()`) only answers where the time falls and records no drop on the track.
	fn drop_groups(&self, groups: &RangeSet<u64>, err: Error, hole: super::Hole) {
		if groups.is_empty() {
			return;
		}
		if self.floor.is_none()
			&& let Ok(mut state) = TrackState::modify(&self.state)
		{
			for groups in groups.iter() {
				state.drop_unfed(groups.clone(), hole.clone());
			}
		}
		self.answer(groups, err, Some(hole));
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

	fn refuse(&self, groups: &RangeSet<u64>, err: Error) {
		self.answer(groups, err, None);
	}

	/// Answer `groups` with `err`, and the `hole` the source named when it has none of them.
	fn answer(&self, groups: &RangeSet<u64>, err: Error, hole: Option<super::Hole>) {
		if groups.is_empty() {
			return;
		}
		if let Ok(mut outcome) = self.result.write() {
			for groups in groups.iter() {
				outcome.refuse(groups.clone(), err.clone(), hole.clone());
			}
		}
		for groups in groups.iter() {
			uncover(&self.fills, self.id, groups.clone());
		}
	}

	/// Refuse `groups` with `err`, as their source answered a request for them.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn reject(&self, groups: ops::Range<u64>, err: Error) {
		// Hoisted: a guard in the scrutinee would live through the arms, deadlocking against
		// the drop's write.
		let bound = self.state.read().max_age_bound();
		match (err, bound) {
			// Past the publisher's max age, and so is every group older than it: a timed track's
			// starts never rewind. The bound binds every reader, so the track holds it as a stale
			// verdict under that age: retryable, it lasts only until new terms or the drop set's
			// expiry window forget it.
			(Error::Old, Some(bound)) => {
				let older = self.start.group..groups.end;
				self.drop_groups(&self.owed(older), Error::Old, super::Hole::stale(bound));
			}
			// Anything else, a miss or an age not known here included, answers only this request:
			// one starting at another frame may still be served, such as the tail of a group whose
			// head expired. Refused for now, recorded nowhere.
			(err, _) => {
				let unresolved = self.unresolved(groups);
				self.refuse(&unresolved, err);
			}
		}
	}

	/// The run of owed keys around `key`: read by a reader and unanswered.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn owed_around(&self, key: u64) -> ops::Range<u64> {
		let wanted = self.wanted();
		let outcome = self.result.read();
		wanted
			.overlapping(&(key..key + 1))
			.flat_map(|run| outcome.resolved.gaps(run))
			.find(|gap| gap.contains(&key))
			.unwrap_or(key..key + 1)
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn finish_track_at(&self, final_key: u64) {
		if let Ok(mut state) = TrackState::modify(&self.state)
			&& state.declared().is_none()
		{
			let _ = state.set_end(Position::group(final_key), None);
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
			if self.floor.is_none()
				&& let Ok(mut state) = TrackState::modify(&self.state)
			{
				// Unanswered is a miss downstream, as an explicit `drop_groups` would say.
				for groups in left.iter() {
					state.drop_unfed(groups.clone(), super::Hole::drop(StreamError::NotFound));
				}
			}
			if let Ok(mut outcome) = self.result.write() {
				for groups in left.iter() {
					outcome.refuse(
						groups.clone(),
						Error::Dropped,
						Some(super::Hole::drop(StreamError::NotFound)),
					);
				}
			}
		}
		// What it held is the cache's again: a reader that has yet to take it asks again if
		// it is evicted.
		if !self.flight
			&& !self.state.read().fill_holds_as(self.id, None, &None)
			&& let Ok(mut state) = TrackState::modify(&self.state)
		{
			state.release_fill(self.id);
		}
		self.fills.lock().end(self.id);
	}
}

impl TrackState {
	/// Record `groups` as dropped, as a fill's source answered, except what the cache holds
	/// or the live feed may still deliver: a fill refused is no verdict on the feed.
	fn drop_unfed(&mut self, groups: ops::Range<u64>, hole: super::Hole) {
		// Until the serving session resolves where the feed starts, it may deliver anything.
		if matches!(self.start, super::FeedStart::Pending(_)) {
			return;
		}
		let now = self.cache.pool().now_wall();
		let mut dropped = RangeSet::new();
		for hole in super::miss::holes(self, groups) {
			// A group lost to the cache since it arrived is the source's to name too: the live
			// feed delivers it no more.
			let unfed = self.unfed(hole, now);
			for groups in unfed.never.iter().chain(unfed.lost.iter()) {
				dropped.insert(groups.clone());
			}
		}
		for groups in dropped.iter() {
			self.drop_groups(groups.clone(), hole.clone());
		}
		// The runs are billed, so a source refusing fills in fragments pays for them.
		if !dropped.is_empty() {
			self.charge_debt();
		}
	}
}

/// The readers of one group of a fill, outliving its [`GroupFill`].
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct Readers {
	fills: kio::Shared<FillState>,
	id: u64,
	key: u64,
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
				Some(pending) if !pending.wants.contains_key(&self.key) => Poll::Ready(()),
				_ => Poll::Pending,
			})
			.map(drop)
	}
}

/// One group of a [`FillGroups`], for a session serving it a group at a time. Answering it answers
/// that group of the fill; dropping it unanswered leaves the group to the fill.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct GroupFill {
	inner: Arc<SendCell<Inner>>,
	key: u64,
	frame_start: u64,
	frame_end: Option<u64>,
	priority: i8,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl GroupFill {
	/// The group key to fetch.
	pub(crate) fn key(&self) -> u64 {
		self.key
	}

	/// The delivery priority the readers asked for when the group was taken.
	pub(crate) fn priority(&self) -> i8 {
		self.priority
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
	pub(crate) fn accept_from(&self, info: Info, frames: group::Bounds) -> Result<Option<group::Producer>> {
		self.inner.accept_from(self.key, Some(info), frames)
	}

	/// Poll until no reader waits on this group, as [`FillGroups::poll_unwanted`].
	pub(crate) fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_unwanted(self.key, waiter)
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
			key: self.key,
			outcome: self.inner.result.clone(),
		}
	}

	/// Insert the group into the cache, as [`FillGroups::accept`].
	#[cfg(test)]
	pub(crate) fn accept(self, info: impl Into<Option<Info>>) -> Result<Option<group::Producer>> {
		self.inner.accept(self.key, info.into())
	}

	/// Refuse the group with the source's answer, for the readers waiting now. Only
	/// [`Error::Old`] under the publisher's max age (with every older group of the fill)
	/// is recorded, as a stale verdict under that age.
	pub(crate) fn reject(self, err: Error) {
		self.inner.reject(self.key..self.key + 1, err);
	}

	/// Declare the group never produced (SUBSCRIBE_DROP with SKIP), so waiting readers pass
	/// over it without recording a loss.
	pub(crate) fn skip(self) {
		let owed = self.inner.owed(self.key..self.key + 1);
		self.inner.drop_groups(&owed, Error::Skipped, super::Hole::Skip);
	}
}

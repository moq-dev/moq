//! Incremental demand, separate from the track's cached content.
use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	task::Poll,
	time::Duration,
};

use super::super::subscription::{Joining, max_of};
use super::{JoinAsk, Order, Position, Range, Start, Subscription, TrackState, clamp_combined};

mod ranges;
use ranges::Ranges;

/// How many answers to a received copy's live joins were recorded, on any track: a live join
/// range a reader adds notes it, and takes only an answer recorded since (see
/// [`Prefs::since`]). Counted process-wide, so a front's reader and the copies it reads
/// compare their ticks.
static JOIN_TICK: AtomicU64 = AtomicU64::new(0);

/// How many join answers were recorded: see [`answered`].
pub(in crate::model) fn join_tick() -> u64 {
	JOIN_TICK.load(Ordering::SeqCst)
}

/// Note an answer to a live join recorded, returning its tick: greater than that of every
/// range added before.
pub(super) fn answered() -> u64 {
	JOIN_TICK.fetch_add(1, Ordering::SeqCst) + 1
}

/// A track's demand, under one lock shared by its producer and readers.
#[derive(Clone, Default)]
pub(in crate::model) struct SubscriptionsLock {
	lock: kio::Lock<Subscriptions>,
	/// Guards out now, so a test can tell a wake runs with the lock released.
	#[cfg(test)]
	held: Arc<std::sync::atomic::AtomicUsize>,
}

impl SubscriptionsLock {
	/// Lock the demand, blocking until it is free.
	pub(super) fn lock(&self) -> SubscriptionsGuard<'_> {
		let guard = self.lock.lock();
		#[cfg(test)]
		self.held.fetch_add(1, Ordering::SeqCst);
		SubscriptionsGuard {
			guard,
			#[cfg(test)]
			held: &self.held,
		}
	}

	/// Whether a guard is out, as a waker running under it would see.
	#[cfg(test)]
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn is_held(&self) -> bool {
		self.held.load(Ordering::SeqCst) > 0
	}
}

/// The locked demand. Releases the lock on drop.
pub(super) struct SubscriptionsGuard<'a> {
	guard: kio::LockGuard<'a, Subscriptions>,
	#[cfg(test)]
	held: &'a std::sync::atomic::AtomicUsize,
}

impl std::ops::Deref for SubscriptionsGuard<'_> {
	type Target = Subscriptions;

	fn deref(&self) -> &Subscriptions {
		&self.guard
	}
}

impl std::ops::DerefMut for SubscriptionsGuard<'_> {
	fn deref_mut(&mut self) -> &mut Subscriptions {
		&mut self.guard
	}
}

#[cfg(test)]
impl Drop for SubscriptionsGuard<'_> {
	fn drop(&mut self) {
		self.held.fetch_sub(1, Ordering::SeqCst);
	}
}

#[derive(Default)]
pub(super) struct Subscriptions {
	count: usize,
	priority: BTreeMap<i8, usize>,
	/// Each non-empty range's staleness budget, counted: a history range's is the widest of all.
	max_delay: BTreeMap<Duration, usize>,
	ranges: Ranges,
	/// The live joins already placed (see [`Subscription::placed`]), by the group each was
	/// placed at and its end, with each one's staleness budget counted. Kept out of
	/// [`Self::ranges`]: a placed join starts where it was placed, whatever its lag, so it
	/// merges with no other range.
	placed: BTreeMap<(u64, Option<u64>), BTreeMap<Duration, usize>>,
	/// History ranges placed at an exact group (lite-07 Position), by group and end. Kept out
	/// of [`Self::ranges`] so they never trigger time-containment lookup, and out of
	/// [`Self::placed`] so publisher max-age clamping never expires them.
	exact: BTreeMap<(u64, Option<u64>), usize>,
	descending: usize,
	/// Subscribers that need the live feed; see [`Subscription::feed`].
	feed: usize,
	/// Where resuming subscribers resume; see [`Subscription::resume`].
	resumes: BTreeMap<Position, usize>,
	/// How many subscribers resume, so the aggregate asks without summing [`Self::resumes`].
	resuming: usize,
	/// The time starts a received copy's readers wait on, by time, counted: see
	/// [`Subscription::asks`].
	asks: BTreeMap<u64, usize>,
	/// The time starts whose upstream lookup failed, by time and then by the [`Self::refusals`]
	/// each refusal was at: the readers that waited then, counted apart from [`Self::asks`]
	/// until each lets go, and what they fail with. Kept per refusal, so a later one fails
	/// only its own readers with its own error.
	refused: BTreeMap<u64, BTreeMap<u64, RefusedTime>>,
	/// Bumped by each refusal, so a reader can tell whether it began waiting before one.
	refusals: u64,
	/// Where capping subscribers stop; see [`Subscription::cap`].
	caps: BTreeMap<Position, usize>,
	/// How many subscribers cap, so the aggregate asks without summing [`Self::caps`].
	capping: usize,
	/// The live joins a received copy's readers wait on, by lag and end: see
	/// [`Subscription::joins`].
	joins: BTreeMap<(Option<Duration>, Option<u64>), Waiting>,
	/// The lowest group consumers want while no refresh is pending, never lowered.
	wanted: u64,
	/// A request for a group past every key written, until a group answers it or its
	/// callers withdraw it. Read against `next` rather than stored as a number, so a key
	/// written without answering it can never leave the ask behind.
	refresh: Option<Pending>,
	/// Bumped whenever a pending refresh ends, so a caller can tell its own from a later one.
	epoch: u64,
	/// One past the highest key written, which a pending refresh asks for.
	next: u64,
	waiters: kio::WaiterList,
	/// [`super::Refreshing`] callers, woken when their request is sent or ends.
	callers: kio::WaiterList,
	/// The track's cache account, told the most urgent priority so its session sheds live
	/// edges in delivery's order. Absent on a track with no account.
	pub(super) account: Option<Arc<crate::model::cache::Track>>,
}

/// The readers waiting on one live join, counted under the `max` each waits with and the
/// newest answer each saw before it began waiting.
#[derive(Default)]
struct Waiting {
	budgets: BTreeMap<Duration, usize>,
	since: BTreeMap<u64, usize>,
}

/// The readers a time start's refusal failed, which still count it.
struct RefusedTime {
	err: crate::Error,
	count: usize,
}

/// A pending request for a group past every key written.
struct Pending {
	/// When it takes effect: at once, or once the track's refresh interval has passed.
	at: crate::time::Instant,
	/// [`super::Refreshing`] callers waiting for it to be sent. The last to drop before then
	/// withdraws it, unless it is `kept`.
	callers: usize,
	/// A join, a shed read, or a peer asked too, and none of those withdraws.
	kept: bool,
	/// Whatever serves the track (an upstream session or the local publisher) read it.
	sent: bool,
}

/// Where a [`super::Refreshing`] caller stands; see [`Subscriptions::request`].
pub(super) enum Requested {
	/// Nothing is left to wait for.
	Done,
	/// Waiting on the pending request of this epoch.
	Waiting(u64),
}

/// Everything the aggregate holds besides its ranges.
type Scalars = (i8, Order, bool, Option<Position>, Option<Position>);

impl Subscriptions {
	pub(super) fn combined(&self) -> Option<Subscription> {
		let (priority, order, feed, resume, cap) = self.scalars()?;
		let mut ranges = self.ranges.union();
		let mut placed = BTreeMap::new();
		// Each placed join once under the widest `max` asked of it; its lag places nothing.
		for (&(group, end), budgets) in &self.placed {
			let (budget, _) = budgets.last_key_value().expect("a counted placed join");
			placed.insert(ranges.len() as u64, group);
			ranges.push(Range {
				start: Start::Live {
					min: Some(Duration::ZERO),
					max: max_of(*budget),
				},
				end,
			});
		}
		// Each exact-group history range once, placed at its exact group.
		for &(group, end) in self.exact.keys() {
			placed.insert(ranges.len() as u64, group);
			ranges.push(Range {
				start: Start::Time(group),
				end,
			});
		}
		Some(Subscription {
			priority,
			ranges,
			order,
			feed,
			resume,
			placed,
			cap,
			asks: self.asks(),
			joins: self.joins(),
		})
	}

	/// Each live join readers wait on, with the widest budget one waits under and the newest
	/// answer one saw before it began.
	fn joins(&self) -> Vec<JoinAsk> {
		(self.joins.iter())
			.filter_map(|(&(min, end), waiting)| {
				Some(JoinAsk {
					min,
					end,
					budget: *waiting.budgets.last_key_value()?.0,
					since: *waiting.since.last_key_value()?.0,
				})
			})
			.collect()
	}

	/// Count a reader waiting on `join`, or uncount one when not `waits`, waking the
	/// aggregate's watchers when what [`Self::joins`] reports moves.
	pub(super) fn join(&mut self, join: JoinAsk, waits: bool) -> kio::WaiterList {
		let waiting = self.joins.entry((join.min, join.end)).or_default();
		let before = (waiting.budgets.last_key_value().map(|(budget, _)| *budget))
			.zip(waiting.since.last_key_value().map(|(since, _)| *since));
		match waits {
			true => {
				add(&mut waiting.budgets, join.budget);
				add(&mut waiting.since, join.since);
			}
			false => {
				remove(&mut waiting.budgets, join.budget);
				remove(&mut waiting.since, join.since);
			}
		}
		let after = (waiting.budgets.last_key_value().map(|(budget, _)| *budget))
			.zip(waiting.since.last_key_value().map(|(since, _)| *since));
		if waiting.budgets.is_empty() {
			self.joins.remove(&(join.min, join.end));
		}
		match before != after {
			true => self.waiters.take(),
			false => Default::default(),
		}
	}

	/// Each time start readers wait on.
	fn asks(&self) -> Vec<u64> {
		self.asks.keys().copied().collect()
	}

	/// Count a reader waiting on time start `time`, or uncount one when not `waits`, waking the
	/// aggregate's watchers when the first starts or the last stops waiting on it. `since` is
	/// the [`Self::refusals`] it began waiting at, which a reader waiting returns.
	pub(super) fn ask(&mut self, time: u64, since: u64, waits: bool) -> kio::WaiterList {
		let moved = match waits {
			true => add(&mut self.asks, time),
			// One a refusal failed is counted there instead.
			false => match self.refused_since(time, since).map(|(epoch, _)| epoch) {
				Some(epoch) => {
					let refusals = self.refused.get_mut(&time).expect("a refused time");
					let refused = refusals.get_mut(&epoch).expect("its refusal");
					refused.count -= 1;
					if refused.count == 0 {
						refusals.remove(&epoch);
						if refusals.is_empty() {
							self.refused.remove(&time);
						}
					}
					false
				}
				None => remove(&mut self.asks, time),
			},
		};
		match moved {
			true => self.waiters.take(),
			false => Default::default(),
		}
	}

	/// The refusals so far, which a reader beginning to wait on a time start notes.
	pub(super) fn refusals(&self) -> u64 {
		self.refusals
	}

	/// Fail the readers waiting on time start `time` now with `err`, as the upstream refused
	/// to place it: they stop counting in [`Self::asks`], and a reader waiting later asks
	/// again. Wakes the aggregate's watchers when any waited.
	pub(super) fn refuse(&mut self, time: u64, err: crate::Error) -> kio::WaiterList {
		let Some(count) = self.asks.remove(&time) else {
			return Default::default();
		};
		self.refusals += 1;
		let refused = RefusedTime { err, count };
		self.refused.entry(time).or_default().insert(self.refusals, refused);
		self.waiters.take()
	}

	/// Why time start `time` failed for a reader waiting on it since refusal `since`, if it did.
	pub(super) fn refusal(&self, time: u64, since: u64) -> Option<crate::Error> {
		Some(self.refused_since(time, since)?.1.err.clone())
	}

	/// The refusal of time start `time` that failed a reader waiting on it since refusal
	/// `since`, with its epoch: the first after `since`, which took the reader out of the asks.
	fn refused_since(&self, time: u64, since: u64) -> Option<(u64, &RefusedTime)> {
		let (&epoch, refused) = self.refused.get(&time)?.range(since + 1..).next()?;
		Some((epoch, refused))
	}

	/// Whether any time start's refusal still fails a reader.
	pub(super) fn has_refusals(&self) -> bool {
		!self.refused.is_empty()
	}

	/// [`Self::combined`] without its ranges, costing none of them.
	pub(super) fn terms(&self) -> Option<Subscription> {
		let (priority, order, feed, resume, cap) = self.scalars()?;
		Some(Subscription {
			priority,
			ranges: Vec::new(),
			order,
			feed,
			resume,
			placed: BTreeMap::new(),
			cap,
			asks: Vec::new(),
			joins: Vec::new(),
		})
	}

	/// The widest staleness budget any range asks under, without materializing the ranges.
	pub(super) fn max_delay(&self) -> Duration {
		self.max_delay
			.last_key_value()
			.map_or(Duration::ZERO, |(delay, _)| *delay)
	}

	fn scalars(&self) -> Option<Scalars> {
		(self.count != 0).then(|| {
			(
				*self.priority.last_key_value().unwrap().0,
				if self.descending == 0 {
					Order::Ascending
				} else {
					Order::Descending
				},
				self.feed != 0,
				// A subscriber asking for the whole group outweighs any resuming one.
				match self.resuming == self.count {
					true => self.resumes.first_key_value().map(|(resume, _)| *resume),
					false => None,
				},
				// So does one asking for the rest of the group past any cap.
				match self.capping == self.count {
					true => self.caps.last_key_value().map(|(cap, _)| *cap),
					false => None,
				},
			)
		})
	}

	pub(super) fn poll_changed(
		&mut self,
		bound: Option<Duration>,
		prev: &mut Option<Subscription>,
		waiter: &kio::Waiter,
	) -> Poll<Option<Subscription>> {
		let next = clamp_combined(self.combined(), bound);
		if next != *prev {
			*prev = next.clone();
			Poll::Ready(next)
		} else {
			waiter.register(&mut self.waiters);
			Poll::Pending
		}
	}

	/// The lowest group start consumers want at `now`, whether or not anyone consumes: the
	/// next free key while a due refresh is pending.
	pub(super) fn wanted(&self, now: crate::time::Instant) -> u64 {
		match self.refresh.as_ref().is_some_and(|pending| pending.at <= now) {
			true => self.next.max(self.wanted),
			false => self.wanted,
		}
	}

	/// Whether consumers want a group past every key written at `now`: the first group of
	/// a track, or a due refresh.
	pub(super) fn asking(&self, now: crate::time::Instant) -> bool {
		self.wanted(now) >= self.next
	}

	/// When a deferred refresh takes effect, if one is pending and not due at `now`.
	pub(super) fn deferred(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
		self.refresh.as_ref().map(|pending| pending.at).filter(|at| *at > now)
	}

	/// What serves the track read the group asked for at `now`: a due refresh is sent,
	/// waking its callers.
	pub(super) fn sent(&mut self, now: crate::time::Instant) -> kio::WaiterList {
		match &mut self.refresh {
			Some(pending) if pending.at <= now && !pending.sent => {
				pending.sent = true;
				self.callers.take()
			}
			_ => Default::default(),
		}
	}

	/// Arm `waiter` for the next change to what [`Self::wanted`] reports, except a deferred
	/// refresh coming due, which the caller arms.
	pub(super) fn watch(&mut self, waiter: &kio::Waiter) {
		waiter.register(&mut self.waiters);
	}

	/// Ask for a group past every key written, taking effect at `at`, as a request nobody
	/// withdraws. Requests while one is pending coalesce into it, the earliest taking effect.
	pub(super) fn refresh(&mut self, at: crate::time::Instant) -> kio::WaiterList {
		self.pend(at, true)
	}

	/// [`Self::refresh`] for a caller that waits for it to be sent and may withdraw it,
	/// returning where the caller stands. One joining a request already sent is done.
	pub(super) fn request(&mut self, at: crate::time::Instant) -> (Requested, kio::WaiterList) {
		if self.refresh.as_ref().is_some_and(|pending| pending.sent) {
			return (Requested::Done, Default::default());
		}
		let wakes = self.pend(at, false);
		(Requested::Waiting(self.epoch), wakes)
	}

	fn pend(&mut self, at: crate::time::Instant, kept: bool) -> kio::WaiterList {
		let fresh = self.refresh.is_none();
		let pending = self.refresh.get_or_insert(Pending {
			at,
			callers: 0,
			kept: false,
			sent: false,
		});
		pending.kept |= kept;
		pending.callers += usize::from(!kept);
		// Only a new request, or an earlier one, changes what is asked for.
		if !fresh && pending.at <= at {
			return Default::default();
		}
		pending.at = at;
		self.waiters.take()
	}

	/// Whether the caller waiting on `epoch` is done: its request was sent or ended.
	pub(super) fn requested(&mut self, epoch: u64, waiter: &kio::Waiter) -> bool {
		let done = self.epoch != epoch || self.refresh.as_ref().is_none_or(|pending| pending.sent);
		if !done {
			waiter.register(&mut self.callers);
		}
		done
	}

	/// The caller waiting on `epoch` gave up: the last caller withdraws a request not yet
	/// sent, unless something else that never withdraws asked too.
	pub(super) fn withdraw(&mut self, epoch: u64) -> [kio::WaiterList; 2] {
		if self.epoch != epoch {
			return Default::default();
		}
		let Some(pending) = self.refresh.as_mut().filter(|pending| !pending.sent) else {
			return Default::default();
		};
		pending.callers -= 1;
		if pending.callers > 0 || pending.kept {
			return Default::default();
		}
		[self.end_refresh(), self.waiters.take()]
	}

	/// The pending refresh ended, answered or withdrawn: its callers are done.
	fn end_refresh(&mut self) -> kio::WaiterList {
		self.refresh = None;
		self.epoch += 1;
		self.callers.take()
	}

	/// Key `key` was written. One at or past the next free key moves it: a
	/// group subscribers receive answers a pending refresh, and anything else moves the
	/// key it asks for.
	pub(super) fn wrote(&mut self, key: u64, answers: bool) -> [kio::WaiterList; 2] {
		let next = self.next;
		self.next = next.max(key.saturating_add(1));
		if self.refresh.is_none() || key < next {
			return Default::default();
		}
		let callers = match answers {
			true => {
				self.wanted = self.wanted.max(key);
				self.end_refresh()
			}
			false => Default::default(),
		};
		[callers, self.waiters.take()]
	}

	/// Mutate counts under one lock, taking wakes only when the aggregate changes.
	///
	/// `moved` is the ranges `old` holds and `new` doesn't, then those `new` holds and `old`
	/// doesn't, so the lock every subscriber shares is held for what changed alone.
	fn replace(
		&mut self,
		old: Option<Contribution>,
		new: Option<Contribution>,
		moved: (&[Held], &[Held]),
	) -> kio::WaiterList {
		let prev = self.scalars();
		let mut reshaped = false;
		// New ranges count before old ones leave, so a range both hold never reads as gone.
		let (gone, came) = moved;
		for (range, _) in came.iter().filter(|(range, _)| !range.is_empty()) {
			add(&mut self.max_delay, range.budget());
		}
		for (range, _) in gone.iter().filter(|(range, _)| !range.is_empty()) {
			remove(&mut self.max_delay, range.budget());
		}
		if let Some(new) = &new {
			add(&mut self.priority, new.priority);
			for held in came {
				reshaped |= self.add_range(held);
			}
			self.descending += usize::from(new.order == Order::Descending);
			self.feed += usize::from(new.feed);
			if let Some(resume) = new.resume {
				add(&mut self.resumes, resume);
				self.resuming += 1;
			}
			if let Some(cap) = new.cap {
				add(&mut self.caps, cap);
				self.capping += 1;
			}
			self.count += 1;
		}
		if let Some(old) = &old {
			remove(&mut self.priority, old.priority);
			for held in gone {
				reshaped |= self.remove_range(held);
			}
			self.descending -= usize::from(old.order == Order::Descending);
			self.feed -= usize::from(old.feed);
			if let Some(resume) = old.resume {
				remove(&mut self.resumes, resume);
				self.resuming -= 1;
			}
			if let Some(cap) = old.cap {
				remove(&mut self.caps, cap);
				self.capping -= 1;
			}
			self.count -= 1;
		}
		let priority = self.priority.last_key_value().map(|(priority, _)| *priority);
		if let Some(account) = &self.account
			&& prev.map(|prev| prev.0) != priority
		{
			account.set_subscriber(priority);
		}
		if reshaped || self.scalars() != prev {
			self.waiters.take()
		} else {
			Default::default()
		}
	}

	pub(super) fn len(&self) -> usize {
		self.count
	}

	/// Count `held` into the ranges, returning whether their aggregate moved.
	fn add_range(&mut self, (range, placed): &Held) -> bool {
		let Some(group) = placed else {
			return self.ranges.add(*range);
		};
		if matches!(range.start, Start::Time(_)) {
			if range.is_empty() {
				return false;
			}
			let key = (*group, range.end);
			let count = self.exact.entry(key).or_default();
			*count += 1;
			return *count == 1;
		}
		let budgets = self.placed.entry((*group, range.end)).or_default();
		let widest = budgets.last_key_value().map(|(budget, _)| *budget);
		add(budgets, range.budget());
		widest != budgets.last_key_value().map(|(budget, _)| *budget)
	}

	/// Take back what [`Self::add_range`] counted, returning whether the aggregate moved.
	fn remove_range(&mut self, (range, placed): &Held) -> bool {
		let Some(group) = placed else {
			return self.ranges.remove(*range);
		};
		if matches!(range.start, Start::Time(_)) {
			if range.is_empty() {
				return false;
			}
			let key = (*group, range.end);
			let count = self.exact.get_mut(&key).expect("a counted exact range");
			*count -= 1;
			if *count == 0 {
				self.exact.remove(&key);
				return true;
			}
			return false;
		}
		let key = (*group, range.end);
		let budgets = self.placed.get_mut(&key).expect("a counted placed join");
		let widest = budgets.last_key_value().map(|(budget, _)| *budget);
		remove(budgets, range.budget());
		let after = budgets.last_key_value().map(|(budget, _)| *budget);
		if budgets.is_empty() {
			self.placed.remove(&key);
		}
		widest != after
	}
}

/// A range as a subscriber holds it, with the group it was [placed](Subscription::placed) at.
pub(super) type Held = (Range, Option<u64>);

/// The ranges `old` holds more of than `new`, then those `new` holds more of than `old`.
fn moved<T: Copy + Eq + std::hash::Hash>(old: &[T], new: &[T]) -> (Vec<T>, Vec<T>) {
	if old == new {
		return Default::default();
	}
	let mut counts = std::collections::HashMap::<T, isize>::with_capacity(old.len());
	for range in old {
		*counts.entry(*range).or_default() += 1;
	}
	for range in new {
		*counts.entry(*range).or_default() -= 1;
	}
	let (mut gone, mut came) = (Vec::new(), Vec::new());
	for (range, count) in counts {
		let into = if count > 0 { &mut gone } else { &mut came };
		into.extend(std::iter::repeat_n(range, count.unsigned_abs()));
	}
	(gone, came)
}

/// Count `value`, returning whether it is new.
fn add<T: Ord>(counts: &mut BTreeMap<T, usize>, value: T) -> bool {
	let count = counts.entry(value).or_default();
	*count += 1;
	*count == 1
}

/// Uncount `value`, returning whether that was its last.
fn remove<T: Ord>(counts: &mut BTreeMap<T, usize>, value: T) -> bool {
	let count = counts.get_mut(&value).expect("registered contribution");
	*count -= 1;
	let last = *count == 0;
	if last {
		counts.remove(&value);
	}
	last
}

/// What one subscriber adds to the aggregate besides its ranges.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Contribution {
	priority: i8,
	order: Order,
	feed: bool,
	resume: Option<Position>,
	cap: Option<Position>,
}

/// A change to a subscriber's preferences that edits its ranges by key: the ranges keyed
/// `retire` go, then each of `add` joins under its key, and the rest stay as they resolved.
/// Costs the ranges it names, however many the subscriber holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct Edit {
	pub retire: Vec<u64>,
	pub add: Vec<(u64, Range)>,
	/// The group each live join among `add` was already placed at, by its key: see
	/// [`Subscription::placed`].
	pub placed: BTreeMap<u64, u64>,
	pub priority: i8,
	pub order: Order,
}

/// A subscriber's preferences as the model holds them: the ranges by the key each was given,
/// so an [`Edit`] costs only what it names, with a log of the keys recent changes touched so
/// a reader catches up on those alone.
pub(in crate::model) struct Prefs {
	pub priority: i8,
	pub order: Order,
	pub feed: bool,
	pub resume: Option<Position>,
	pub cap: Option<Position>,
	/// In key order, which is the order given: a whole replacement keys by position, and each
	/// key an edit adds is past every key before it.
	ranges: BTreeMap<u64, Range>,
	/// The group each live join range was already placed at, by key: see
	/// [`Subscription::placed`].
	placed: BTreeMap<u64, u64>,
	/// The join tick each live join range was added at, by key: see [`Self::since`].
	since: BTreeMap<u64, u64>,
	/// How many ranges join live and follow the live edge: see [`Self::joins_live`].
	joining: usize,
	/// Bumped whenever the ranges change.
	pub revision: u64,
	/// The keys each change since `base` touched, by revision. Held to twice the ranges, so a
	/// reader that fell further behind resolves every range again, at most once per as many
	/// changes as there are ranges.
	log: VecDeque<(u64, u64)>,
	/// The oldest revision [`Self::changes`] can catch a reader up from.
	base: u64,
	/// The revision of the last whole replacement, which every reader behind places afresh.
	restated: u64,
}

/// What changed in a subscriber's ranges since a revision a reader saw.
pub(in crate::model) enum Changes {
	/// The keys the changes touched.
	Keys(BTreeSet<u64>),
	/// More changes than the log holds: the reader compares every range with its own.
	Overflowed,
	/// The ranges were replaced whole, so the reader places every range afresh.
	Restated,
}

impl Prefs {
	/// `value`, its live join ranges added at join tick `tick`.
	fn new(value: Subscription, tick: u64) -> Self {
		let mut prefs = Self {
			priority: value.priority,
			order: value.order,
			feed: value.feed,
			resume: value.resume,
			cap: value.cap,
			ranges: BTreeMap::new(),
			placed: BTreeMap::new(),
			since: BTreeMap::new(),
			joining: 0,
			revision: 0,
			log: VecDeque::new(),
			base: 0,
			restated: 0,
		};
		for (key, range) in (0..).zip(value.ranges) {
			prefs.insert(key, (range, value.placed.get(&key).copied()), tick);
		}
		prefs
	}

	/// The join tick the live join range keyed `key` was added at, 0 for another: a received
	/// copy places it only from an answer recorded since, the live edge having moved since any
	/// earlier one.
	pub fn since(&self, key: u64) -> u64 {
		self.since.get(&key).copied().unwrap_or(0)
	}

	/// How a received copy places the live join range keyed `key`: where it was already
	/// placed, or from an answer recorded since it was added.
	pub fn joining(&self, key: u64) -> Joining {
		match self.placed.get(&key) {
			Some(group) => Joining::At(*group),
			None => Joining::Since(self.since(key)),
		}
	}

	/// These preferences as a [`Subscription`], its ranges in key order. Costs every range.
	pub fn subscription(&self) -> Subscription {
		Subscription {
			priority: self.priority,
			ranges: self.ranges.values().copied().collect(),
			order: self.order,
			feed: self.feed,
			resume: self.resume,
			placed: self.placed_by_index(),
			cap: self.cap,
			asks: Vec::new(),
			joins: Vec::new(),
		}
	}

	/// The group each placed join was placed at, by its index in key order, as
	/// [`Subscription::placed`] holds it.
	fn placed_by_index(&self) -> BTreeMap<u64, u64> {
		(0..)
			.zip(self.ranges.keys())
			.filter_map(|(index, key)| Some((index, *self.placed.get(key)?)))
			.collect()
	}

	/// Every range in key order, with the group each was placed at.
	fn held(&self) -> Vec<Held> {
		(self.ranges.iter())
			.map(|(key, range)| (*range, self.placed.get(key).copied()))
			.collect()
	}

	/// The widest staleness budget any of these ranges gives a group.
	pub fn widest(&self) -> Duration {
		(self.ranges.values())
			.filter(|range| !range.is_empty())
			.map(Range::budget)
			.max()
			.unwrap_or(Duration::ZERO)
	}

	/// Whether a subscriber joins at the live edge: a live join following it. A time start
	/// resumes a known group instead, such as a route switch picking up where the reader
	/// stopped.
	pub fn joins_live(&self) -> bool {
		self.joining > 0
	}

	/// Every range with its key, in key order.
	pub fn ranges(&self) -> impl ExactSizeIterator<Item = (u64, Range)> + Clone + '_ {
		self.ranges.iter().map(|(key, range)| (*key, *range))
	}

	/// The range keyed `key`, if held.
	pub fn range(&self, key: u64) -> Option<Range> {
		self.ranges.get(&key).copied()
	}

	/// The group the live join range keyed `key` was placed at, if it was.
	pub fn placed(&self, key: u64) -> Option<u64> {
		self.placed.get(&key).copied()
	}

	/// The keys whose range or live join placement `ranges` and `joining` hold otherwise,
	/// those these preferences retired included: what a reader holding them edits after
	/// [`Changes::Overflowed`]. Costs every range.
	pub fn differ(&self, ranges: &BTreeMap<u64, Range>, joining: &BTreeMap<u64, Joining>) -> BTreeSet<u64> {
		let retired = ranges.keys().filter(|key| !self.ranges.contains_key(key)).copied();
		let moved = self.ranges.iter().filter_map(|(key, range)| {
			let held = (ranges.get(key), joining.get(key).copied());
			let expected = (range.join().is_some() || self.placed.contains_key(key)).then(|| self.joining(*key));
			(held != (Some(range), expected)).then_some(*key)
		});
		retired.chain(moved).collect()
	}

	/// What changed since `revision`: see [`Changes`].
	pub fn changes(&self, revision: u64) -> Changes {
		if revision < self.restated {
			return Changes::Restated;
		}
		if revision < self.base {
			return Changes::Overflowed;
		}
		let from = self.log.partition_point(|(at, _)| *at <= revision);
		Changes::Keys(self.log.range(from..).map(|(_, key)| *key).collect())
	}

	fn contribution(&self) -> Contribution {
		Contribution {
			priority: self.priority,
			order: self.order,
			feed: self.feed,
			resume: self.resume,
			cap: self.cap,
		}
	}

	fn insert(&mut self, key: u64, (range, placed): Held, tick: u64) {
		self.count(&range, placed, 1);
		if let Some(old) = self.ranges.insert(key, range) {
			self.count(&old, self.placed.get(&key).copied(), -1);
		}
		match placed {
			Some(group) => _ = self.placed.insert(key, group),
			None => _ = self.placed.remove(&key),
		}
		match range.join() {
			Some(_) => _ = self.since.insert(key, tick),
			None => _ = self.since.remove(&key),
		}
	}

	/// The join tick to add `range` under `key` at, `tick` for a new range: a live join that
	/// changes only its `max` keeps the one it was added at, since it stays where it joined.
	fn tick_for(&self, key: u64, range: &Range, tick: u64) -> u64 {
		match self.ranges.get(&key) {
			Some(old) if range.join().is_some() && old.join() == range.join() => self.since(key),
			_ => tick,
		}
	}

	fn remove(&mut self, key: u64) -> Option<Held> {
		let range = self.ranges.remove(&key)?;
		let placed = self.placed.remove(&key);
		self.since.remove(&key);
		self.count(&range, placed, -1);
		Some((range, placed))
	}

	/// Count `range` `by` more into [`Self::joining`]: a placed join waits on no live edge.
	fn count(&mut self, range: &Range, placed: Option<u64>, by: isize) {
		let open = range.is_open() && placed.is_none();
		self.joining = self.joining.wrapping_add_signed(by * isize::from(open));
	}

	/// Note that the ranges keyed `keys` changed.
	fn touched(&mut self, keys: impl IntoIterator<Item = u64>) {
		self.revision += 1;
		let revision = self.revision;
		self.log.extend(keys.into_iter().map(|key| (revision, key)));
		while self.log.len() > 2 * self.ranges.len() + 64 {
			let (at, _) = self.log.pop_front().expect("a logged change");
			self.base = at;
		}
	}

	/// Key the ranges by `keys`, one per range in the order held, keeping each range: every
	/// reader resolves them all again.
	fn rekey(&mut self, keys: impl IntoIterator<Item = u64>) {
		let ranges = std::mem::take(&mut self.ranges);
		let since = std::mem::take(&mut self.since);
		let placed = std::mem::take(&mut self.placed);
		let count = ranges.len();
		let rekeyed: Vec<(u64, u64, Range)> = (keys.into_iter())
			.zip(ranges)
			.map(|(key, (old, range))| (key, old, range))
			.collect();
		for &(key, old, _) in &rekeyed {
			if let Some(&tick) = since.get(&old) {
				self.since.insert(key, tick);
			}
			if let Some(&group) = placed.get(&old) {
				self.placed.insert(key, group);
			}
		}
		self.ranges = rekeyed.into_iter().map(|(key, _, range)| (key, range)).collect();
		debug_assert_eq!(self.ranges.len(), count, "one key per range");
		self.revision += 1;
		self.log.clear();
		self.base = self.revision;
		self.restated = self.revision;
	}

	/// Replace every preference, rekeying the ranges by position when they change: every
	/// reader resolves them all again, its live joins added at join tick `tick`. With
	/// `retune`, ranges that change only a live join's `max` keep their keys, so each stays
	/// where it joined; without, as a restated subscription, every range is placed afresh.
	fn replace(&mut self, next: Subscription, tick: u64, retune: bool) {
		(self.priority, self.order, self.feed, self.resume, self.cap) =
			(next.priority, next.order, next.feed, next.resume, next.cap);
		let placed = self.placed_by_index();
		if self.ranges.values().eq(&next.ranges) && placed == next.placed {
			return;
		}
		let retunes = retune
			&& self.ranges.len() == next.ranges.len()
			&& placed == next.placed
			&& (self.ranges.values().zip(&next.ranges))
				.all(|(old, new)| old == new || (new.join().is_some() && old.join() == new.join()));
		if retunes {
			let changed: Vec<(u64, Range)> = (self.ranges.iter().zip(&next.ranges))
				.filter(|((_, old), new)| old != new)
				.map(|((key, _), new)| (*key, *new))
				.collect();
			for &(key, range) in &changed {
				self.ranges.insert(key, range);
			}
			self.touched(changed.into_iter().map(|(key, _)| key));
			return;
		}
		self.ranges.clear();
		self.placed.clear();
		self.since.clear();
		self.joining = 0;
		for (key, range) in (0..).zip(next.ranges) {
			self.insert(key, (range, next.placed.get(&key).copied()), tick);
		}
		self.revision += 1;
		self.log.clear();
		self.base = self.revision;
		self.restated = self.revision;
	}
}

/// Clones share one contribution, including controls and pending subscriptions.
#[derive(Clone)]
pub(in crate::model) struct Preferences(Arc<Preference>);

struct Preference {
	value: kio::Producer<Prefs>,
	subscriptions: Option<SubscriptionsLock>,
}

impl Preferences {
	pub(super) fn register(state: kio::Ref<'_, TrackState>, value: Subscription) -> Self {
		let subscriptions = (!state.is_closed()).then(|| state.subscriptions.clone());
		drop(state);
		let value = Prefs::new(value, join_tick());
		if let Some(subs) = &subscriptions {
			let ranges = value.held();
			let mut wakes = subs.lock().replace(None, Some(value.contribution()), (&[], &ranges));
			wakes.wake();
		}
		Self(Arc::new(Preference {
			value: kio::Producer::new(value),
			subscriptions,
		}))
	}

	/// Replace every preference, as [`super::Subscriber::update`] does: replaced ranges are
	/// keyed afresh and resolve again, except a live join whose `max` alone changed.
	pub(in crate::model) fn update(&self, next: Subscription) -> crate::Result<()> {
		self.replace(next, true)
	}

	/// Replace every preference, every range placed afresh, as
	/// [`super::Subscriber::restate`] does.
	pub(in crate::model) fn restate(&self, next: Subscription) -> crate::Result<()> {
		self.replace(next, false)
	}

	fn replace(&self, next: Subscription, retune: bool) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		let old = value.held();
		let contribution = value.contribution();
		let next_contribution = Contribution {
			priority: next.priority,
			order: next.order,
			feed: next.feed,
			resume: next.resume,
			cap: next.cap,
		};
		let new: Vec<Held> = (0..)
			.zip(&next.ranges)
			.map(|(index, range)| (*range, next.placed.get(&index).copied()))
			.collect();
		if contribution == next_contribution && old == new {
			return Ok(());
		}
		// Found before taking the lock every subscriber shares.
		let (gone, came) = moved(&old, &new);
		let mut wakes = self.0.subscriptions.as_ref().map(|subs| {
			subs.lock()
				.replace(Some(contribution), Some(next_contribution), (&gone, &came))
		});
		value.replace(next, join_tick(), retune);
		drop(value);
		if let Some(wakes) = &mut wakes {
			wakes.wake();
		}
		Ok(())
	}

	/// Apply `edit`, costing the ranges it names: see [`Edit`]. A key retired that isn't held
	/// is ignored, and a key added again replaces its range.
	pub(in crate::model) fn edit(&self, edit: Edit) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		let contribution = value.contribution();
		let scalars = Contribution {
			priority: edit.priority,
			order: edit.order,
			..contribution
		};
		if contribution == scalars && edit.retire.is_empty() && edit.add.is_empty() {
			return Ok(());
		}
		let tick = join_tick();
		let mut gone = Vec::new();
		let mut came = Vec::new();
		let mut touched = Vec::with_capacity(edit.retire.len() + edit.add.len());
		for key in edit.retire {
			if let Some(range) = value.remove(key) {
				gone.push(range);
				touched.push(key);
			}
		}
		for (key, range) in edit.add {
			let tick = value.tick_for(key, &range, tick);
			if let Some(old) = value.remove(key) {
				gone.push(old);
			}
			let held = (range, edit.placed.get(&key).copied());
			value.insert(key, held, tick);
			came.push(held);
			touched.push(key);
		}
		(value.priority, value.order) = (edit.priority, edit.order);
		if !touched.is_empty() {
			value.touched(touched);
		}
		let next = value.contribution();
		let mut wakes = self
			.0
			.subscriptions
			.as_ref()
			.map(|subs| subs.lock().replace(Some(contribution), Some(next), (&gone, &came)));
		drop(value);
		if let Some(wakes) = &mut wakes {
			wakes.wake();
		}
		Ok(())
	}
}

impl Preferences {
	/// Key the ranges by `keys`, one per range in the order held, as [`Edit`]s then name
	/// them. The ranges stay as they are, so the aggregate does too.
	pub(in crate::model) fn rekey(&self, keys: impl IntoIterator<Item = u64>) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		value.rekey(keys);
		Ok(())
	}

	/// Note each live join range keyed in `since` as added at its tick instead, such as the
	/// one a front's reader added it at, which the copy it reads asked for: an answer recorded
	/// since then places it. Other ranges are left as they are.
	pub(in crate::model) fn stamp(&self, since: impl IntoIterator<Item = (u64, u64)>) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		for (key, tick) in since {
			if let Some(stamped) = value.since.get_mut(&key) {
				*stamped = tick;
			}
		}
		Ok(())
	}

	pub(in crate::model) fn same_channel(&self, other: &Self) -> bool {
		self.0.value.same_channel(&other.0.value)
	}
	pub(in crate::model) fn read(&self) -> kio::Ref<'_, Prefs> {
		self.0.value.read()
	}
	pub(in crate::model) fn consume(&self) -> kio::Consumer<Prefs> {
		self.0.value.consume()
	}
	pub(in crate::model) fn poll_ref<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R, kio::Ref<'_, Prefs>>>
	where
		F: FnOnce(&kio::Ref<'_, Prefs>) -> Poll<R>,
	{
		self.0.value.poll_ref(waiter, f)
	}
}

impl Drop for Preference {
	fn drop(&mut self) {
		if let Some(subs) = &self.subscriptions {
			let value = self.value.read();
			let ranges = value.held();
			let mut wakes = subs.lock().replace(Some(value.contribution()), None, (&ranges, &[]));
			wakes.wake();
		}
	}
}

#[cfg(test)]
impl From<Subscription> for Preferences {
	fn from(value: Subscription) -> Self {
		Self(Arc::new(Preference {
			value: kio::Producer::new(Prefs::new(value, 0)),
			subscriptions: None,
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::subscription::Combined;
	use super::super::{Range, Start};

	/// The ranges an update moves are its multiset difference, in either direction.
	#[test]
	fn an_update_moves_only_the_ranges_it_changes() {
		let range = |start| Range {
			start: Start::Time(start),
			end: None,
		};
		let sorted = |(mut gone, mut came): (Vec<Range>, Vec<Range>)| {
			gone.sort();
			came.sort();
			(gone, came)
		};
		let old = [range(1), range(2), range(2), range(3)];
		assert_eq!(sorted(super::moved(&old, &old)), (vec![], vec![]));
		assert_eq!(
			sorted(super::moved(&old, &[range(3), range(2), range(4), range(1)])),
			(vec![range(2)], vec![range(4)])
		);
		assert_eq!(sorted(super::moved(&[], &old[..2])), (vec![], vec![range(1), range(2)]));
	}
	use super::*;
	use rand::{RngExt, SeedableRng};
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::task::Wake;

	#[test]
	fn counted_demand_matches_a_full_fold() {
		let broadcast = crate::broadcast::Info::default().produce();
		let producer = broadcast.create_track("demand", None).unwrap();
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x64656d616e64);
		let mut slots: Vec<Option<(super::super::Subscriber, Subscription)>> = (0..64).map(|_| None).collect();
		for step in 0..10_000 {
			let index = rng.random_range(0..slots.len());
			let start = |rng: &mut rand::rngs::StdRng| match rng.random_range(0..4) {
				0 => Start::Oldest,
				1 => Start::Live {
					min: (rng.random_bool(0.8)).then(|| Duration::from_millis(rng.random_range(0..8))),
					max: rng
						.random_bool(0.8)
						.then(|| Duration::from_millis(rng.random_range(0..8))),
				},
				_ => Start::Time(rng.random_range(0..8)),
			};
			let ranges: Vec<Range> = (0..rng.random_range(0..3))
				.map(|_| Range {
					start: start(&mut rng),
					end: rng.random_bool(0.5).then(|| rng.random_range(0..8)),
				})
				.collect();
			// Some live joins already placed, as a route switch asks them.
			let mut placed = BTreeMap::new();
			for (index, range) in (0..).zip(&ranges) {
				if range.join().is_some() && rng.random_bool(0.5) {
					placed.insert(index, rng.random_range(0..8));
				}
			}
			let next = Subscription {
				priority: rng.random_range(0..8),
				ranges,
				order: if rng.random_bool(0.5) {
					Order::Ascending
				} else {
					Order::Descending
				},
				feed: rng.random_bool(0.25),
				resume: rng
					.random_bool(0.5)
					.then(|| Position::new(rng.random_range(0..8), rng.random_range(1..4))),
				placed,
				cap: rng
					.random_bool(0.5)
					.then(|| Position::new(rng.random_range(0..8), rng.random_range(1..4))),
				asks: Vec::new(),
				joins: Vec::new(),
			};
			match (&mut slots[index], rng.random_range(0..3)) {
				(slot @ None, _) => *slot = Some((producer.subscribe(next.clone()), next)),
				(Some((subscriber, value)), 0) => {
					subscriber.update(next.clone()).unwrap();
					*value = next;
				}
				(Some((subscriber, value)), 1) => {
					subscriber.control().update(next.clone()).unwrap();
					*value = next;
				}
				(slot, _) => *slot = None,
			}
			let mut expected = Combined::default();
			for (_, value) in slots.iter().flatten() {
				expected.add(value);
			}
			let expected = expected.finish();
			assert_eq!(producer.subscription(), expected, "operation {step}");
		}
		drop(slots);
		assert_eq!(producer.subscription(), None);
	}

	struct Count(AtomicUsize);
	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
		fn wake_by_ref(self: &Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	#[test]
	fn unchanged_demand_does_not_wake_the_publisher() {
		let broadcast = crate::broadcast::Info::default().produce();
		let mut producer = broadcast.create_track("demand", None).unwrap();
		// History from the oldest group covers every range the weak reader adds.
		let strong = producer.subscribe(Subscription::default().with_ranges([Range::ALL]).with_priority(10));
		let count = Arc::new(Count(AtomicUsize::new(0)));
		let waiter = kio::Waiter::new(std::task::Waker::from(count.clone()));
		assert!(producer.poll_subscription_changed(&waiter).is_ready());
		assert!(producer.poll_subscription_changed(&waiter).is_pending());
		let mut weak = producer.subscribe(
			Subscription::default()
				.with_range(100..200)
				.with_order(Order::Ascending),
		);
		weak.update(Subscription::default().with_priority(5)).unwrap();
		drop(weak);
		assert_eq!(count.0.load(Ordering::SeqCst), 0);
		drop(strong);
		assert_eq!(count.0.load(Ordering::SeqCst), 1);
		assert!(matches!(
			producer.poll_subscription_changed(&waiter),
			Poll::Ready(Ok(None))
		));
	}

	#[test]
	fn controls_and_pending_subscriptions_share_one_contribution() {
		let broadcast = crate::broadcast::Info::default().produce();
		let producer = broadcast.create_track("demand", None).unwrap();
		let pending = producer.consume().subscribe(Subscription::default().with_priority(3));
		let Poll::Ready(Ok(subscriber)) = pending.poll_ok(&kio::Waiter::noop()) else {
			panic!("known track");
		};
		let control = subscriber.control();
		drop(pending);
		drop(subscriber);
		assert_eq!(producer.subscription().unwrap().priority, 3);
		control.update(Subscription::default().with_priority(8)).unwrap();
		assert_eq!(producer.subscription().unwrap().priority, 8);
		drop(control);
		assert_eq!(producer.subscription(), None);
	}
}

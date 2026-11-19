//! Incremental demand, separate from the track's cached content.
use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
	task::Poll,
	time::Duration,
};

use super::{Anchor, Delay, Order, Range, Subscription, TrackState, clamp_combined};

mod ranges;
use ranges::Ranges;

#[derive(Default)]
pub(super) struct Subscriptions {
	count: usize,
	priority: BTreeMap<i8, usize>,
	// Keyed by budget, so `None` counts as the widest of all.
	max_delay: BTreeMap<Duration, usize>,
	// Only stated minimums count, so an unstated one never reads as zero.
	min_delay: BTreeMap<Duration, usize>,
	ranges: Ranges,
	descending: usize,
	/// Subscribers that need the live feed; see [`Subscription::feed`].
	feed: usize,
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
type Scalars = (i8, Delay, Order, bool);

impl Subscriptions {
	pub(super) fn combined(&self) -> Option<Subscription> {
		let (priority, delay, order, feed) = self.scalars()?;
		Some(Subscription {
			priority,
			delay,
			ranges: self.ranges.union(),
			order,
			feed,
		})
	}

	/// [`Self::combined`] without its ranges, costing none of them.
	pub(super) fn terms(&self) -> Option<Subscription> {
		let (priority, delay, order, feed) = self.scalars()?;
		Some(Subscription {
			priority,
			delay,
			ranges: Vec::new(),
			order,
			feed,
		})
	}

	/// The largest delay budget without materializing the subscription's ranges.
	pub(super) fn max_delay(&self) -> Duration {
		self.max_delay
			.last_key_value()
			.map_or(Duration::ZERO, |(delay, _)| *delay)
	}

	fn scalars(&self) -> Option<Scalars> {
		(self.count != 0).then(|| {
			(
				*self.priority.last_key_value().unwrap().0,
				Delay {
					// The largest, so an upstream starts far enough back for the reader that buffers most.
					min: self.min_delay.last_key_value().map(|(min, _)| *min),
					max: Some(*self.max_delay.last_key_value().unwrap().0).filter(|budget| *budget != Duration::MAX),
				},
				if self.descending == 0 {
					Order::Ascending
				} else {
					Order::Descending
				},
				self.feed != 0,
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
		moved: (&[Range], &[Range]),
	) -> kio::WaiterList {
		let prev = self.scalars();
		// New ranges count before old ones leave, so a range both hold never reads as gone.
		let mut reshaped = false;
		let (gone, came) = moved;
		if let Some(new) = &new {
			add(&mut self.priority, new.priority);
			add(&mut self.max_delay, new.budget());
			if let Some(min) = new.delay.min {
				add(&mut self.min_delay, min);
			}
			for range in came {
				reshaped |= self.ranges.add(*range);
			}
			self.descending += usize::from(new.order == Order::Descending);
			self.feed += usize::from(new.feed);
			self.count += 1;
		}
		if let Some(old) = &old {
			remove(&mut self.priority, old.priority);
			remove(&mut self.max_delay, old.budget());
			if let Some(min) = old.delay.min {
				remove(&mut self.min_delay, min);
			}
			for range in gone {
				reshaped |= self.ranges.remove(*range);
			}
			self.descending -= usize::from(old.order == Order::Descending);
			self.feed -= usize::from(old.feed);
			self.count -= 1;
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
}

/// The ranges `old` holds more of than `new`, then those `new` holds more of than `old`.
fn moved(old: &[Range], new: &[Range]) -> (Vec<Range>, Vec<Range>) {
	if old == new {
		return Default::default();
	}
	let mut counts = std::collections::HashMap::<Range, isize>::with_capacity(old.len());
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
	delay: Delay,
	order: Order,
	feed: bool,
}

impl Contribution {
	fn budget(&self) -> Duration {
		self.delay.budget()
	}
}

/// A change to a subscriber's preferences that edits its ranges by key: the ranges keyed
/// `retire` go, then each of `add` joins under its key, and the rest stay as they resolved.
/// Costs the ranges it names, however many the subscriber holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct Edit {
	pub retire: Vec<u64>,
	pub add: Vec<(u64, Range)>,
	pub priority: i8,
	pub delay: Delay,
	pub order: Order,
}

/// A subscriber's preferences as the model holds them: the ranges by the key each was given,
/// so an [`Edit`] costs only what it names, with a log of the keys recent changes touched so
/// a reader catches up on those alone.
pub(in crate::model) struct Prefs {
	pub priority: i8,
	pub delay: Delay,
	pub order: Order,
	pub feed: bool,
	/// In key order, which is the order given: a whole replacement keys by position, and each
	/// key an edit adds is past every key before it.
	ranges: BTreeMap<u64, Range>,
	/// How many ranges start at the oldest or latest group with no end: see [`Self::joins_live`].
	joining: usize,
	/// Bumped whenever the ranges change.
	pub revision: u64,
	/// The keys each change since `base` touched, by revision. Held to twice the ranges, so a
	/// reader that fell further behind resolves every range again, at most once per as many
	/// changes as there are ranges.
	log: VecDeque<(u64, u64)>,
	/// The oldest revision [`Self::changes`] can catch a reader up from.
	base: u64,
}

impl Prefs {
	fn new(value: Subscription) -> Self {
		let mut prefs = Self {
			priority: value.priority,
			delay: value.delay,
			order: value.order,
			feed: value.feed,
			ranges: BTreeMap::new(),
			joining: 0,
			revision: 0,
			log: VecDeque::new(),
			base: 0,
		};
		for (key, range) in (0..).zip(value.ranges) {
			prefs.insert(key, range);
		}
		prefs
	}

	/// These preferences as a [`Subscription`], its ranges in key order. Costs every range.
	pub fn subscription(&self) -> Subscription {
		Subscription {
			priority: self.priority,
			delay: self.delay,
			ranges: self.ranges.values().copied().collect(),
			order: self.order,
			feed: self.feed,
		}
	}

	pub fn budget(&self) -> Duration {
		self.delay.budget()
	}

	/// Whether a subscriber joins at the live edge: a range counting back from the latest
	/// group and following it. A fixed start resumes a known group instead, such as a route
	/// switch picking up where the reader stopped.
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

	/// The keys the changes since `revision` touched, or `None` once the log no longer
	/// reaches back that far, when the reader resolves every range again.
	pub fn changes(&self, revision: u64) -> Option<BTreeSet<u64>> {
		if revision < self.base {
			return None;
		}
		let from = self.log.partition_point(|(at, _)| *at <= revision);
		Some(self.log.range(from..).map(|(_, key)| *key).collect())
	}

	fn contribution(&self) -> Contribution {
		Contribution {
			priority: self.priority,
			delay: self.delay,
			order: self.order,
			feed: self.feed,
		}
	}

	fn insert(&mut self, key: u64, range: Range) {
		self.joining += usize::from(joins(&range));
		if let Some(old) = self.ranges.insert(key, range) {
			self.joining -= usize::from(joins(&old));
		}
	}

	fn remove(&mut self, key: u64) -> Option<Range> {
		let range = self.ranges.remove(&key)?;
		self.joining -= usize::from(joins(&range));
		Some(range)
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
		let count = ranges.len();
		self.ranges = keys.into_iter().zip(ranges.into_values()).collect();
		debug_assert_eq!(self.ranges.len(), count, "one key per range");
		self.revision += 1;
		self.log.clear();
		self.base = self.revision;
	}

	/// Replace every preference, rekeying the ranges by position when they change: every
	/// reader resolves them all again.
	fn replace(&mut self, next: Subscription) {
		(self.priority, self.delay, self.order, self.feed) = (next.priority, next.delay, next.order, next.feed);
		if self.ranges.values().eq(&next.ranges) {
			return;
		}
		self.ranges.clear();
		self.joining = 0;
		for (key, range) in (0..).zip(next.ranges) {
			self.insert(key, range);
		}
		self.revision += 1;
		self.log.clear();
		self.base = self.revision;
	}
}

/// Whether `range` starts at the oldest or latest group with no end.
fn joins(range: &Range) -> bool {
	matches!(range.start, Anchor::Oldest | Anchor::Relative(_)) && range.end.is_none()
}

/// Clones share one contribution, including controls and pending subscriptions.
#[derive(Clone)]
pub(in crate::model) struct Preferences(Arc<Preference>);

struct Preference {
	value: kio::Producer<Prefs>,
	subscriptions: Option<kio::Lock<Subscriptions>>,
}

impl Preferences {
	pub(super) fn register(state: kio::Ref<'_, TrackState>, value: Subscription) -> Self {
		let subscriptions = (!state.is_closed()).then(|| state.subscriptions.clone());
		drop(state);
		let value = Prefs::new(value);
		if let Some(subs) = &subscriptions {
			let ranges: Vec<Range> = value.ranges.values().copied().collect();
			let mut wakes = subs.lock().replace(None, Some(value.contribution()), (&[], &ranges));
			wakes.wake();
		}
		Self(Arc::new(Preference {
			value: kio::Producer::new(value),
			subscriptions,
		}))
	}

	/// Replace every preference, as [`super::Subscriber::update`] does: replaced ranges are
	/// keyed afresh and resolve again.
	pub(in crate::model) fn update(&self, next: Subscription) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		let old: Vec<Range> = value.ranges.values().copied().collect();
		let contribution = value.contribution();
		let next_contribution = Contribution {
			priority: next.priority,
			delay: next.delay,
			order: next.order,
			feed: next.feed,
		};
		if contribution == next_contribution && old == next.ranges {
			return Ok(());
		}
		// Found before taking the lock every subscriber shares.
		let (gone, came) = moved(&old, &next.ranges);
		let mut wakes = self.0.subscriptions.as_ref().map(|subs| {
			subs.lock()
				.replace(Some(contribution), Some(next_contribution), (&gone, &came))
		});
		value.replace(next);
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
		let next = Contribution {
			priority: edit.priority,
			delay: edit.delay,
			order: edit.order,
			..contribution
		};
		if contribution == next && edit.retire.is_empty() && edit.add.is_empty() {
			return Ok(());
		}
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
			if let Some(old) = value.remove(key) {
				gone.push(old);
			}
			value.insert(key, range);
			came.push(range);
			touched.push(key);
		}
		(value.priority, value.delay, value.order) = (edit.priority, edit.delay, edit.order);
		if !touched.is_empty() {
			value.touched(touched);
		}
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
			let ranges: Vec<Range> = value.ranges.values().copied().collect();
			let mut wakes = subs.lock().replace(Some(value.contribution()), None, (&ranges, &[]));
			wakes.wake();
		}
	}
}

#[cfg(test)]
impl From<Subscription> for Preferences {
	fn from(value: Subscription) -> Self {
		Self(Arc::new(Preference {
			value: kio::Producer::new(Prefs::new(value)),
			subscriptions: None,
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::subscription::Combined;
	use super::super::{Anchor, Range};

	/// The ranges an update moves are its multiset difference, in either direction.
	#[test]
	fn an_update_moves_only_the_ranges_it_changes() {
		let range = |start| Range {
			start: Anchor::Time(start),
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
			let anchor = |rng: &mut rand::rngs::StdRng| match rng.random_range(0..4) {
				0 => Anchor::Relative(rng.random_range(0..3)),
				1 => Anchor::Oldest,
				_ => Anchor::Time(rng.random_range(0..8)),
			};
			let ranges: Vec<Range> = (0..rng.random_range(0..3))
				.map(|_| Range {
					start: anchor(&mut rng),
					end: rng.random_bool(0.75).then(|| anchor(&mut rng)),
				})
				.collect();
			let next = Subscription {
				priority: rng.random_range(0..8),
				delay: Delay {
					min: rng
						.random_bool(0.75)
						.then(|| Duration::from_millis(rng.random_range(0..8))),
					max: Some(Duration::from_millis(rng.random_range(0..8))),
				},
				ranges,
				order: if rng.random_bool(0.5) {
					Order::Ascending
				} else {
					Order::Descending
				},
				feed: rng.random_bool(0.25),
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

	/// An oldest end is refused wherever it is read, so it asks nothing upstream, even
	/// past a time start.
	#[test]
	fn an_oldest_end_adds_no_demand() {
		let broadcast = crate::broadcast::Info::default().produce();
		let producer = broadcast.create_track("demand", None).unwrap();
		let _sub = producer.subscribe(Subscription::default().with_ranges([Range {
			start: Anchor::Time(3),
			end: Some(Anchor::Oldest),
		}]));
		assert_eq!(producer.subscription().map(|sub| sub.ranges), Some(Vec::new()));
	}

	#[test]
	fn unchanged_demand_does_not_wake_the_publisher() {
		let broadcast = crate::broadcast::Info::default().produce();
		let mut producer = broadcast.create_track("demand", None).unwrap();
		let strong = producer.subscribe(
			Subscription::default()
				.with_priority(10)
				.with_delay(Delay::default().with_max(Duration::from_secs(1))),
		);
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

//! Incremental demand, separate from the track's cached content.
use std::{
	collections::BTreeMap,
	sync::{
		Arc,
		atomic::{self, AtomicU64},
	},
	task::Poll,
	time::Duration,
};

use super::{Order, Sequence, Subscription, TrackState, clamp_combined};

mod ranges;
use ranges::Ranges;

#[derive(Default)]
pub(super) struct Subscriptions {
	count: usize,
	priority: BTreeMap<i8, usize>,
	max_delay: BTreeMap<Duration, usize>,
	ranges: Ranges,
	descending: usize,
	/// The lowest group consumers want while no refresh is pending, never lowered. See
	/// [`super::Demand::wanted`].
	wanted: u64,
	/// A join over the track's refresh ratio asked for a group past every sequence written,
	/// and no such group has landed yet. Read against the namespace rather than stored as a
	/// number, so a datagram taking the next sequence can never leave the ask behind.
	refresh: bool,
	waiters: kio::WaiterList,
}

/// Everything the aggregate holds besides its ranges.
type Scalars = (i8, Duration, Order);

impl Subscriptions {
	pub(super) fn combined(&self) -> Option<Subscription> {
		let (priority, max_delay, order) = self.scalars()?;
		Some(Subscription {
			priority,
			max_delay,
			ranges: self.ranges.union(),
			order,
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
				*self.max_delay.last_key_value().unwrap().0,
				if self.descending == 0 {
					Order::Ascending
				} else {
					Order::Descending
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

	/// The lowest group consumers want, whether or not anyone consumes now: the next free
	/// sequence of `namespace` while a refresh is pending.
	///
	/// Read under this lock, so a write that moves the namespace after it wakes the waiters
	/// armed here (see [`Self::wrote`]).
	pub(super) fn wanted(&self, namespace: &Sequence) -> u64 {
		match self.refresh {
			true => namespace.next().max(self.wanted),
			false => self.wanted,
		}
	}

	/// [`Self::wanted`], arming `waiter` for its next change.
	pub(super) fn poll_wanted(&mut self, namespace: &Sequence, waiter: &kio::Waiter) -> u64 {
		waiter.register(&mut self.waiters);
		self.wanted(namespace)
	}

	/// Raise the floor of [`Self::wanted`] to `group`, taking the wakes only when it moved.
	pub(super) fn want(&mut self, group: u64) -> kio::WaiterList {
		if group <= self.wanted {
			return Default::default();
		}
		self.wanted = group;
		self.waiters.take()
	}

	/// A join over the refresh ratio: ask for a group past every sequence written. Joins
	/// while one is pending coalesce into it.
	pub(super) fn refresh(&mut self) -> kio::WaiterList {
		if std::mem::replace(&mut self.refresh, true) {
			return Default::default();
		}
		self.waiters.take()
	}

	/// Sequence `sequence` was written at or past `next`, the namespace's next free sequence
	/// before it. A group subscribers receive answers a pending refresh; anything else, such
	/// as a datagram, moves the sequence it asks for.
	pub(super) fn wrote(&mut self, sequence: u64, next: u64, answers: bool) -> kio::WaiterList {
		if !self.refresh || sequence < next {
			return Default::default();
		}
		if answers {
			self.refresh = false;
			self.wanted = self.wanted.max(sequence);
		}
		self.waiters.take()
	}

	/// Mutate counts under one lock, taking wakes only when the aggregate changes.
	fn replace(&mut self, old: Option<&Subscription>, new: Option<&Subscription>) -> kio::WaiterList {
		let prev = self.scalars();
		// New ranges count before old ones leave, so a range both hold never reads as gone.
		let mut reshaped = false;
		let moved = old.map(|old| &old.ranges) != new.map(|new| &new.ranges);
		if let Some(new) = new {
			add(&mut self.priority, new.priority);
			add(&mut self.max_delay, new.max_delay);
			if moved {
				for range in &new.ranges {
					reshaped |= self.ranges.add(*range);
				}
			}
			self.descending += usize::from(new.order == Order::Descending);
			self.count += 1;
		}
		if let Some(old) = old {
			remove(&mut self.priority, old.priority);
			remove(&mut self.max_delay, old.max_delay);
			if moved {
				for range in &old.ranges {
					reshaped |= self.ranges.remove(*range);
				}
			}
			self.descending -= usize::from(old.order == Order::Descending);
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

/// Clones share one contribution, including controls and pending subscriptions.
#[derive(Clone)]
pub(in crate::model) struct Preferences(Arc<Preference>);

struct Preference {
	value: kio::Producer<Subscription>,
	subscriptions: Option<kio::Lock<Subscriptions>>,
	/// Bumped under the write whenever the ranges change.
	ranges_revision: AtomicU64,
}

impl Preferences {
	pub(super) fn register(state: kio::Ref<'_, TrackState>, value: Subscription) -> Self {
		let subscriptions = (!state.is_closed()).then(|| state.subscriptions.clone());
		drop(state);
		if let Some(subs) = &subscriptions {
			let mut wakes = subs.lock().replace(None, Some(&value));
			wakes.wake();
		}
		Self(Arc::new(Preference {
			value: kio::Producer::new(value),
			subscriptions,
			ranges_revision: AtomicU64::new(0),
		}))
	}

	pub(super) fn update(&self, next: Subscription) -> crate::Result<()> {
		let mut value = self.0.value.write().map_err(|_| crate::Error::Closed)?;
		if *value == next {
			return Ok(());
		}
		let mut wakes = self
			.0
			.subscriptions
			.as_ref()
			.map(|subs| subs.lock().replace(Some(&value), Some(&next)));
		if value.ranges != next.ranges {
			self.0.ranges_revision.fetch_add(1, atomic::Ordering::Relaxed);
		}
		*value = next;
		drop(value);
		if let Some(wakes) = &mut wakes {
			wakes.wake();
		}
		Ok(())
	}
}

impl Preferences {
	pub(in crate::model) fn same_channel(&self, other: &Self) -> bool {
		self.0.value.same_channel(&other.0.value)
	}
	pub(in crate::model) fn read(&self) -> kio::Ref<'_, Subscription> {
		self.0.value.read()
	}
	/// Counts changes to the ranges, so a reader can tell them apart without comparing every
	/// range. Read it while holding [`Self::read`] to get the count matching those ranges.
	pub(in crate::model) fn ranges_revision(&self) -> u64 {
		self.0.ranges_revision.load(atomic::Ordering::Relaxed)
	}
	pub(in crate::model) fn consume(&self) -> kio::Consumer<Subscription> {
		self.0.value.consume()
	}
	pub(in crate::model) fn poll_ref<F, R>(
		&self,
		waiter: &kio::Waiter,
		f: F,
	) -> Poll<Result<R, kio::Ref<'_, Subscription>>>
	where
		F: FnOnce(&kio::Ref<'_, Subscription>) -> Poll<R>,
	{
		self.0.value.poll_ref(waiter, f)
	}
}

impl Drop for Preference {
	fn drop(&mut self) {
		if let Some(subs) = &self.subscriptions {
			let mut wakes = subs.lock().replace(Some(&self.value.read()), None);
			wakes.wake();
		}
	}
}

#[cfg(test)]
impl From<kio::Producer<Subscription>> for Preferences {
	fn from(value: kio::Producer<Subscription>) -> Self {
		Self(Arc::new(Preference {
			value,
			subscriptions: None,
			ranges_revision: AtomicU64::new(0),
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::subscription::Combined;
	use super::super::{Anchor, Range};
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
				1 => Anchor::Relative(u64::MAX),
				_ => Anchor::Absolute(rng.random_range(0..8)),
			};
			let ranges: Vec<Range> = (0..rng.random_range(0..3))
				.map(|_| Range {
					start: anchor(&mut rng),
					end: rng.random_bool(0.75).then(|| anchor(&mut rng)),
				})
				.collect();
			let next = Subscription {
				priority: rng.random_range(0..8),
				max_delay: Duration::from_millis(rng.random_range(0..8)),
				ranges,
				order: if rng.random_bool(0.5) {
					Order::Ascending
				} else {
					Order::Descending
				},
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
		let strong = producer.subscribe(
			Subscription::default()
				.with_priority(10)
				.with_max_delay(Duration::from_secs(1)),
		);
		let count = Arc::new(Count(AtomicUsize::new(0)));
		let waiter = kio::Waiter::new(std::task::Waker::from(count.clone()));
		assert!(producer.poll_subscription_changed(&waiter).is_ready());
		assert!(producer.poll_subscription_changed(&waiter).is_pending());
		let mut weak = producer.subscribe(
			Subscription::default()
				.with_groups(100..200)
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

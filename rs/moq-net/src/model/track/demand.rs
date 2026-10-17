//! Incremental demand, separate from the track's cached content.
use std::{collections::BTreeMap, sync::Arc, task::Poll, time::Duration};

use super::super::subscription::union;
use super::{Order, Range, Subscription, TrackState, clamp_combined};

#[derive(Default)]
pub(super) struct Subscriptions {
	count: usize,
	priority: BTreeMap<u8, usize>,
	max_delay: BTreeMap<Duration, usize>,
	ranges: BTreeMap<Range, usize>,
	/// The union of `ranges`' keys, redone only when a range first appears or last leaves,
	/// so readers sharing a range never pay for it.
	union: Vec<Range>,
	descending: usize,
	waiters: kio::WaiterList,
}

impl Subscriptions {
	pub(super) fn combined(&self) -> Option<Subscription> {
		(self.count != 0).then(|| Subscription {
			priority: *self.priority.last_key_value().unwrap().0,
			max_delay: *self.max_delay.last_key_value().unwrap().0,
			ranges: self.union.clone(),
			order: if self.descending == 0 {
				Order::Ascending
			} else {
				Order::Descending
			},
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

	/// Mutate counts under one lock, taking wakes only when an extreme changes.
	fn replace(&mut self, old: Option<&Subscription>, new: Option<&Subscription>) -> kio::WaiterList {
		let prev = self.combined();
		let mut reshaped = false;
		if let Some(old) = old {
			remove(&mut self.priority, old.priority);
			remove(&mut self.max_delay, old.max_delay);
			for range in &old.ranges {
				reshaped |= remove(&mut self.ranges, *range);
			}
			self.descending -= usize::from(old.order == Order::Descending);
			self.count -= 1;
		}
		if let Some(new) = new {
			add(&mut self.priority, new.priority);
			add(&mut self.max_delay, new.max_delay);
			for range in &new.ranges {
				reshaped |= add(&mut self.ranges, *range);
			}
			self.descending += usize::from(new.order == Order::Descending);
			self.count += 1;
		}
		if reshaped {
			// The default range covers every other, so an audience holding it skips the walk.
			self.union = match self.ranges.contains_key(&Range::oldest()) {
				true => vec![Range::oldest()],
				false => union(self.ranges.keys().copied()),
			};
		}
		if self.combined() != prev {
			self.waiters.take()
		} else {
			Default::default()
		}
	}

	#[cfg(test)]
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
pub(crate) struct Preferences(Arc<Preference>);

struct Preference {
	value: kio::Producer<Subscription>,
	subscriptions: Option<kio::Lock<Subscriptions>>,
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
		*value = next;
		drop(value);
		if let Some(wakes) = &mut wakes {
			wakes.wake();
		}
		Ok(())
	}
}

impl Preferences {
	pub(crate) fn same_channel(&self, other: &Self) -> bool {
		self.0.value.same_channel(&other.0.value)
	}
	pub(crate) fn read(&self) -> kio::Ref<'_, Subscription> {
		self.0.value.read()
	}
	pub(crate) fn consume(&self) -> kio::Consumer<Subscription> {
		self.0.value.consume()
	}
	pub(crate) fn poll_ref<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R, kio::Ref<'_, Subscription>>>
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
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::subscription::Combined;
	use super::super::{Anchor, Position};
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
				_ => Anchor::Absolute(Position {
					group: rng.random_range(0..8),
					frame: rng.random_range(0..4),
				}),
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
			assert_eq!(producer.subscription(), expected.finish(), "operation {step}");
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

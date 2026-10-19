//! Abort notifications for the groups each capped subscriber parks, and for every
//! producer cut a wire publisher forwards as a drop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One registration per subscriber, with pending sequences bounded by its parked set,
/// or by the cuts between two polls.
#[derive(Default)]
pub(super) struct Aborts {
	// Visit only active cursors, even after a larger fanout has disconnected.
	readers: kio::Lock<BTreeMap<usize, kio::Lock<State>>>,
}

#[derive(Default)]
struct State {
	// Notified of every cut, whatever `watched` holds.
	cuts: bool,
	watched: BTreeSet<u64>,
	pending: BTreeSet<u64>,
	waiters: kio::WaiterList,
}

impl Aborts {
	pub(super) fn watch(self: &Arc<Self>) -> Watch {
		self.register(State::default())
	}

	pub(super) fn watch_cuts(self: &Arc<Self>) -> Watch {
		self.register(State {
			cuts: true,
			..State::default()
		})
	}

	fn register(self: &Arc<Self>, state: State) -> Watch {
		let state = kio::Lock::new(state);
		let mut readers = self.readers.lock();
		let key = readers
			.last_key_value()
			.map_or(0, |(key, _)| key.checked_add(1).expect("abort watcher IDs exhausted"));
		readers.insert(key, state.clone());
		Watch {
			registry: self.clone(),
			key,
			state,
		}
	}

	pub(super) fn notify(&self, sequence: u64, cut: bool) {
		let mut wakes = Vec::new();
		{
			let readers = self.readers.lock();
			for reader in readers.values() {
				let mut state = reader.lock();
				if ((cut && state.cuts) || state.watched.contains(&sequence)) && state.pending.insert(sequence) {
					wakes.push(state.waiters.take());
				}
			}
		}
		// An executor may poll inline, so release both locks before waking it.
		for mut wake in wakes {
			wake.wake();
		}
	}
}

pub(super) struct Watch {
	registry: Arc<Aborts>,
	key: usize,
	state: kio::Lock<State>,
}

impl Watch {
	pub(super) fn insert(&self, sequence: u64) {
		self.state.lock().watched.insert(sequence);
	}

	pub(super) fn remove(&self, sequence: u64) {
		let mut state = self.state.lock();
		state.watched.remove(&sequence);
		state.pending.remove(&sequence);
	}

	pub(super) fn register(&self, waiter: &kio::Waiter) {
		let mut state = self.state.lock();
		state.waiters.register(waiter);
		let pending = !state.pending.is_empty();
		drop(state);
		if pending {
			waiter.waker().wake_by_ref();
		}
	}

	pub(super) fn poll(&self, waiter: &kio::Waiter) -> BTreeSet<u64> {
		let mut state = self.state.lock();
		state.waiters.register(waiter);
		std::mem::take(&mut state.pending)
	}
}

impl Drop for Watch {
	fn drop(&mut self) {
		self.registry.readers.lock().remove(&self.key);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::task::{Wake, Waker};

	#[derive(Default)]
	struct Counter(AtomicUsize);
	impl Wake for Counter {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	#[test]
	fn registering_after_an_abort_still_wakes() {
		let registry = Arc::new(Aborts::default());
		let watch = registry.watch();
		watch.insert(7);
		registry.notify(7, false);
		let counter = Arc::new(Counter::default());
		let waiter = kio::Waiter::new(Waker::from(counter.clone()));
		watch.register(&waiter);
		assert_eq!(counter.0.load(Ordering::SeqCst), 1);
		assert_eq!(watch.poll(&waiter), BTreeSet::from([7]));
	}

	#[test]
	fn a_cuts_watch_sees_every_cut_and_nothing_else() {
		let registry = Arc::new(Aborts::default());
		let watch = registry.watch_cuts();
		registry.notify(1, false);
		registry.notify(2, true);
		assert_eq!(watch.poll(&kio::Waiter::noop()), BTreeSet::from([2]));
	}

	#[test]
	fn notifications_remain_bounded_by_watched_groups() {
		let registry = Arc::new(Aborts::default());
		let watch = registry.watch();
		watch.insert(3);
		for sequence in 0..1024 {
			registry.notify(sequence, false);
		}
		assert_eq!(watch.poll(&kio::Waiter::noop()), BTreeSet::from([3]));
		registry.notify(3, false);
		watch.remove(3);
		assert!(watch.poll(&kio::Waiter::noop()).is_empty());
		drop(watch);
		assert!(registry.readers.lock().is_empty());
	}
}

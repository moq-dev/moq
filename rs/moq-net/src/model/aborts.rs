//! Notifications of every producer cut a wire publisher forwards as a drop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One registration per wire publisher, with pending sequences bounded by the cuts
/// between two polls.
#[derive(Default)]
pub(super) struct Aborts {
	// Visit only active watchers, even after a larger fanout has disconnected.
	readers: kio::Lock<BTreeMap<usize, kio::Lock<State>>>,
}

#[derive(Default)]
struct State {
	pending: BTreeSet<u64>,
	waiters: kio::WaiterList,
}

impl Aborts {
	pub(super) fn watch_cuts(self: &Arc<Self>) -> Watch {
		let state = kio::Lock::new(State::default());
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

	/// Tell every watcher that the publisher cut group `sequence`.
	pub(super) fn notify(&self, sequence: u64) {
		let mut wakes = Vec::new();
		{
			let readers = self.readers.lock();
			for reader in readers.values() {
				let mut state = reader.lock();
				if state.pending.insert(sequence) {
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
	fn a_cut_wakes_the_watcher_once_per_poll() {
		let registry = Arc::new(Aborts::default());
		let watch = registry.watch_cuts();
		let counter = Arc::new(Counter::default());
		let waiter = kio::Waiter::new(Waker::from(counter.clone()));
		assert!(watch.poll(&waiter).is_empty());
		registry.notify(2);
		registry.notify(3);
		assert_eq!(counter.0.load(Ordering::SeqCst), 1);
		assert_eq!(watch.poll(&waiter), BTreeSet::from([2, 3]));
		drop(watch);
		assert!(registry.readers.lock().is_empty());
	}
}

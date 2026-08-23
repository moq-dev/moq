//! Userspace timers: a heap the worker sweeps, never a timeout SQE.
//!
//! Every armed deadline lives in one ordered map. The worker fires the due
//! prefix each turn and parks with the earliest remaining instant as the
//! absolute `io_uring_enter` timeout, so timers cost zero submissions and
//! re-arming is a pure memory operation (what `moq_net::runtime::Timer`
//! demands).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::task::Poll;
use std::time::Instant;

/// The ordered set of armed timers, keyed by (deadline, tiebreak).
#[derive(Default)]
pub(crate) struct Heap {
	queue: BTreeMap<(Instant, u64), Rc<Slot>>,
	seq: u64,
}

/// One timer's state, shared between its [`Timer`] handle and the heap.
struct Slot {
	/// The heap key while armed and unfired.
	key: Cell<Option<(Instant, u64)>>,
	/// Fired and not yet re-armed; `poll` stays `Ready` (fused).
	elapsed: Cell<bool>,
	waiters: RefCell<kio::WaiterList>,
}

impl Heap {
	/// Fire everything due at `now`. Returns whether anything fired.
	pub fn fire(&mut self, now: Instant) -> bool {
		let mut fired = false;
		while let Some(entry) = self.queue.first_entry() {
			if entry.key().0 > now {
				break;
			}
			let slot = entry.remove();
			slot.key.set(None);
			slot.elapsed.set(true);
			slot.waiters.borrow_mut().wake();
			fired = true;
		}
		fired
	}

	/// The earliest armed deadline, if any: the park timeout.
	pub fn next(&self) -> Option<Instant> {
		self.queue.first_key_value().map(|(key, _)| key.0)
	}

	fn insert(&mut self, at: Instant, slot: Rc<Slot>) -> (Instant, u64) {
		self.seq += 1;
		let key = (at, self.seq);
		self.queue.insert(key, slot);
		key
	}

	fn remove(&mut self, key: (Instant, u64)) {
		self.queue.remove(&key);
	}
}

/// A single re-armable timer slot in a worker's heap.
///
/// Implements [`moq_net::runtime::Timer`]: arm with
/// [`set`](moq_net::runtime::Timer::set), poll from any `poll_*` function on
/// this worker's thread. Minted by [`moq_net::Timers::timer`] on a
/// [`crate::Handle`].
pub struct Timer {
	heap: Rc<RefCell<Heap>>,
	slot: Rc<Slot>,
}

impl Timer {
	pub(crate) fn new(heap: Rc<RefCell<Heap>>) -> Self {
		Self {
			heap,
			slot: Rc::new(Slot {
				key: Cell::new(None),
				elapsed: Cell::new(false),
				waiters: RefCell::new(kio::WaiterList::new()),
			}),
		}
	}
}

impl moq_net::runtime::Timer for Timer {
	fn set(&mut self, at: Option<Instant>) {
		let mut heap = self.heap.borrow_mut();
		if let Some(key) = self.slot.key.take() {
			heap.remove(key);
		}
		self.slot.elapsed.set(false);
		if let Some(at) = at {
			// An instant already in the past still goes through the heap: the
			// worker's next sweep fires it, and a poll before then observes
			// `at <= now` directly.
			let key = heap.insert(at, self.slot.clone());
			self.slot.key.set(Some(key));
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.slot.elapsed.get() {
			return Poll::Ready(());
		}
		let Some((at, _)) = self.slot.key.get() else {
			return Poll::Pending;
		};
		if at <= Instant::now() {
			// Fire eagerly rather than waiting for the sweep, and drop the
			// heap entry so the sweep doesn't wake anyone spuriously.
			self.heap.borrow_mut().remove(self.slot.key.take().expect("armed"));
			self.slot.elapsed.set(true);
			return Poll::Ready(());
		}
		waiter.register(&mut self.slot.waiters.borrow_mut());
		Poll::Pending
	}
}

impl Drop for Timer {
	fn drop(&mut self) {
		if let Some(key) = self.slot.key.take() {
			self.heap.borrow_mut().remove(key);
		}
	}
}

impl std::fmt::Debug for Timer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Timer")
			.field("at", &self.slot.key.get().map(|key| key.0))
			.field("elapsed", &self.slot.elapsed.get())
			.finish()
	}
}

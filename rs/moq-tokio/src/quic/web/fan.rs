use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};

use kio::{Waiter, WaiterList};

/// A shared [`WaiterList`] that a [`Waker`] can wake.
///
/// The accept futures below take one `Waker` and keep it. Handing them a caller's waker
/// would make the latest caller own the wakeup for everyone, so they are polled with
/// [`Fan::waker`] instead, which outlives every caller and wakes the whole list.
#[derive(Clone, Default)]
pub(crate) struct Fan(Arc<Inner>);

#[derive(Default)]
struct Inner(Mutex<State>);

#[derive(Default)]
struct State {
	list: WaiterList,
	/// Outstanding [`Hold`]s; a wake while any is alive is owed instead of delivered.
	held: usize,
	owed: bool,
}

impl Fan {
	/// Park `waiter` until the next wake.
	pub(crate) fn register(&self, waiter: &Waiter) {
		waiter.register(&mut self.0.0.lock().unwrap().list);
	}

	/// A waker that wakes every parked waiter.
	pub(crate) fn waker(&self) -> Waker {
		Waker::from(self.0.clone())
	}

	/// Wake every parked waiter, or owe the wake until the last [`Hold`] drops.
	pub(crate) fn wake(&self) {
		self.0.notify();
	}

	/// Defer wakes until the guard drops, so none is delivered under the caller's lock.
	pub(crate) fn hold(&self) -> Hold {
		self.0.0.lock().unwrap().held += 1;
		Hold(self.0.clone())
	}
}

impl Inner {
	fn notify(&self) {
		let mut list = {
			let mut state = self.0.lock().unwrap();
			if state.held > 0 {
				state.owed = true;
				return;
			}
			state.list.take()
		};
		// Outside the lock: a waker may resume its task inline.
		list.wake();
	}
}

impl Wake for Inner {
	fn wake(self: Arc<Self>) {
		self.notify();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.notify();
	}
}

/// Defers a [`Fan`]'s wakes while alive, delivering any owed one on drop.
pub(crate) struct Hold(Arc<Inner>);

impl Drop for Hold {
	fn drop(&mut self) {
		let owed = {
			let mut state = self.0.0.lock().unwrap();
			state.held -= 1;
			state.held == 0 && std::mem::take(&mut state.owed)
		};
		if owed {
			self.0.notify();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hold_defers_the_wake_until_dropped() {
		let fan = Fan::default();
		let (waker, count) = counting();
		let waiter = Waiter::new(waker);
		fan.register(&waiter);

		let hold = fan.hold();
		fan.waker().wake_by_ref();
		assert_eq!(count.load(), 0, "held back");

		drop(hold);
		assert_eq!(count.load(), 1, "delivered on drop");
	}

	#[test]
	fn waker_wakes_every_waiter() {
		let fan = Fan::default();
		let (a, count_a) = counting();
		let (b, count_b) = counting();
		// The list holds waiters weakly, so they have to outlive the wake.
		let (a, b) = (Waiter::new(a), Waiter::new(b));
		fan.register(&a);
		fan.register(&b);

		fan.wake();
		assert_eq!((count_a.load(), count_b.load()), (1, 1));
	}

	struct Count(std::sync::atomic::AtomicUsize);

	impl Count {
		fn load(&self) -> usize {
			self.0.load(std::sync::atomic::Ordering::SeqCst)
		}
	}

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		}
	}

	fn counting() -> (Waker, Arc<Count>) {
		let count = Arc::new(Count(Default::default()));
		(Waker::from(count.clone()), count)
	}
}

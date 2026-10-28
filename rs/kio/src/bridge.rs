use std::{
	fmt,
	ops::{Deref, DerefMut},
	// std, not `crate::sync`: `Wake` takes std's Arc.
	sync::{Arc, PoisonError},
	task::{Context, Wake, Waker},
};

use crate::{
	Waiter, WaiterList,
	sync::{Mutex, MutexGuard},
};

/// A [`Context`]-based handle that any number of waiters poll at once.
///
/// The inverse of [`Park`](crate::Park): it lets a `poll_*` method taking `&self` and a
/// [`Waiter`] drive a foreign handle whose `&mut self` polls keep one [`Waker`] per
/// operation, which would only ever wake its latest caller. The handle sits behind a lock
/// and is always polled with this bridge's own waker, which wakes every waiter registered
/// since it last fired. The list holds waiters weakly, so a caller that gives up leaves
/// nothing behind.
///
/// A handle keeps one waker per operation it can wait on, so give each operation that may
/// wait at the same time as another its own bridge, around its own copy of the handle.
pub struct Bridge<T> {
	inner: Mutex<T>,
	waiters: Arc<Waiters>,
	waker: Waker,
}

/// The waiters to wake once the handle makes progress.
#[derive(Default)]
struct Waiters(Mutex<State>);

#[derive(Default)]
struct State {
	list: WaiterList,
	/// The handle is locked, so a wake waits for its release: a waiter woken inline could
	/// poll this bridge again and block on the lock its own wake is holding.
	held: bool,
	/// A wake arrived while the handle was locked.
	deferred: bool,
}

impl Waiters {
	fn lock(&self) -> MutexGuard<'_, State> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Mark the handle locked, deferring wakes until [`Self::release`].
	fn hold(&self) {
		self.lock().held = true;
	}

	/// Mark the handle unlocked, returning the waiters a deferred wake owes.
	fn release(&self) -> Option<WaiterList> {
		let mut state = self.lock();
		state.held = false;
		std::mem::take(&mut state.deferred).then(|| state.list.take())
	}
}

impl Wake for Waiters {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		let mut state = self.lock();
		if state.held {
			state.deferred = true;
			return;
		}
		// Woken outside the lock, so a waker that polls right away cannot deadlock on it.
		let mut woken = state.list.take();
		drop(state);
		woken.wake();
	}
}

impl<T> Bridge<T> {
	/// Bridge `inner`.
	pub fn new(inner: T) -> Self {
		let waiters = Arc::<Waiters>::default();
		Self {
			inner: Mutex::new(inner),
			waker: Waker::from(waiters.clone()),
			waiters,
		}
	}

	/// Run `poll` against the handle with a [`Context`] that wakes `waiter`, along with
	/// every other waiter parked since the handle last made progress.
	///
	/// `waiter` is registered first, so a wake landing during `poll` is never lost; it
	/// stays registered until that wake, or until its owner drops it. A wake landing while
	/// the handle is locked reaches the waiters once it is released, so one may poll this
	/// bridge again from inside its waker.
	pub fn poll<R>(&self, waiter: &Waiter, poll: impl FnOnce(&mut T, &mut Context<'_>) -> R) -> R {
		waiter.register(&mut self.waiters.lock().list);
		let mut cx = Context::from_waker(&self.waker);
		poll(&mut self.lock(), &mut cx)
	}

	/// Lock the handle, for an operation that never waits. Wakes it causes reach the
	/// waiters once the lock is released.
	pub fn lock(&self) -> impl DerefMut<Target = T> + '_ {
		let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
		self.waiters.hold();
		Guard {
			inner: Some(inner),
			waiters: &self.waiters,
		}
	}

	/// The handle, through an exclusive borrow that needs no lock.
	pub fn get_mut(&mut self) -> &mut T {
		self.inner.get_mut().unwrap_or_else(PoisonError::into_inner)
	}

	/// The handle, unwrapped.
	pub fn into_inner(self) -> T {
		self.inner.into_inner().unwrap_or_else(PoisonError::into_inner)
	}
}

/// The lock held on a [`Bridge`]'s handle.
struct Guard<'a, T> {
	/// Taken only on drop, to unlock before the deferred wakes run.
	inner: Option<MutexGuard<'a, T>>,
	waiters: &'a Waiters,
}

impl<T> Deref for Guard<'_, T> {
	type Target = T;

	fn deref(&self) -> &T {
		self.inner.as_ref().expect("held until drop")
	}
}

impl<T> DerefMut for Guard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		self.inner.as_mut().expect("held until drop")
	}
}

impl<T> Drop for Guard<'_, T> {
	fn drop(&mut self) {
		// Marked released while still locked, so no other locker's hold is cleared.
		let woken = self.waiters.release();
		drop(self.inner.take());
		if let Some(mut woken) = woken {
			woken.wake();
		}
	}
}

impl<T: Default> Default for Bridge<T> {
	fn default() -> Self {
		Self::new(T::default())
	}
}

impl<T> fmt::Debug for Bridge<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Bridge").finish_non_exhaustive()
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::{
		sync::atomic::{AtomicUsize, Ordering},
		task::Poll,
	};

	use super::*;

	/// Counts its wakes.
	#[derive(Default)]
	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// A quinn-style handle: one stored waker, replaced by every poll.
	#[derive(Default)]
	struct Handle {
		waker: Option<Waker>,
		ready: bool,
	}

	impl Handle {
		fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<()> {
			if self.ready {
				return Poll::Ready(());
			}
			self.waker = Some(cx.waker().clone());
			Poll::Pending
		}

		fn fire(&mut self) {
			self.ready = true;
			if let Some(waker) = self.waker.take() {
				waker.wake();
			}
		}
	}

	#[test]
	fn every_parked_waiter_wakes() {
		let bridge = Bridge::new(Handle::default());
		let counts: Vec<_> = (0..3).map(|_| Arc::new(Count::default())).collect();
		let waiters: Vec<_> = counts
			.iter()
			.map(|count| Waiter::new(Waker::from(count.clone())))
			.collect();
		for waiter in &waiters {
			assert!(bridge.poll(waiter, Handle::poll_ready).is_pending());
		}

		bridge.lock().fire();
		for count in &counts {
			assert_eq!(count.0.load(Ordering::Relaxed), 1, "a parked waiter missed the wake");
		}
		assert!(bridge.poll(&waiters[0], Handle::poll_ready).is_ready());
	}

	#[test]
	fn an_abandoned_waiter_leaves_nothing() {
		let bridge = Bridge::new(Handle::default());
		for _ in 0..1000 {
			let waiter = Waiter::new(Waker::noop().clone());
			assert!(bridge.poll(&waiter, Handle::poll_ready).is_pending());
		}
		assert!(bridge.waiters.lock().list.is_empty(), "a dropped waiter is still held");
	}

	/// Polls the bridge again from inside its own wake, as an executor that runs a woken
	/// task inline would.
	struct Reenter {
		bridge: Arc<Bridge<Handle>>,
		polled: AtomicUsize,
	}

	impl Wake for Reenter {
		fn wake(self: Arc<Self>) {
			self.wake_by_ref();
		}

		fn wake_by_ref(self: &Arc<Self>) {
			let _ = self.bridge.poll(&Waiter::noop(), Handle::poll_ready);
			self.polled.fetch_add(1, Ordering::Relaxed);
		}
	}

	#[test]
	fn a_waker_may_poll_the_bridge_inline() {
		let bridge = Arc::new(Bridge::new(Handle::default()));
		let reenter = Arc::new(Reenter {
			bridge: bridge.clone(),
			polled: AtomicUsize::new(0),
		});
		let waiter = Waiter::new(Waker::from(reenter.clone()));

		// Woken by the poll itself, while it holds the handle.
		let pending = bridge.poll(&waiter, |handle, cx| {
			cx.waker().wake_by_ref();
			handle.poll_ready(cx)
		});
		assert!(pending.is_pending());
		assert_eq!(reenter.polled.load(Ordering::Relaxed), 1, "the deferred wake was lost");

		// Woken under `lock`.
		assert!(bridge.poll(&waiter, Handle::poll_ready).is_pending());
		bridge.lock().fire();
		assert_eq!(reenter.polled.load(Ordering::Relaxed), 2, "the deferred wake was lost");
	}
}

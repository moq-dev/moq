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
struct Waiters(Mutex<WaiterList>);

impl Waiters {
	fn lock(&self) -> MutexGuard<'_, WaiterList> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

impl Wake for Waiters {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// Woken outside the lock, so a waker that polls right away cannot deadlock on it.
		let mut woken = self.lock().take();
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
	/// stays registered until that wake, or until its owner drops it.
	pub fn poll<R>(&self, waiter: &Waiter, poll: impl FnOnce(&mut T, &mut Context<'_>) -> R) -> R {
		waiter.register(&mut self.waiters.lock());
		let mut cx = Context::from_waker(&self.waker);
		poll(&mut self.lock(), &mut cx)
	}

	/// Lock the handle, for an operation that never waits.
	pub fn lock(&self) -> impl DerefMut<Target = T> + '_ {
		Guard(self.inner.lock().unwrap_or_else(PoisonError::into_inner))
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
struct Guard<'a, T>(MutexGuard<'a, T>);

impl<T> Deref for Guard<'_, T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.0
	}
}

impl<T> DerefMut for Guard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		&mut self.0
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
		assert!(bridge.waiters.lock().is_empty(), "a dropped waiter is still held");
	}
}

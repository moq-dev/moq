use std::task::Poll;

use crate::{
	State,
	lock::Lock,
	producer::{Mut, Ref},
	waiter::*,
};

/// A cloneable, lock-guarded value with waker notification.
///
/// Unlike the [`Producer`](crate::Producer) / [`Consumer`](crate::Consumer) watch channel,
/// every handle is equal: any clone can mutate ([`lock`](Self::lock)), read
/// ([`read`](Self::read)), or park until a predicate holds ([`poll`](Self::poll)).
/// Mutating through the guard wakes the parked polls.
///
/// This is the primitive for state that two sides legitimately mutate, e.g. a request
/// queue where consumers enqueue (coalescing against what's already there) and a handler
/// drains, all under one lock so the dedup is a plain lookup rather than a race. It has
/// no liveness of its own: encode "the other side is gone" as plain state (a counter or
/// flag inside `T`, maintained by the owning handles' `Clone`/`Drop` under the same lock).
#[derive(Debug)]
pub struct Shared<T> {
	state: Lock<State<T>>,
}

impl<T: Default> Default for Shared<T> {
	fn default() -> Self {
		Self::new(T::default())
	}
}

impl<T> Shared<T> {
	/// Create a new shared value.
	pub fn new(value: T) -> Self {
		Self {
			state: Lock::new(State::new(value)),
		}
	}

	/// Lock the value for reading and writing.
	///
	/// Never blocks on anything but the mutex itself. Mutating through the returned
	/// [`Mut`] wakes every parked [`poll`](Self::poll) on drop; a guard that was only
	/// read from wakes nobody.
	pub fn lock(&self) -> Mut<'_, T> {
		Mut::new(self.state.lock())
	}

	/// Lock the value for reading only, never waking anyone.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Poll a read-only predicate; once it holds, hand back a [`Mut`] with the lock
	/// still held, so the caller can inspect and mutate atomically.
	///
	/// Mirrors [`Producer::poll`](crate::Producer::poll): the predicate only sees a
	/// [`Ref`], so it can't flag the state modified and spuriously wake this poll's own
	/// waiter. Registers `waiter` while pending; any mutation through a [`lock`](Self::lock)
	/// guard re-polls it.
	pub fn poll<F>(&self, waiter: &Waiter, mut f: F) -> Poll<Mut<'_, T>>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<()>,
	{
		let mut guard = Ref {
			state: self.state.lock(),
		};
		match f(&guard) {
			// Upgrade the Ref to a Mut, keeping the same lock guard.
			Poll::Ready(()) => Poll::Ready(Mut::new(guard.state)),
			Poll::Pending => {
				waiter.register(&mut guard.state.waiters_value);
				Poll::Pending
			}
		}
	}

	/// Wait until the read-only predicate holds, then acquire write access.
	///
	/// The async sibling of [`poll`](Self::poll). It's infallible: a `Shared` has no
	/// liveness of its own, so there's no closure to report. A predicate that never
	/// holds simply waits forever.
	pub async fn wait<F>(&self, mut f: F) -> Mut<'_, T>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<()> + Unpin,
	{
		crate::wait(move |waiter| self.poll(waiter, &mut f)).await
	}

	/// Returns `true` if both handles share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Clone for Shared<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
		}
	}
}

#[cfg(test)]
mod test {
	use std::{
		future::Future,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Context, Wake, Waker},
	};

	use super::*;

	/// A waker that counts how many times it was woken (mirrors `tests.rs`).
	struct CountWaker(AtomicUsize);
	impl CountWaker {
		fn count(&self) -> usize {
			self.0.load(Ordering::SeqCst)
		}
	}
	impl Wake for CountWaker {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
		fn wake_by_ref(self: &Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}
	fn counting() -> (Arc<CountWaker>, Waker) {
		let waker = Arc::new(CountWaker(AtomicUsize::new(0)));
		let w = Waker::from(waker.clone());
		(waker, w)
	}

	/// Ready once the queue has something to drain.
	fn nonempty(queue: &Ref<'_, Vec<u32>>) -> Poll<()> {
		if queue.is_empty() {
			Poll::Pending
		} else {
			Poll::Ready(())
		}
	}

	#[test]
	fn enqueue_then_drain() {
		let shared = Shared::<Vec<u32>>::default();
		let drain = shared.clone();
		let waiter = Waiter::noop();

		// Nothing queued yet.
		assert!(drain.poll(&waiter, nonempty).is_pending());

		// One side enqueues.
		shared.lock().push(1);

		// The other side drains it, inspecting and mutating under one lock.
		let Poll::Ready(mut guard) = drain.poll(&waiter, nonempty) else {
			panic!("expected a drainable guard");
		};
		assert_eq!(guard.pop(), Some(1));
	}

	#[test]
	fn mutation_wakes_parked_poll() {
		let shared = Shared::<Vec<u32>>::default();
		let drain = shared.clone();

		let (waker, w) = counting();
		let mut cx = Context::from_waker(&w);

		let mut fut = Box::pin(crate::wait(|waiter| drain.poll(waiter, nonempty)));
		assert!(fut.as_mut().poll(&mut cx).is_pending(), "pending until enqueue");

		shared.lock().push(7);
		assert!(waker.count() >= 1, "enqueue should wake the parked poll");

		let Poll::Ready(mut guard) = fut.as_mut().poll(&mut cx) else {
			panic!("expected a drainable guard after enqueue");
		};
		assert_eq!(guard.pop(), Some(7));
	}

	#[test]
	fn read_does_not_wake() {
		let shared = Shared::<Vec<u32>>::default();
		let drain = shared.clone();

		let (waker, w) = counting();
		let mut cx = Context::from_waker(&w);

		let mut fut = Box::pin(crate::wait(|waiter| drain.poll(waiter, nonempty)));
		assert!(fut.as_mut().poll(&mut cx).is_pending());

		// Read-only access (and an unmutated lock) must not wake the parked poll.
		assert!(shared.read().is_empty());
		let guard = shared.lock();
		assert!(guard.is_empty());
		drop(guard);
		assert_eq!(waker.count(), 0, "reads spuriously woke a parked poll");
	}

	/// The async sibling of `poll`: parks until another handle enqueues, then hands
	/// back the drainable guard.
	#[tokio::test]
	async fn wait_parks_until_enqueued() {
		let shared = Shared::<Vec<u32>>::default();
		let drain = shared.clone();

		let task = tokio::spawn(async move {
			let mut guard = drain.wait(nonempty).await;
			guard.pop()
		});

		// Let the task park on an empty queue before anything is enqueued.
		tokio::task::yield_now().await;
		shared.lock().push(3);

		assert_eq!(task.await.unwrap(), Some(3));
	}

	#[test]
	fn same_channel_tracks_identity() {
		let shared = Shared::<Vec<u32>>::default();
		let clone = shared.clone();
		let other = Shared::<Vec<u32>>::default();

		assert!(shared.same_channel(&clone));
		assert!(!shared.same_channel(&other));
	}
}

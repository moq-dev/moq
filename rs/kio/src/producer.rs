use std::{
	ops::{Deref, DerefMut},
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{Counts, State, consumer::Consumer, lock::*, waiter::*, weak::Weak};

/// The producing side of a shared state channel.
///
/// Producers hold mutable access to the shared value. When the state is modified
/// through [`Mut`], all registered consumers are automatically notified.
/// Cloning a producer increments the producer reference count. When the last
/// producer is dropped, the channel is closed.
#[derive(Debug)]
pub struct Producer<T> {
	pub(crate) state: Lock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T: Default> Default for Producer<T> {
	fn default() -> Self {
		Self {
			state: Lock::new(State::default()),
			counts: Arc::new(Counts::default()),
		}
	}
}

impl<T> Producer<T> {
	/// Create a new producer with the given initial value.
	pub fn new(value: T) -> Self {
		Self {
			state: Lock::new(State::new(value)),
			counts: Arc::new(Counts::default()),
		}
	}

	/// Create a new [`Consumer`] that shares this producer's state.
	pub fn consume(&self) -> Consumer<T> {
		let prev = self.counts.consumers.fetch_add(1, Ordering::AcqRel);

		// Wake waiters (e.g. `used()`) when the first consumer appears.
		if prev == 0 {
			let mut waiters = self.state.lock().waiters.take();
			waiters.wake();
		}

		Consumer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	/// Close the channel, notifying all consumers.
	pub fn close(&self) -> Result<(), Ref<'_, T>> {
		self.write()?.close();
		Ok(())
	}

	/// Acquire mutable access to the shared state.
	///
	/// Returns `Ok(Mut)` if the channel is open, or `Err(Ref)` with
	/// read-only access if closed. Only locks once.
	pub fn write(&self) -> Result<Mut<'_, T>, Ref<'_, T>> {
		let state = self.state.lock();
		if state.closed {
			Err(Ref { state })
		} else {
			Ok(Mut::new(state))
		}
	}

	/// Poll a read-only predicate; on [`Poll::Ready`] hand back a [`Mut`] with the
	/// lock still held, so the caller can inspect and mutate atomically.
	///
	/// Unlike [`Consumer::poll`], the predicate returns `Poll<()>` (it just gates
	/// readiness) and a satisfied poll yields write access via [`Mut`]. The
	/// predicate only sees a [`Ref`], so it can't accidentally flag the state
	/// modified (e.g. via a `&mut`-taking method like `Vec::pop`). That sidesteps
	/// the footgun where a no-op mutation during a *pending* poll would wake this
	/// producer's own waiter and spin into an infinite loop. Decide readiness in
	/// the predicate, then mutate through the returned `Mut`. Registers `waiter`
	/// while pending.
	///
	/// Returns `Poll::Ready(Err(`[`Ref`]`))` if the channel is closed.
	pub fn poll<F>(&self, waiter: &Waiter, mut f: F) -> Poll<Result<Mut<'_, T>, Ref<'_, T>>>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<()>,
	{
		let state = self.state.lock();
		if state.closed {
			return Poll::Ready(Err(Ref { state }));
		}

		let mut guard = Ref { state };
		match f(&guard) {
			// Upgrade the Ref to a Mut, keeping the same lock guard.
			Poll::Ready(()) => Poll::Ready(Ok(Mut::new(guard.state))),
			Poll::Pending => {
				waiter.register(&mut guard.state.waiters);
				Poll::Pending
			}
		}
	}

	/// Wait until the read-only predicate holds, then acquire write access.
	///
	/// The async sibling of [`poll`](Self::poll): returns `Ok(Mut)` once `f`
	/// returns [`Poll::Ready`], or `Err(Ref)` if the channel closes first.
	pub async fn wait<F>(&self, mut f: F) -> Result<Mut<'_, T>, Ref<'_, T>>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<()> + Unpin,
	{
		crate::wait(move |waiter| self.poll(waiter, &mut f)).await
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
	}

	fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(());
		}

		waiter.register(&mut state.waiters);
		Poll::Pending
	}

	/// Wait until all consumers have been dropped.
	///
	/// Returns `Ok(())` when no consumers remain, or `Err(Ref)` if the channel closes first.
	pub async fn unused(&self) -> Result<(), Ref<'_, T>> {
		match crate::wait(move |waiter| self.poll_unused(waiter)).await {
			Some(()) => Ok(()),
			None => Err(self.read()),
		}
	}

	fn poll_unused(&self, waiter: &Waiter) -> Poll<Option<()>> {
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
		}

		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Some(()));
		}

		waiter.register(&mut state.waiters);

		// Re-check after registration to avoid TOCTOU race where the last
		// consumer drops between the initial check and waiter registration.
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}

	/// Wait until at least one consumer exists.
	///
	/// Returns `Ok(())` when a consumer is created, or `Err(Ref)` if the channel closes first.
	pub async fn used(&self) -> Result<(), Ref<'_, T>> {
		match crate::wait(move |waiter| self.poll_used(waiter)).await {
			Some(()) => Ok(()),
			None => Err(self.read()),
		}
	}

	fn poll_used(&self, waiter: &Waiter) -> Poll<Option<()>> {
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
		}

		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		waiter.register(&mut state.waiters);

		// Re-check after registration to avoid TOCTOU race where a consumer
		// is created between the initial check and waiter registration.
		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Returns `true` if both producers share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	/// Returns `true` if this is the only remaining producer.
	///
	/// Inherently racy if other handles may clone this producer or upgrade a
	/// [`Weak`] / [`Consumer`] concurrently. Intended for a producer's own
	/// `Drop`, where this handle has not yet been counted out, to gate
	/// last-producer cleanup.
	pub fn is_last(&self) -> bool {
		self.counts.producers.load(Ordering::Acquire) == 1
	}

	/// Create a [`Weak`] reference that doesn't affect the producer/consumer ref counts.
	pub fn weak(&self) -> Weak<T> {
		Weak {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		self.counts.producers.fetch_add(1, Ordering::Relaxed);

		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

impl<T> Drop for Producer<T> {
	fn drop(&mut self) {
		// Atomically decrement and check if we were the last producer
		let prev = self.counts.producers.fetch_sub(1, Ordering::AcqRel);
		if prev > 1 {
			return;
		}

		// We were the last producer, need to close
		let mut waiters = {
			let mut state = self.state.lock();
			if state.closed {
				return;
			}

			state.closed = true;
			state.waiters.take()
		};

		waiters.wake();
	}
}

/// A mutable guard over the shared state.
///
/// Derefs to `T` for direct access. Automatically notifies all waiting consumers
/// when dropped if the state was accessed mutably.
#[derive(Debug)]
pub struct Mut<'a, T> {
	// Its an option so we can drop it before notifying consumers.
	pub(crate) state: Option<LockGuard<'a, State<T>>>,
	pub(crate) modified: bool,
}

impl<'a, T> Mut<'a, T> {
	pub(crate) fn new(state: LockGuard<'a, State<T>>) -> Self {
		Self {
			state: Some(state),
			modified: false,
		}
	}

	/// NOTE: This takes self so it's impossible to be in a closed state.
	pub fn close(mut self) {
		let state = self.state.as_mut().unwrap();
		// We don't need to check for state.closed because we checked when making Mut
		state.closed = true;
		self.modified = true;
	}
}

impl<T> Deref for Mut<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.state.as_ref().unwrap().value
	}
}

impl<T> DerefMut for Mut<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		// If we use the &mut then notify on Drop.
		self.modified = true;
		&mut self.state.as_mut().unwrap().value
	}
}

impl<T> Drop for Mut<'_, T> {
	fn drop(&mut self) {
		let mut state = self.state.take().unwrap();

		if !self.modified {
			return;
		}

		// Drain wakers while holding lock, then wake after releasing
		let mut waiters = state.waiters.take();
		drop(state); // Release Mutex BEFORE waking

		waiters.wake();
	}
}

/// A read-only guard over the shared state.
///
/// Derefs to `T` for direct access. Does not notify consumers when dropped.
pub struct Ref<'a, T> {
	pub(crate) state: LockGuard<'a, State<T>>,
}

impl<T> Ref<'_, T> {
	/// Returns `true` if the channel has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.closed
	}
}

impl<T> Deref for Ref<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.state.value
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn is_last_tracks_producer_count() {
		let producer = Producer::new(0u8);
		assert!(producer.is_last());

		let clone = producer.clone();
		assert!(!producer.is_last());
		assert!(!clone.is_last());

		drop(clone);
		assert!(producer.is_last());

		// Consumers and weak handles don't count as producers.
		let _consumer = producer.consume();
		let _weak = producer.weak();
		assert!(producer.is_last());
	}

	#[test]
	fn poll_gates_on_predicate_then_writes() {
		let producer = Producer::<Vec<u32>>::default();
		let waiter = Waiter::noop();

		let predicate = |state: &Ref<'_, Vec<u32>>| {
			if state.is_empty() {
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		};

		// Empty queue: the read-only predicate is pending, so no Mut is handed out
		// (and crucially nothing flags the state modified to wake our own waiter).
		assert!(matches!(producer.poll(&waiter, predicate), Poll::Pending));

		let Ok(mut write) = producer.write() else {
			panic!("channel should be open");
		};
		write.push(1);
		drop(write);

		// Now satisfied: poll upgrades to a Mut with the lock still held.
		let Poll::Ready(Ok(mut state)) = producer.poll(&waiter, predicate) else {
			panic!("expected a writable guard");
		};
		assert_eq!(state.pop(), Some(1));
		drop(state);

		// Closed channel reports back through Err.
		assert!(producer.close().is_ok());
		assert!(matches!(producer.poll(&waiter, predicate), Poll::Ready(Err(_))));
	}
}

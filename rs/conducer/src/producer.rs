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

	/// Poll-based mutable access with waker registration.
	///
	/// Calls `f` with a [`Mut`] guard. If `f` returns [`Poll::Pending`],
	/// registers the [`Waiter`] for notification when the state next changes.
	/// Returns `Poll::Ready(Err(`[`Ref`]`))` if the channel is closed.
	pub fn poll<F, R>(&self, waiter: &Waiter, mut f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnMut(&mut Mut<'_, T>) -> Poll<R>,
	{
		let mut state = self.write()?;

		if let Poll::Ready(res) = f(&mut state) {
			return Poll::Ready(Ok(res));
		}

		let inner = state.state.as_mut().unwrap();

		// Take existing waiters if f modified the state, so we can notify consumers.
		let waiters = if state.modified {
			Some(inner.waiters.take())
		} else {
			None
		};

		// Register ourselves for future notifications.
		waiter.register(&mut inner.waiters);

		// Prevent Drop from re-waking the waiter we just registered.
		state.modified = false;

		// Release the lock before waking consumers.
		drop(state);

		if let Some(mut waiters) = waiters {
			waiters.wake();
		}

		Poll::Pending
	}

	/// Wait for the closure to return [`Poll::Ready`], re-polling on each state change.
	///
	/// Returns `Ok(R)` when the closure returns [`Poll::Ready`], or `Err(Ref)` with
	/// read-only access to the final state if the channel closes first.
	pub async fn wait<F, R>(&self, mut f: F) -> Result<R, Ref<'_, T>>
	where
		F: FnMut(&mut Mut<'_, T>) -> Poll<R> + Unpin,
		R: Unpin,
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

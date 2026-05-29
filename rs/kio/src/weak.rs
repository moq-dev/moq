use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{
	Counts, State,
	consumer::Consumer,
	lock::*,
	producer::{Mut, Producer, Ref},
	waiter::*,
};

/// A weak reference to a Producer/Consumer state.
///
/// Does not affect ref counts, so it won't prevent auto-close when all Producers are dropped.
/// Can be upgraded to a full Producer or Consumer.
#[derive(Debug)]
pub struct Weak<T> {
	pub(crate) state: Lock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T> Weak<T> {
	/// Upgrade to a [`Producer`], returning `None` if the channel is already closed.
	pub fn produce(&self) -> Option<Producer<T>> {
		// Increment first to prevent the last Producer::drop from
		// closing the state between our check and the return.
		self.counts.producers.fetch_add(1, Ordering::Relaxed);

		{
			let state = self.state.lock();
			if state.closed {
				self.counts.producers.fetch_sub(1, Ordering::Relaxed);
				return None;
			}
		}

		Some(Producer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		})
	}

	/// Create a new [`Consumer`] that shares this state.
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

	/// Acquire mutable access to the shared state without upgrading to a full [`Producer`].
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

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Poll-based mutable access with waker registration.
	///
	/// Calls `f` with a [`Mut`] guard. If `f` returns [`Poll::Pending`],
	/// registers the waiter for notification when the state next changes.
	/// Returns `None` if the channel is closed.
	pub fn poll_write<F, R>(&self, waiter: &Waiter, mut f: F) -> Poll<Option<R>>
	where
		F: FnMut(&mut Mut<'_, T>) -> Poll<R>,
	{
		let Ok(mut state) = self.write() else {
			return Poll::Ready(None);
		};

		if let Poll::Ready(res) = f(&mut state) {
			return Poll::Ready(Some(res));
		}

		// Reset modified so the drop doesn't immediately wake the waiter we're about to register.
		state.modified = false;

		let state = state.state.as_mut().unwrap();
		waiter.register(&mut state.waiters);
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
		match crate::wait(move |waiter| self.poll_write(waiter, &mut f)).await {
			Some(r) => Ok(r),
			None => Err(self.read()),
		}
	}

	/// Returns `true` if the channel has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.lock().closed
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
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Some(()));
		}

		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
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
		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
		}

		waiter.register(&mut state.waiters);

		// Re-check after registration to avoid TOCTOU race.
		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}

	/// Returns `true` if both weak references share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Clone for Weak<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

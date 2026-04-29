use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{
	Counts, State,
	lock::*,
	producer::{Producer, Ref},
	waiter::*,
	weak::Weak,
};

/// The consuming side of a shared state channel.
///
/// Consumers have read-only access to the shared value and are notified when
/// a producer modifies it. Cloning a consumer increments the consumer reference
/// count. When the last consumer is dropped, all waiters (e.g. [`Producer::unused`])
/// are notified.
#[derive(Debug)]
pub struct Consumer<T> {
	pub(crate) state: Lock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T> Consumer<T> {
	/// Poll the shared state with a closure.
	///
	/// Calls `f` with a [`Ref`]. If `f` returns [`Poll::Pending`] and the
	/// channel is still open, registers the [`Waiter`] for notification.
	/// Returns `Err(`[`Ref`]`)` if the channel has been closed while the
	/// condition returned by `f` is still pending.
	pub fn poll<F, R>(&self, waiter: &Waiter, mut f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<R>,
	{
		let state = self.state.lock();
		let consumer_state = Ref { state };

		if let Poll::Ready(res) = f(&consumer_state) {
			return Poll::Ready(Ok(res));
		}

		if consumer_state.state.closed {
			return Poll::Ready(Err(consumer_state));
		}

		// Re-extract state from consumer_state to register
		let mut state = consumer_state.state;
		waiter.register(&mut state.waiters);

		Poll::Pending
	}

	/// Poll for channel closure, registering the waiter if still open.
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(());
		}

		waiter.register(&mut state.waiters);
		Poll::Pending
	}

	/// Wait for the closure to return [`Poll::Ready`], re-polling on each state change.
	///
	/// Returns `Ok(R)` when the closure returns [`Poll::Ready`], or `Err(Ref)` with
	/// read-only access to the final state if the channel closes first.
	pub async fn wait<F, R>(&self, mut f: F) -> Result<R, Ref<'_, T>>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<R> + Unpin,
		R: Unpin,
	{
		crate::wait(move |waiter| self.poll(waiter, &mut f)).await
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
	}

	/// Upgrade to a Producer, returning `None` if the state is already closed.
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

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Returns `true` if both consumers share the same underlying state.
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

impl<T> Drop for Consumer<T> {
	fn drop(&mut self) {
		// Atomically decrement and check if we were the last consumer
		let prev = self.counts.consumers.fetch_sub(1, Ordering::AcqRel);
		if prev > 1 {
			return;
		}

		// We were the last consumer, need to wake waiters
		let mut waiters = {
			let mut state = self.state.lock();
			state.waiters.take()
		};

		waiters.wake();
	}
}

impl<T> Clone for Consumer<T> {
	fn clone(&self) -> Self {
		self.counts.consumers.fetch_add(1, Ordering::Relaxed);

		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

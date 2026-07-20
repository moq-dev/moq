use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{Closed, Counts, State, lock::*, producer::Ref, waiter::*, weak::ConsumerWeak};

/// The consuming side of a shared state channel.
///
/// Consumers have read-only access to the shared value and are notified when
/// a producer modifies it. Cloning a consumer increments the consumer reference
/// count. When the last consumer is dropped, the consumer-count waiters
/// (e.g. [`Producer::unused`](crate::Producer::unused)) are notified.
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
		waiter.register(&mut state.waiters_value);

		Poll::Pending
	}

	/// Poll for channel closure, registering the waiter if still open.
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(());
		}

		waiter.register(&mut state.waiters_closed);
		Poll::Pending
	}

	/// Wait for the closure to return [`Poll::Ready`], re-polling on each state change.
	///
	/// Returns `Ok(R)` when the closure returns [`Poll::Ready`], or [`Closed`] if the
	/// channel closes first. Unlike [`poll`](Self::poll) this hands back no [`Ref`], so
	/// no lock guard can be held across a later `.await`; call [`read`](Self::read) if
	/// you need the final state.
	pub async fn wait<F, R>(&self, mut f: F) -> Result<R, Closed>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<R> + Unpin,
	{
		// The `Ref` is dropped here inside the closure, releasing the lock before the
		// caller ever sees the result.
		crate::wait(move |waiter| self.poll(waiter, &mut f).map(|res| res.map_err(|_| Closed))).await
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Returns `true` if the channel has been closed by the producer.
	pub fn is_closed(&self) -> bool {
		self.state.lock().closed
	}

	/// Returns `true` if both consumers share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	/// Create a [`ConsumerWeak`] reference to this state.
	///
	/// Does not affect ref counts, so it won't prevent auto-close when all
	/// producers are dropped. The weak can only mint more consumers, never a
	/// producer, so read-only access stays read-only.
	pub fn weak(&self) -> ConsumerWeak<T> {
		ConsumerWeak {
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

		// We were the last consumer, so wake the `unused()` waiters. The value
		// and closed waiters don't care about the consumer count, so leave
		// them alone.
		let mut waiters = {
			let mut state = self.state.lock();
			state.waiters_consumer.take()
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

#[cfg(test)]
mod test {
	use crate::{Closed, Producer};
	use std::task::Poll;

	/// `wait` reports closure as a plain [`Closed`], holding no lock once it returns:
	/// the caller can bind the error, `.await` again, and still reach the final state.
	#[tokio::test]
	async fn wait_reports_closure_without_holding_the_lock() {
		let producer = Producer::new(0u32);
		let consumer = producer.consume();

		// Never satisfied, so the wait can only end via closure.
		let never = |v: &crate::Ref<'_, u32>| if **v == 99 { Poll::Ready(()) } else { Poll::Pending };

		producer.close().ok().expect("open");

		let err = consumer.wait(never).await.expect_err("closed");
		assert_eq!(err, Closed);

		// Awaiting while the error is still bound would deadlock if `Err` carried a
		// guard. The final state stays reachable through `read()`.
		tokio::task::yield_now().await;
		assert_eq!(err, Closed);
		assert_eq!(*consumer.read(), 0);
		assert!(consumer.is_closed());
	}
}

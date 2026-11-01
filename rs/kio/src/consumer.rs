use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{
	Closed, Counts,
	channel::{Channel, Ref},
	waiter::*,
	weak::ConsumerWeak,
};

/// The consuming side of a shared state channel.
///
/// Consumers have read-only access to the shared value and are notified when
/// a producer modifies it. Cloning a consumer increments the consumer reference
/// count. When the last consumer is dropped, the consumer-count waiters
/// (e.g. [`Producer::wanted_changed`](crate::Producer::wanted_changed)) are notified.
#[derive(Debug)]
pub struct Consumer<T> {
	pub(crate) state: Arc<Channel<T>>,
	pub(crate) counts: Arc<Counts>,
}

/// Mint a consumer, or `None` if `only_open` and the channel has closed.
///
/// Zero-to-one transitions share the state lock with idle teardown. Existing
/// demand can be incremented without relocking when closed-channel reads are allowed.
fn mint<T>(state: &Arc<Channel<T>>, counts: &Arc<Counts>, only_open: bool) -> Option<Consumer<T>> {
	let first = if !only_open
		&& counts
			.consumers
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
				(count > 0).then(|| count + 1)
			})
			.is_ok()
	{
		// The successful increment keeps the count above zero, so an idle teardown
		// cannot commit. Callers may already hold the state lock on this path.
		false
	} else {
		let guard = state.read();
		if only_open && guard.is_closed() {
			return None;
		}
		counts.consumers.fetch_add(1, Ordering::AcqRel) == 0
	};

	// Wake `wanted_changed()` waiters when the first consumer appears, after the lock is
	// released.
	if first {
		state.wake_consumer();
	}

	Some(Consumer {
		state: state.clone(),
		counts: counts.clone(),
	})
}

/// Mint a consumer, whether or not the channel is still open. A closed channel's
/// consumer reads its final value and then reports the closure.
pub(crate) fn consume<T>(state: &Arc<Channel<T>>, counts: &Arc<Counts>) -> Consumer<T> {
	mint(state, counts, false).expect("minting is infallible for an open-or-closed channel")
}

/// Mint a consumer only while the channel is open.
pub(crate) fn consume_open<T>(state: &Arc<Channel<T>>, counts: &Arc<Counts>) -> Option<Consumer<T>> {
	mint(state, counts, true)
}

/// Poll for the consumer count's demand to differ from `last`, for every handle that
/// offers [`Producer::poll_wanted_changed`](crate::Producer::poll_wanted_changed).
#[cfg_attr(feature = "sim", track_caller)]
pub(crate) fn poll_wanted_changed<T>(
	state: &Channel<T>,
	counts: &Counts,
	waiter: &Waiter,
	last: bool,
) -> Poll<Result<bool, Closed>> {
	// Closure is checked first, so a closed channel reports `Closed` from every handle
	// rather than a consumer count that only dropped because it's dead.
	let check = || {
		if state.is_closed() {
			return Some(Err(Closed));
		}
		let wanted = counts.consumers.load(Ordering::Relaxed) > 0;
		(wanted != last).then_some(Ok(wanted))
	};

	if let Some(res) = check() {
		return crate::coop::ready(res);
	}

	waiter.register(&mut state.waiters().consumer);

	// Re-check after registration: the count or the closed flag may have moved between
	// the first check and the registration, and its wake drained the list before this
	// waiter joined it. Both wake the list after moving, under its lock, so this sees it.
	if let Some(res) = check() {
		return crate::coop::ready(res);
	}

	Poll::Pending
}

impl<T> Consumer<T> {
	/// Poll the shared state with a closure.
	///
	/// Calls `f` with a [`Ref`]. If `f` returns [`Poll::Pending`] and the
	/// channel is still open, registers the [`Waiter`] for notification.
	/// Returns `Err(`[`Ref`]`)` if the channel has been closed while the
	/// condition returned by `f` is still pending.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll<F, R>(&self, waiter: &Waiter, f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<R>,
	{
		self.state.poll(waiter, f)
	}

	/// Poll for channel closure, registering the waiter if still open.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
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
		self.state.read()
	}

	/// Returns `true` if the channel has been closed by the producer.
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	/// Returns `true` if both consumers share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.state, &other.state)
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

		// We were the last consumer, so wake the `wanted_changed()` waiters. The value
		// and closed waiters don't care about the consumer count, so leave
		// them alone.
		self.state.wake_consumer();
	}
}

impl<T> Clone for Consumer<T> {
	fn clone(&self) -> Self {
		// Unlike minting one from scratch, this needs no lock: `self` is already
		// counted, so the count runs 1 -> 2 and a teardown holding the lock can never
		// read the zero that would let it commit.
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

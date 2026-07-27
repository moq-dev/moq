use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{
	Closed, Counts, State,
	consumer::Consumer,
	lock::*,
	producer::{Producer, Ref},
	waiter::*,
};

/// A handle that owns nothing at all ([`Producer::downgrade`](crate::Producer::downgrade)).
///
/// Unlike [`ProducerWeak`], which keeps the state allocated so it stays readable after
/// the channel closes, this drops with the last real handle. That makes it the one
/// handle you can store *inside* the state it points at (a child holding a link back to
/// its parent, say) without the two keeping each other alive forever.
///
/// [`upgrade`](Self::upgrade) it back to a [`Producer`] while the channel is still live.
pub struct Weak<T> {
	pub(crate) state: WeakLock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T> Weak<T> {
	/// A handle that never upgrades, mirroring [`std::sync::Weak::new`].
	///
	/// Useful as a placeholder for a link that isn't wired up yet, or that a
	/// standalone (channel-less) value doesn't have.
	pub fn new() -> Self {
		Self {
			state: WeakLock::new(),
			counts: Arc::new(Counts::default()),
		}
	}

	/// Recover a [`Producer`], or `None` once the state has been dropped or the
	/// channel closed.
	///
	/// This counts: while the returned producer lives the channel stays open, and
	/// dropping it can be what finally closes it.
	pub fn upgrade(&self) -> Option<Producer<T>> {
		// Reuse `produce`'s increment-then-check ordering, which is what keeps the
		// last `Producer::drop` from closing the channel out from under us.
		ProducerWeak {
			state: self.state.upgrade()?,
			counts: self.counts.clone(),
		}
		.produce()
	}
}

impl<T> Default for Weak<T> {
	fn default() -> Self {
		Self::new()
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

impl<T> std::fmt::Debug for Weak<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Weak")
			.field("alive", &self.state.upgrade().is_some())
			.finish()
	}
}

/// A weak handle from the producing side ([`Producer::weak`](crate::Producer::weak)).
///
/// Holds no ref count, so it never keeps the channel open. Upgrade it back to a [`Producer`]
/// (write access) or a [`Consumer`] (read access) while the channel is still live.
///
/// It does keep the state *allocated*, which is what lets it read a closed channel's final
/// value. Reach for [`Weak`] instead when the handle is stored inside that same state.
#[derive(Debug)]
pub struct ProducerWeak<T> {
	pub(crate) state: Lock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T> ProducerWeak<T> {
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

		// Wake `used()` waiters when the first consumer appears.
		if prev == 0 {
			let mut waiters = self.state.lock().waiters_consumer.take();
			waiters.wake();
		}

		Consumer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Returns `true` if the channel has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.lock().closed
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
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

	/// Wait until all consumers have been dropped.
	///
	/// Returns `Ok(())` when no consumers remain, or [`Closed`] if the channel closes first.
	pub async fn unused(&self) -> Result<(), Closed> {
		match crate::wait(move |waiter| self.poll_unused(waiter)).await {
			Some(()) => Ok(()),
			None => Err(Closed),
		}
	}

	/// Poll-based variant of [`Self::unused`]: `Ready(Some(()))` when no consumers
	/// remain, `Ready(None)` if the channel closed first, else `Pending`.
	pub fn poll_unused(&self, waiter: &Waiter) -> Poll<Option<()>> {
		// Closure is checked first, matching `Producer::poll_unused`: a closed channel
		// with no consumers resolves `None` from either handle.
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
		}

		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Some(()));
		}

		waiter.register(&mut state.waiters_consumer);

		// Re-check after registration to avoid TOCTOU race where the last
		// consumer drops between the initial check and waiter registration.
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}

	/// Whether any consumer handle currently exists.
	///
	/// A point-in-time snapshot with no registration; use [`Self::poll_used`] /
	/// [`Self::poll_unused`] to wait for the edge instead.
	pub fn is_used(&self) -> bool {
		self.counts.consumers.load(Ordering::Relaxed) > 0
	}

	/// Wait until at least one consumer exists.
	///
	/// Returns `Ok(())` when a consumer is created, or [`Closed`] if the channel closes first.
	pub async fn used(&self) -> Result<(), Closed> {
		match crate::wait(move |waiter| self.poll_used(waiter)).await {
			Some(()) => Ok(()),
			None => Err(Closed),
		}
	}

	/// Poll-based variant of [`Self::used`]: `Ready(Some(()))` once a consumer
	/// exists, `Ready(None)` if the channel closed first, else `Pending`.
	pub fn poll_used(&self, waiter: &Waiter) -> Poll<Option<()>> {
		// Closure is checked first, matching `Producer::poll_used`.
		let mut state = self.state.lock();
		if state.closed {
			return Poll::Ready(None);
		}

		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		waiter.register(&mut state.waiters_consumer);

		// Re-check after registration to avoid TOCTOU race.
		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}

	/// Returns `true` if both handles share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Clone for ProducerWeak<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

/// A weak handle from the consuming side ([`Consumer::weak`](crate::Consumer::weak)).
///
/// Holds no ref count, so it never keeps the channel open. Unlike [`ProducerWeak`] it can
/// only mint more [`Consumer`]s, so a read-only handle can never grow write access.
#[derive(Debug)]
pub struct ConsumerWeak<T> {
	pub(crate) state: Lock<State<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T> ConsumerWeak<T> {
	/// Create a new [`Consumer`] that shares this state.
	pub fn consume(&self) -> Consumer<T> {
		let prev = self.counts.consumers.fetch_add(1, Ordering::AcqRel);

		// Wake `used()` waiters when the first consumer appears.
		if prev == 0 {
			let mut waiters = self.state.lock().waiters_consumer.take();
			waiters.wake();
		}

		Consumer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Returns `true` if the channel has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.lock().closed
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
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

	/// Returns `true` if both handles share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Clone for ConsumerWeak<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A closed, consumer-free channel reports the same thing through either handle.
	/// Both check closure before the consumer count, so neither reports `Ok(())` for a
	/// channel that is merely out of consumers because it's dead.
	#[tokio::test]
	async fn weak_and_producer_agree_once_closed() {
		let producer = Producer::new(0u32);
		let weak = producer.weak();

		// No consumers were ever created, and the channel is now closed.
		producer.close().ok().expect("open");

		assert_eq!(producer.unused().await, Err(Closed));
		assert_eq!(weak.unused().await, Err(Closed));

		assert_eq!(producer.used().await, Err(Closed));
		assert_eq!(weak.used().await, Err(Closed));
	}

	/// While the channel is open the two handles still agree on the consumer count.
	#[tokio::test]
	async fn weak_and_producer_agree_while_open() {
		let producer = Producer::new(0u32);
		let weak = producer.weak();

		assert_eq!(producer.unused().await, Ok(()));
		assert_eq!(weak.unused().await, Ok(()));

		let consumer = producer.consume();
		assert_eq!(producer.used().await, Ok(()));
		assert_eq!(weak.used().await, Ok(()));

		drop(consumer);
		assert_eq!(weak.unused().await, Ok(()));
	}

	/// A state holding a handle back to itself is still deallocated once the last real
	/// handle drops. `ProducerWeak` would keep it (and everything it owns) alive forever.
	#[test]
	fn weak_breaks_a_self_reference() {
		struct Node {
			// Only its refcount matters: it's how the test sees the state deallocate.
			_alive: Arc<()>,
			back: Option<Weak<Node>>,
		}

		let alive = Arc::new(());
		let producer = Producer::new(Node {
			_alive: alive.clone(),
			back: None,
		});
		producer.write().ok().expect("open").back = Some(producer.downgrade());
		assert_eq!(Arc::strong_count(&alive), 2, "the state is alive");

		let weak = producer.downgrade();
		assert!(weak.upgrade().is_some(), "upgradeable while the channel is live");

		drop(producer);
		assert_eq!(Arc::strong_count(&alive), 1, "the state is gone despite the cycle");
		assert!(weak.upgrade().is_none());
	}

	/// A closed channel never upgrades, even while a consumer is still draining it.
	/// Producing into it again would resurrect a channel every reader has been told
	/// is over.
	#[test]
	fn weak_does_not_upgrade_once_closed() {
		let producer = Producer::new(0u32);
		let weak = producer.downgrade();
		let consumer = producer.consume();

		drop(producer);
		assert!(weak.upgrade().is_none(), "closed, despite the state being alive");

		drop(consumer);
		assert!(weak.upgrade().is_none());
	}

	/// An upgrade counts as a producer for as long as it lives, so the channel can't
	/// close underneath it.
	#[test]
	fn upgrade_holds_the_channel_open() {
		let producer = Producer::new(0u32);
		let weak = producer.downgrade();
		let consumer = producer.consume();

		let upgraded = weak.upgrade().expect("open");

		drop(producer);
		assert!(!consumer.is_closed(), "the upgrade keeps it open");

		drop(upgraded);
		assert!(consumer.is_closed(), "dropping the last one closes it");
	}

	#[tokio::test]
	async fn consumer_weak_reads_and_observes_close() {
		let producer = Producer::new(7u32);
		let consumer = producer.consume();
		let weak = consumer.weak();

		assert_eq!(*weak.read(), 7);
		assert!(!weak.is_closed());

		// Dropping the last producer closes the channel, resolving `closed()`.
		drop(producer);
		weak.closed().await;
		assert!(weak.is_closed());
		assert!(weak.read().is_closed());
	}
}

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

	/// Read the state while another handle keeps it allocated, even once the channel
	/// closed. Counts as neither a producer nor a consumer. `None` once it was dropped.
	pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
		let state = self.state.upgrade()?;
		let state = Ref { state: state.lock() };
		Some(f(&state))
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
/// Holds no ref count, so it never keeps the channel open. Upgrade it to a [`Producer`]
/// while the channel is live, or create a [`Consumer`] to read even after closure.
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
		{
			// Registering under the same lock that guards `closed` is what makes the
			// returned producer keep the channel open: a concurrent last-producer drop
			// either closes first (and we bail) or sees our count and doesn't close.
			let state = self.state.lock();
			if state.closed {
				return None;
			}
			self.counts.producers.fetch_add(1, Ordering::Relaxed);
		}

		Some(Producer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		})
	}

	/// Create a consumer that can read the final value even after the channel closes.
	pub fn consume(&self) -> Consumer<T> {
		crate::consumer::consume(&self.state, &self.counts)
	}

	/// Create a [`ConsumerWeak`], which reads the state without joining the consumer count.
	pub fn consume_weak(&self) -> ConsumerWeak<T> {
		ConsumerWeak {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	/// Create a consumer only if the channel is open.
	///
	/// The closed check and count increment share the lock held by
	/// [`Producer::write_unwanted`], so an idle teardown either sees this consumer
	/// or closes the channel before this returns `None`.
	pub fn try_consume(&self) -> Option<Consumer<T>> {
		crate::consumer::consume_open(&self.state, &self.counts)
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
	#[cfg_attr(feature = "_sim", track_caller)]
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		let mut state = self.state.lock();
		if state.closed {
			return crate::coop::ready(());
		}

		waiter.register(&mut state.waiters_closed);
		Poll::Pending
	}

	/// Whether any consumer handle exists right now.
	///
	/// A point-in-time snapshot with no registration; use
	/// [`Self::poll_wanted_changed`] to wait for it to change instead.
	pub fn wanted(&self) -> bool {
		self.counts.consumers.load(Ordering::Relaxed) > 0
	}

	/// Poll for [`Self::wanted`] to differ from `last`: `Ready(Ok)` with the new value once
	/// it does, [`Closed`] if the channel closed first, else `Pending`.
	#[cfg_attr(feature = "_sim", track_caller)]
	pub fn poll_wanted_changed(&self, waiter: &Waiter, last: bool) -> Poll<Result<bool, Closed>> {
		// Closure is checked first, so a closed channel reports `Closed` from every handle
		// rather than a consumer count that only dropped because it's dead.
		let mut state = self.state.lock();
		if state.closed {
			return crate::coop::ready(Err(Closed));
		}

		if self.wanted() != last {
			return crate::coop::ready(Ok(!last));
		}

		waiter.register(&mut state.waiters_consumer);

		// Re-check after registration to avoid a TOCTOU race where the count moves
		// between the initial check and waiter registration.
		if self.wanted() != last {
			return crate::coop::ready(Ok(!last));
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
	/// Create a consumer that can read the final value even after the channel closes.
	pub fn consume(&self) -> Consumer<T> {
		crate::consumer::consume(&self.state, &self.counts)
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			state: self.state.lock(),
		}
	}

	/// Poll the shared state with a closure, exactly as [`Consumer::poll`] does.
	///
	/// Available here so a watcher can register for changes without joining the
	/// consumer count, which is what [`Producer::wanted_changed`](crate::Producer::wanted_changed)
	/// keys off.
	#[cfg_attr(feature = "_sim", track_caller)]
	pub fn poll<F, R>(&self, waiter: &Waiter, f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<R>,
	{
		let state = self.state.lock();
		let consumer_state = Ref { state };

		if let Poll::Ready(res) = f(&consumer_state) {
			return crate::coop::ready(Ok(res));
		}

		if consumer_state.state.closed {
			return crate::coop::ready(Err(consumer_state));
		}

		let mut state = consumer_state.state;
		waiter.register(&mut state.waiters_value);

		Poll::Pending
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
	#[cfg_attr(feature = "_sim", track_caller)]
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		let mut state = self.state.lock();
		if state.closed {
			return crate::coop::ready(());
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

		for last in [false, true] {
			assert_eq!(producer.wanted_changed(last).await, Err(Closed));
			assert_eq!(crate::wait(|w| weak.poll_wanted_changed(w, last)).await, Err(Closed));
		}
	}

	/// While the channel is open the two handles still agree on the consumer count.
	#[tokio::test]
	async fn weak_and_producer_agree_while_open() {
		let producer = Producer::new(0u32);
		let weak = producer.weak();

		assert!(!producer.wanted());
		assert!(!weak.wanted());

		let consumer = producer.consume();
		assert_eq!(producer.wanted_changed(false).await, Ok(true));
		assert_eq!(crate::wait(|w| weak.poll_wanted_changed(w, false)).await, Ok(true));

		drop(consumer);
		assert_eq!(crate::wait(|w| weak.poll_wanted_changed(w, true)).await, Ok(false));
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

	/// A closed channel stays readable through the weak handle while any handle keeps
	/// it allocated, without that read reopening it or counting as demand.
	#[test]
	fn weak_reads_a_closed_channel() {
		let producer = Producer::new(7u32);
		let weak = producer.downgrade();
		let consumer = producer.consume();

		drop(producer);
		assert_eq!(weak.read(|value| *value), Some(7));
		assert!(weak.upgrade().is_none(), "reading does not reopen it");

		drop(consumer);
		assert_eq!(weak.read(|value| *value), None);
	}

	/// An upgrade racing the last producer's drop either loses (no handle) or wins
	/// (a handle that is genuinely open). It must never hand back a producer for a
	/// channel that the drop is about to close.
	///
	/// A smoke test, not a reproducer: the window is a few instructions wide and this
	/// doesn't trip on the unsynchronized ordering even with the barrier. What rules
	/// the interleaving out is doing the count transition under the state lock.
	#[test]
	fn upgrade_never_wins_a_closing_channel() {
		for _ in 0..2_000 {
			let producer = Producer::new(0u32);
			let weak = producer.downgrade();
			// Keeps the state allocated so the upgrade is about the channel, not the
			// allocation.
			let consumer = producer.consume();

			// Line both threads up on the drop/upgrade window.
			let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
			let dropper = {
				let gate = gate.clone();
				std::thread::spawn(move || {
					gate.wait();
					drop(producer);
				})
			};

			gate.wait();
			if let Some(upgraded) = weak.upgrade() {
				assert!(
					upgraded.write().is_ok(),
					"an upgrade must not resolve a closing channel"
				);
			}
			dropper.join().expect("dropper panicked");
			drop(consumer);
		}
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

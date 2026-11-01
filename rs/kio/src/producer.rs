use std::{
	sync::{Arc, atomic::Ordering},
	task::Poll,
};

use crate::{
	Closed, Counts,
	channel::{Channel, Mut, Ref},
	consumer::{self, Consumer},
	waiter::*,
	weak::{ProducerWeak, Weak},
};

/// The producing side of a shared state channel.
///
/// Producers hold mutable access to the shared value. When the state is modified
/// through [`Mut`], all registered consumers are automatically notified.
/// Cloning a producer increments the producer reference count. When the last
/// producer is dropped, the channel is closed.
#[derive(Debug)]
pub struct Producer<T> {
	pub(crate) state: Arc<Channel<T>>,
	pub(crate) counts: Arc<Counts>,
}

impl<T: Default> Default for Producer<T> {
	fn default() -> Self {
		Self {
			state: Arc::default(),
			counts: Arc::new(Counts::default()),
		}
	}
}

impl<T> Producer<T> {
	/// Heap bytes this channel occupies, for callers budgeting memory per channel.
	///
	/// A channel owns two allocations regardless of how it is used: the state cell
	/// holding `T` and its mutex, the version, and the three waiter lists under their own
	/// mutex, plus the reference counts
	/// beside it. Both are sized at compile time, so a caller charging per channel can
	/// derive its constant from this rather than measuring a process and pasting the
	/// number.
	///
	/// [`Consumer`] has no counterpart because it shares these same two allocations:
	/// [`consume`](Self::consume) hands back two more pointers and bumps a count, so a
	/// channel costs this whether it has one consumer or a thousand.
	///
	/// Excludes whatever `T` allocates on its own.
	pub const HEAP: usize = crate::arc_heap::<Channel<T>>() + crate::arc_heap::<Counts>();

	/// Create a new producer with the given initial value.
	pub fn new(value: T) -> Self {
		Self {
			state: Arc::new(Channel::new(value)),
			counts: Arc::new(Counts::default()),
		}
	}

	/// Create a new [`Consumer`] that shares this producer's state.
	pub fn consume(&self) -> Consumer<T> {
		consumer::consume(&self.state, &self.counts)
	}

	/// Acquire mutable access only while nothing is consuming the channel.
	///
	/// The count is read under the lock [`consume`](Self::consume) takes when
	/// demand is zero, so the returned guard keeps the channel unwanted until it
	/// drops. That is what a teardown decided by [`poll_wanted_changed`](Self::poll_wanted_changed)
	/// needs: the poll is a level snapshot, so committing on it directly can cancel
	/// a consumer that appeared in the gap. Commit through this instead, and take
	/// [`Unwanted::Wanted`] as "a consumer got there first, keep going".
	///
	/// Pair it with [`ProducerWeak::try_consume`], which refuses to mint a consumer for
	/// a closed channel: between the two, a consumer either exists in time to
	/// decline the teardown or is never handed out at all.
	pub fn write_unwanted(&self) -> Unwanted<'_, T> {
		let state = self.state.read();
		if state.is_closed() {
			return Unwanted::Closed;
		}

		// Relaxed is enough: a zero-to-one increment happens under this lock.
		// Further increments cannot pass through zero. A decrement runs outside
		// it, so this may still read a consumer that is going away, which only
		// declines a teardown that the next `wanted_changed` wake will offer again.
		if self.counts.consumers.load(Ordering::Relaxed) > 0 {
			return Unwanted::Wanted;
		}

		Unwanted::Idle(Mut::new(state))
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
		let state = self.state.read();
		if state.is_closed() {
			Err(state)
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
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll<F>(&self, waiter: &Waiter, f: F) -> Poll<Result<Mut<'_, T>, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<()>,
	{
		let guard = self.state.read();
		// Read under the lock, so it is the version of the value `f` judges.
		let seen = self.state.version();
		if guard.is_closed() {
			return crate::coop::ready(Err(guard));
		}

		match f(&guard) {
			// Upgrade the Ref to a Mut, keeping the same lock guard.
			Poll::Ready(()) => crate::coop::ready(Ok(Mut::new(guard))),
			Poll::Pending => {
				drop(guard);
				self.state.park_value(waiter, seen);
				Poll::Pending
			}
		}
	}

	/// Poll read-only access with waker registration.
	///
	/// Like [`Self::poll`] but hands `f` a [`Ref`] instead of returning a
	/// [`Mut`], so it never flags the state modified and never wakes consumers.
	/// Use it to wait on a read condition from the producer side without creating
	/// a [`Consumer`].
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll_ref<F, R>(&self, waiter: &Waiter, f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<R>,
	{
		self.state.poll(waiter, f)
	}

	/// Wait until the read-only predicate holds, then acquire write access.
	///
	/// The async sibling of [`poll`](Self::poll): returns `Ok(Mut)` once `f` returns
	/// [`Poll::Ready`], or [`Closed`] if the channel closes first. The `Ok` guard is the
	/// write access you asked for, so it's yours to hold; the `Err` case hands back no
	/// guard at all. Call [`read`](Self::read) if you need the final state.
	pub async fn wait<F>(&self, mut f: F) -> Result<Mut<'_, T>, Closed>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<()> + Unpin,
	{
		// The `Ref` is dropped here inside the closure, releasing the lock before the
		// caller ever sees the error.
		crate::wait(move |waiter| self.poll(waiter, &mut f).map(|res| res.map_err(|_| Closed))).await
	}

	/// Wait until the channel is closed.
	pub async fn closed(&self) {
		crate::wait(move |waiter| self.poll_closed(waiter)).await
	}

	/// Poll for channel closure (an explicit [`Mut::close`], e.g. an abort),
	/// registering the waiter if still open.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
	}

	/// Whether any consumer handle exists right now.
	///
	/// A point-in-time snapshot with no registration; use
	/// [`Self::poll_wanted_changed`] to wait for it to change instead.
	pub fn wanted(&self) -> bool {
		self.counts.consumers.load(Ordering::Relaxed) > 0
	}

	/// Wait until [`Self::wanted`] differs from `last`, returning the new value, or
	/// [`Closed`] if the channel closes first.
	pub async fn wanted_changed(&self, last: bool) -> Result<bool, Closed> {
		crate::wait(move |waiter| self.poll_wanted_changed(waiter, last)).await
	}

	/// Poll for [`Self::wanted`] to differ from `last`: `Ready(Ok)` with the new value once
	/// it does, [`Closed`] if the channel closed first, else `Pending`.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll_wanted_changed(&self, waiter: &Waiter, last: bool) -> Poll<Result<bool, Closed>> {
		crate::consumer::poll_wanted_changed(&self.state, &self.counts, waiter, last)
	}

	/// Get read-only access to the shared state.
	pub fn read(&self) -> Ref<'_, T> {
		self.state.read()
	}

	/// Returns `true` if both producers share the same underlying state.
	pub fn same_channel(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.state, &other.state)
	}

	/// Create a [`ProducerWeak`] reference that doesn't affect the producer/consumer ref counts.
	pub fn weak(&self) -> ProducerWeak<T> {
		ProducerWeak {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	/// Create a [`Weak`] reference that owns nothing, not even the state allocation.
	///
	/// Use this instead of [`Self::weak`] for a handle stored inside the state itself,
	/// where a [`ProducerWeak`] would keep the allocation alive through its own value.
	pub fn downgrade(&self) -> Weak<T> {
		Weak {
			state: Arc::downgrade(&self.state),
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
		{
			// The count moves under the state lock, in step with the closed flag it
			// decides. Decrementing outside it would let `ProducerWeak::produce` slip
			// between the decrement and the close, handing back a producer for a
			// channel that is about to close.
			let state = self.state.read();
			if self.counts.producers.fetch_sub(1, Ordering::AcqRel) > 1 {
				return;
			}
			if state.is_closed() {
				return;
			}

			// We were the last producer, so close.
			self.state.close(&state);
		}

		// Every waiter reacts to closure (value/closed resolve, `wanted_changed` resolves
		// to `Closed`), so wake every list now the lock is released.
		self.state.wake(true);
	}
}

/// What [`Producer::write_unwanted`] found.
#[must_use]
#[derive(Debug)]
pub enum Unwanted<'a, T> {
	/// Nothing is consuming the channel: write access, held under the lock a new
	/// consumer has to take, so the write lands before one can exist.
	Idle(Mut<'a, T>),
	/// A consumer exists, so whatever was decided while the channel looked unwanted
	/// is stale.
	Wanted,
	/// The channel is already closed, so there is nothing left to write.
	Closed,
}

#[cfg(test)]
mod test {
	use super::*;

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

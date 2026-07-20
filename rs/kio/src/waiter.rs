use std::{
	fmt,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	sync::{Arc, OnceLock, Weak},
	task::{Context, Poll, Waker},
};

use smallvec::SmallVec;

/// Number of slots stored inline before spilling to the heap.
const INLINE_WAITERS: usize = 32;

/// Handle passed to poll functions for registering with [`WaiterList`]s.
///
/// Holds the task's [`Waker`] by value and, lazily, a shared `Arc<Waker>` that list
/// entries reference weakly. The `Arc` is allocated on the first [`Self::register`],
/// so a poll that resolves without ever parking never touches the heap. Its `Weak`s
/// go dead the moment the owning [`Waiter`] drops, which is how a [`WaiterList`]
/// reclaims slots with no explicit deregister.
pub struct Waiter {
	// The task waker. Cloning it is cheap (an atomic bump, no allocation).
	waker: Waker,

	// The shared handle downgraded into every list this waiter registers with. Created on the
	// first `register` (a poll that never parks never allocates it), then reused so multiple
	// lists in one poll share a single allocation whose `Weak`s die together when the waiter drops.
	shared: OnceLock<Arc<Waker>>,
}

impl Waiter {
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self {
			waker,
			shared: OnceLock::new(),
		}
	}

	/// Create a no-op waiter that discards registrations.
	pub fn noop() -> Self {
		Self::new(Waker::noop().clone())
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}

	/// The underlying task [`Waker`], for hand-rolling foreign-future integration. Prefer
	/// [`poll_future`](Self::poll_future), which wraps the usual [`Context`] dance.
	pub fn waker(&self) -> &Waker {
		&self.waker
	}

	/// The shared waker handle downgraded into lists, allocated on first use and cached so
	/// repeat registrations (across polls, or across lists in one poll) share one allocation.
	fn shared(&self) -> &Arc<Waker> {
		self.shared.get_or_init(|| Arc::new(self.waker.clone()))
	}

	/// Poll a foreign [`Future`] against this waiter, so it re-wakes the enclosing
	/// `poll_*` step when it is ready.
	pub fn poll_future<F: Future + ?Sized>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
		future.poll(&mut Context::from_waker(self.waker()))
	}
}

/// A list of weak wakers waiting for notification.
///
/// Slots live inline (up to `INLINE_WAITERS`) and only spill to the heap
/// for unusually high concurrency. A rotating cursor amortizes garbage
/// collection across many `register` calls so the list doesn't grow
/// unboundedly while keeping per-call cost O(1).
pub struct WaiterList {
	entries: SmallVec<[Weak<Waker>; INLINE_WAITERS]>,
	/// Rotating cursor for opportunistic GC on `register`.
	cursor: usize,
}

impl WaiterList {
	/// Create an empty list, allocating nothing until the first [`register`](Self::register).
	pub fn new() -> Self {
		Self {
			entries: SmallVec::new(),
			cursor: 0,
		}
	}

	/// Register a waiter.
	///
	/// Performs a small, bounded amount of garbage collection: probes the
	/// slot at the rotating cursor, replacing it in place if dead. The
	/// cursor advances on each append so the probe window covers the
	/// whole list over time.
	pub fn register(&mut self, waiter: &Waiter) {
		let new_weak = Arc::downgrade(waiter.shared());

		for _ in 0..self.entries.len().min(2) {
			if self.entries[self.cursor].strong_count() == 0 {
				// Reuse the dead slot in place. Each Waiter owns a
				// unique Arc<Waker>, so strong_count == 0 uniquely
				// identifies a slot whose owner has been dropped.
				// No will_wake / pointer comparison needed.
				self.entries[self.cursor] = new_weak;
				return;
			}
			self.cursor = (self.cursor + 1) % self.entries.len();
		}

		self.entries.push(new_weak);
	}

	/// Drain all entries into a new [`WaiterList`], leaving this one empty.
	pub fn take(&mut self) -> Self {
		self.cursor = 0;
		Self {
			entries: std::mem::take(&mut self.entries),
			cursor: 0,
		}
	}

	/// Wake all live waiters, draining the list.
	pub fn wake(&mut self) {
		self.cursor = 0;
		for waker in self.entries.drain(..).filter_map(|w| w.upgrade()) {
			waker.wake_by_ref();
		}
	}
}

impl Default for WaiterList {
	fn default() -> Self {
		Self::new()
	}
}

impl fmt::Debug for WaiterList {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("WaiterList").field("len", &self.entries.len()).finish()
	}
}

/// Future that drives a poll function, managing waiter lifetime across polls.
struct WaiterFn<F, R> {
	poll: F,
	waiter: Option<Waiter>, // Store the previous waiter to avoid dropping it.
	// `fn() -> R` keeps the marker `Unpin` (and `Send`/`Sync`) regardless of `R`:
	// the output is only ever moved out of `Poll::Ready`, never stored.
	_marker: PhantomData<fn() -> R>,
}

/// Create a [`Future`] from a poll function that receives a [`Waiter`].
///
/// The waiter is kept alive between polls so its registration in a
/// [`WaiterList`] remains valid until the next poll replaces it.
pub fn wait<F, R>(poll: F) -> impl Future<Output = R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
{
	WaiterFn {
		poll,
		waiter: None,
		_marker: PhantomData,
	}
}

impl<F, R> Future for WaiterFn<F, R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
{
	type Output = R;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R> {
		let this = &mut *self;
		// Replacing drops the previous waiter, killing its Weak ref in the
		// list so the inner poll function's register call can recycle it.
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		(this.poll)(this.waiter.as_ref().unwrap())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn poll_future_bridges_a_std_future() {
		let waiter = Waiter::noop();

		// A ready future resolves through the waiter.
		let fut = std::pin::pin!(std::future::ready(7u8));
		assert_eq!(waiter.poll_future(fut), Poll::Ready(7));

		// A never-ready future stays pending.
		let fut = std::pin::pin!(std::future::pending::<u8>());
		assert_eq!(waiter.poll_future(fut), Poll::Pending);

		// A type-erased future works too (the `?Sized` bound).
		let mut boxed: Pin<Box<dyn Future<Output = u8>>> = Box::pin(std::future::ready(9u8));
		assert_eq!(waiter.poll_future(boxed.as_mut()), Poll::Ready(9));
	}

	// `Waiter` is shared behind `&self` across threads, so the lazily allocated
	// `shared` handle must use a thread-safe cell. A `!Sync` waiter silently
	// infects `Pending` and `Shared`, and through them every moq-net consumer.
	const fn assert_sync<T: Sync>() {}

	const _: () = {
		assert_sync::<Waiter>();
		assert_sync::<crate::Pending<crate::Consumer<u32>>>();
		assert_sync::<crate::Shared<u32>>();
	};

	#[test]
	fn wait_output_need_not_be_unpin() {
		struct NotUnpin(#[allow(dead_code)] std::marker::PhantomPinned);

		let mut fut = std::pin::pin!(crate::wait(|_| Poll::Ready(NotUnpin(std::marker::PhantomPinned))));
		let mut cx = Context::from_waker(Waker::noop());
		assert!(fut.as_mut().poll(&mut cx).is_ready());
	}
}

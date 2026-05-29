use std::{
	fmt,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	sync::{Arc, Weak},
	task::{Context, Poll, Waker},
};

use smallvec::SmallVec;

/// Number of slots stored inline before spilling to the heap.
const INLINE_WAITERS: usize = 32;

/// Handle passed to poll functions for registering with [`WaiterList`]s.
///
/// Each waiter owns an `Arc<Waker>`; list entries hold a `Weak<Waker>` that
/// becomes dead as soon as the owning [`Waiter`] is dropped. The list
/// reclaims those dead slots in place on the next register call without
/// needing to walk the whole list or do any explicit removal.
pub struct Waiter {
	waker: Arc<Waker>,
}

impl Waiter {
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self { waker: Arc::new(waker) }
	}

	/// Create a no-op waiter that discards registrations.
	///
	/// Registrations are stored as `Weak<Waker>` refs, so a noop waiter's
	/// weak ref will just be cleaned up on the next register call.
	pub fn noop() -> Self {
		Self {
			waker: Arc::new(std::task::Waker::noop().clone()),
		}
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
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
		let new_weak = Arc::downgrade(&waiter.waker);

		for _ in 0..self.entries.len().min(2) {
			if self.entries[self.cursor].strong_count() == 0 {
				// Reuse the dead slot in place. Each Waiter owns a
				// unique Arc<Waker>, so strong_count == 0 uniquely
				// identifies a slot whose owner has been dropped —
				// no will_wake / pointer comparison needed.
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
	_marker: PhantomData<R>,
}

/// Create a [`Future`] from a poll function that receives a [`Waiter`].
///
/// The waiter is kept alive between polls so its registration in a
/// [`WaiterList`] remains valid until the next poll replaces it.
pub fn wait<F, R>(poll: F) -> impl Future<Output = R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
	R: Unpin,
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
	R: Unpin,
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

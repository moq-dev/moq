use std::{
	fmt,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Waker},
};

use smallvec::SmallVec;

/// Number of slots stored inline before spilling to the heap.
const INLINE_WAITERS: usize = 32;

/// Handle passed to poll functions for registering with [`WaiterList`]s.
///
/// Each waiter holds an `Arc<AtomicBool>` "active" flag that is shared with
/// every list slot it has been registered into. Dropping the [`Waiter`]
/// flips the flag to `false`, marking those slots as garbage so the next
/// [`WaiterList::register`] call can reclaim them in place — no list
/// traversal or removal needed.
pub struct Waiter {
	waker: Waker,
	active: Arc<AtomicBool>,
}

impl Waiter {
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self {
			waker,
			active: Arc::new(AtomicBool::new(true)),
		}
	}

	/// Create a no-op waiter that discards registrations.
	///
	/// The waiter goes out of scope immediately after registering, so its
	/// slot is marked inactive and will be reclaimed on the next register.
	pub fn noop() -> Self {
		Self::new(std::task::Waker::noop().clone())
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}
}

impl Drop for Waiter {
	fn drop(&mut self) {
		// Mark any slots registered by this waiter as inactive so the next
		// register call can reclaim them without further bookkeeping.
		self.active.store(false, Ordering::Release);
	}
}

struct Slot {
	waker: Waker,
	active: Arc<AtomicBool>,
}

impl Slot {
	fn from_waiter(waiter: &Waiter) -> Self {
		Self {
			waker: waiter.waker.clone(),
			active: waiter.active.clone(),
		}
	}

	fn is_active(&self) -> bool {
		self.active.load(Ordering::Acquire)
	}
}

/// A list of wakers waiting for notification.
///
/// Slots live inline (up to [`INLINE_WAITERS`]) and only spill to the heap
/// for unusually high concurrency. Each slot shares an `AtomicBool` with
/// its [`Waiter`], so a slot can be detected as dead in O(1) without
/// walking the list — the previous `Weak<Waker>` upgrade dance is gone.
pub struct WaiterList {
	entries: SmallVec<[Slot; INLINE_WAITERS]>,
}

impl WaiterList {
	pub fn new() -> Self {
		Self {
			entries: SmallVec::new(),
		}
	}

	/// Register a waiter.
	///
	/// Performs opportunistic garbage collection: scans up to two existing
	/// slots and reuses the first inactive one for the new registration.
	/// Otherwise appends a new slot. This keeps the per-call cost O(1)
	/// while preventing unbounded growth in the common single- or
	/// few-waiter case.
	pub fn register(&mut self, waiter: &Waiter) {
		for slot in self.entries.iter_mut().take(2) {
			if !slot.is_active() {
				*slot = Slot::from_waiter(waiter);
				return;
			}
		}

		self.entries.push(Slot::from_waiter(waiter));
	}

	/// Drain all entries into a new [`WaiterList`], leaving this one empty.
	pub fn take(&mut self) -> Self {
		Self {
			entries: std::mem::take(&mut self.entries),
		}
	}

	/// Wake all live waiters, consuming the list.
	pub fn wake(self) {
		for slot in self.entries {
			// Skip slots whose owning Waiter has already been dropped.
			if slot.is_active() {
				slot.waker.wake();
			}
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
		// Replacing drops the previous waiter, marking its slot inactive so
		// the inner poll function's register call can recycle that slot.
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		(this.poll)(this.waiter.as_ref().unwrap())
	}
}

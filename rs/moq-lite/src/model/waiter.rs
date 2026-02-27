use std::{
	collections::VecDeque,
	fmt,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	sync::{Arc, Weak},
	task::{Context, Poll, Waker},
};

/// Handle passed to poll functions for registering with WaiterLists
pub struct Waiter {
	waker: Arc<Waker>,
}

impl Waiter {
	pub fn new(waker: Waker) -> Self {
		Self { waker: Arc::new(waker) }
	}

	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}

	/*
	/// Create a no-op waiter for synchronous polling (won't register)
	pub fn noop() -> Self {
		Self {
			waker: Arc::new(Waker::noop().clone()),
		}
	}
	*/
}

/// A list of weak wakers waiting for notification
///
/// Uses a ring buffer that self-cleans dead entries on register.
pub struct WaiterList {
	// TODO replace with an inline array, avoiding heap allocations for small collections.
	entries: VecDeque<Weak<Waker>>,
}

impl WaiterList {
	pub fn new() -> Self {
		Self {
			entries: VecDeque::new(),
		}
	}

	/// Register a waiter. Cleans up dead entries from the front first.
	pub fn register(&mut self, waiter: &Waiter) {
		// Clean up dead entries at front that fail to upgrade
		while let Some(front) = self.entries.pop_front() {
			if front.strong_count() == 0 {
				// Dead entry, skip
				continue;
			}

			// Add it to the back so we'll start at a different entry next time.
			self.entries.push_back(front);
			break;
		}

		self.entries.push_back(Arc::downgrade(&waiter.waker));
	}

	pub fn take(&mut self) -> Self {
		Self {
			entries: std::mem::take(&mut self.entries),
		}
	}

	// TODO Reuse the list instead of taking ownership.
	pub fn wake(mut self) {
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

/// Future wrapper that manages waiter state
pub struct WaiterFn<F, R> {
	poll: F,
	waiter: Option<Waiter>, // Store the previous waiter to avoid dropping it.
	_marker: PhantomData<R>,
}

pub fn waiter_fn<F, R>(poll: F) -> WaiterFn<F, R>
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
	R: Unpin,
{
	type Output = R;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R> {
		let this = &mut *self;
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		(this.poll)(this.waiter.as_ref().unwrap())
	}
}

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
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self { waker: Arc::new(waker) }
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}
}

/// A list of weak wakers waiting for notification
///
/// Uses a ring buffer that self-cleans dead entries on register.
pub struct WaiterList {
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

	/// Drain all entries into a new [`WaiterList`], leaving this one empty.
	pub fn take(&mut self) -> Self {
		Self {
			entries: std::mem::take(&mut self.entries),
		}
	}

	/// Wake all live waiters, consuming the list.
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
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		(this.poll)(this.waiter.as_ref().unwrap())
	}
}

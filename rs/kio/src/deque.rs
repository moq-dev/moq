use std::{collections::VecDeque, fmt, task::Poll};

use crate::{
	Closed,
	lock::Lock,
	waiter::{Waiter, WaiterList},
};

/// The push failed; the item is handed back.
pub enum PushError<T> {
	/// The queue is at capacity. Only a bounded [`Deque`]'s `try_push` reports this.
	Full(T),
	/// The queue was closed; nothing will ever pop the item.
	Closed(T),
}

impl<T> PushError<T> {
	/// Recover the item that failed to push.
	pub fn into_inner(self) -> T {
		match self {
			Self::Full(item) | Self::Closed(item) => item,
		}
	}
}

// Manual, so the error is debuggable without requiring `T: Debug`.
impl<T> fmt::Debug for PushError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Full(_) => f.debug_tuple("Full").finish_non_exhaustive(),
			Self::Closed(_) => f.debug_tuple("Closed").finish_non_exhaustive(),
		}
	}
}

impl<T> fmt::Display for PushError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Full(_) => write!(f, "queue full"),
			Self::Closed(_) => write!(f, "queue closed"),
		}
	}
}

impl<T> std::error::Error for PushError<T> {}

#[derive(Debug)]
struct State<T> {
	queue: VecDeque<T>,
	/// `None` = unbounded.
	capacity: Option<usize>,
	closed: bool,
	/// Parked pops, woken by a push or close.
	waiters_pop: WaiterList,
	/// Parked pushes, woken by a pop or close. Always empty when unbounded.
	waiters_push: WaiterList,
}

impl<T> State<T> {
	fn has_space(&self) -> bool {
		self.capacity.is_none_or(|capacity| self.queue.len() < capacity)
	}
}

/// A poll-native FIFO queue with waker notification.
///
/// Role-less like [`Shared`](crate::Shared): every clone can push, pop, or close,
/// and there is no liveness of its own — closure is explicit via
/// [`close`](Self::close), never implied by dropping handles. Bounded queues park
/// pushes at capacity ([`poll_push_with`](Self::poll_push_with) /
/// [`push`](Self::push)) or reject them ([`try_push`](Self::try_push)); pops park
/// while empty. After close, pops drain what's queued before reporting [`Closed`].
///
/// An event wakes *every* waiter parked on the affected side (there is no
/// wake-one): with several handles racing to pop, one wins and the rest re-park.
#[derive(Debug)]
pub struct Deque<T> {
	state: Lock<State<T>>,
}

impl<T> Deque<T> {
	/// Create an unbounded queue: pushes never park or report full.
	pub fn new() -> Self {
		Self::with_capacity(None)
	}

	/// Create a bounded queue holding at most `capacity` items.
	///
	/// Panics if `capacity` is zero, which could never accept an item.
	pub fn bounded(capacity: usize) -> Self {
		assert!(capacity > 0, "a zero-capacity Deque could never accept an item");
		Self::with_capacity(Some(capacity))
	}

	fn with_capacity(capacity: Option<usize>) -> Self {
		Self {
			state: Lock::new(State {
				queue: VecDeque::new(),
				capacity,
				closed: false,
				waiters_pop: WaiterList::new(),
				waiters_push: WaiterList::new(),
			}),
		}
	}

	/// Push without waiting, or hand the item back when closed or (if bounded) full.
	pub fn try_push(&self, item: T) -> Result<(), PushError<T>> {
		let mut waiters = {
			let mut state = self.state.lock();
			if state.closed {
				return Err(PushError::Closed(item));
			}
			if !state.has_space() {
				return Err(PushError::Full(item));
			}
			state.queue.push_back(item);
			state.waiters_pop.take()
		};
		waiters.wake();
		Ok(())
	}

	/// Poll to push, building the item only once there is room for it.
	///
	/// The item comes from a closure so a pending poll costs nothing: nothing is
	/// built, nothing needs handing back, and the caller retries with a fresh
	/// closure. `make` runs with the queue lock held — it must not touch this queue
	/// (or anything that does).
	///
	/// Registers `waiter` while full; a pop or close re-polls it.
	pub fn poll_push_with<F: FnOnce() -> T>(&self, waiter: &Waiter, make: F) -> Poll<Result<(), Closed>> {
		let mut waiters = {
			let mut state = self.state.lock();
			if state.closed {
				return Poll::Ready(Err(Closed));
			}
			if !state.has_space() {
				waiter.register(&mut state.waiters_push);
				return Poll::Pending;
			}
			let item = make();
			state.queue.push_back(item);
			state.waiters_pop.take()
		};
		waiters.wake();
		Poll::Ready(Ok(()))
	}

	/// Push, waiting for room while a bounded queue is full.
	///
	/// Returns [`Closed`] if the queue closes first (or already was); the item is
	/// dropped in that case — use [`try_push`](Self::try_push) to get it back.
	pub async fn push(&self, item: T) -> Result<(), Closed> {
		let mut item = Some(item);
		// Capture the slot by `&mut` so the closure is `Unpin` regardless of `T`.
		let slot = &mut item;
		crate::wait(move |waiter| self.poll_push_with(waiter, || slot.take().expect("polled after completion"))).await
	}

	/// Pop without waiting.
	///
	/// `Ok(None)` means the queue is empty but still open. Queued items drain
	/// before closure is reported: [`Closed`] means empty *and* closed.
	pub fn try_pop(&self) -> Result<Option<T>, Closed> {
		let (item, mut waiters) = {
			let mut state = self.state.lock();
			match state.queue.pop_front() {
				Some(item) => (item, state.waiters_push.take()),
				None if state.closed => return Err(Closed),
				None => return Ok(None),
			}
		};
		waiters.wake();
		Ok(Some(item))
	}

	/// Poll for the next item.
	///
	/// Queued items drain before closure is reported, so [`Closed`] means empty
	/// *and* closed. Registers `waiter` while empty; a push or close re-polls it.
	pub fn poll_pop(&self, waiter: &Waiter) -> Poll<Result<T, Closed>> {
		let (item, mut waiters) = {
			let mut state = self.state.lock();
			match state.queue.pop_front() {
				Some(item) => (item, state.waiters_push.take()),
				None if state.closed => return Poll::Ready(Err(Closed)),
				None => {
					waiter.register(&mut state.waiters_pop);
					return Poll::Pending;
				}
			}
		};
		waiters.wake();
		Poll::Ready(Ok(item))
	}

	/// Pop, waiting for an item while the queue is empty.
	///
	/// Returns [`Closed`] once the queue is both closed and drained.
	pub async fn pop(&self) -> Result<T, Closed> {
		crate::wait(move |waiter| self.poll_pop(waiter)).await
	}

	/// Close the queue: pushes fail from here on, pops drain what's queued and then
	/// report [`Closed`]. Idempotent.
	pub fn close(&self) {
		let mut waiters = {
			let mut state = self.state.lock();
			if state.closed {
				return;
			}
			state.closed = true;
			// Every waiter reacts to closure; wake both sides after unlocking.
			[state.waiters_pop.take(), state.waiters_push.take()]
		};
		for list in &mut waiters {
			list.wake();
		}
	}

	/// Whether [`close`](Self::close) has been called. Items may still be queued.
	pub fn is_closed(&self) -> bool {
		self.state.lock().closed
	}

	/// Number of items currently queued.
	pub fn len(&self) -> usize {
		self.state.lock().queue.len()
	}

	/// Whether nothing is currently queued.
	pub fn is_empty(&self) -> bool {
		self.state.lock().queue.is_empty()
	}

	/// The capacity bound, or `None` when unbounded.
	pub fn capacity(&self) -> Option<usize> {
		self.state.lock().capacity
	}

	/// Returns `true` if both handles share the same underlying queue.
	pub fn same_channel(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Default for Deque<T> {
	fn default() -> Self {
		Self::new()
	}
}

impl<T> Clone for Deque<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
		}
	}
}

#[cfg(all(test, not(loom)))]
mod test {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Context, Wake, Waker},
	};

	use super::*;
	use crate::WaiterCell;

	/// A waker that counts how many times it was woken (mirrors `tests.rs`).
	struct CountWaker(AtomicUsize);
	impl CountWaker {
		fn count(&self) -> usize {
			self.0.load(Ordering::SeqCst)
		}
	}
	impl Wake for CountWaker {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
		fn wake_by_ref(self: &Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}
	fn counting() -> (Arc<CountWaker>, Waker) {
		let waker = Arc::new(CountWaker(AtomicUsize::new(0)));
		let w = Waker::from(waker.clone());
		(waker, w)
	}

	#[test]
	fn fifo_order() {
		let deque = Deque::new();
		deque.try_push(1).unwrap();
		deque.try_push(2).unwrap();
		deque.try_push(3).unwrap();

		assert_eq!(deque.try_pop().unwrap(), Some(1));
		assert_eq!(deque.try_pop().unwrap(), Some(2));
		assert_eq!(deque.try_pop().unwrap(), Some(3));
		assert_eq!(deque.try_pop().unwrap(), None, "empty but open");
	}

	#[test]
	fn bounded_rejects_when_full_and_returns_the_item() {
		let deque = Deque::bounded(1);
		deque.try_push(1).unwrap();

		match deque.try_push(2) {
			Err(PushError::Full(item)) => assert_eq!(item, 2),
			other => panic!("expected Full, got {other:?}"),
		}

		// Popping frees the slot.
		assert_eq!(deque.try_pop().unwrap(), Some(1));
		deque.try_push(2).unwrap();
	}

	#[test]
	fn closed_rejects_pushes_and_drains_pops() {
		let deque = Deque::new();
		deque.try_push(1).unwrap();
		deque.close();
		deque.close(); // idempotent

		match deque.try_push(2) {
			Err(PushError::Closed(item)) => assert_eq!(item, 2),
			other => panic!("expected Closed, got {other:?}"),
		}

		// The queued item drains before closure is reported.
		assert_eq!(deque.try_pop().unwrap(), Some(1));
		assert_eq!(deque.try_pop(), Err(Closed));

		let waiter = Waiter::noop();
		assert_eq!(deque.poll_pop(&waiter), Poll::Ready(Err(Closed)));
		assert_eq!(deque.poll_push_with(&waiter, || 3), Poll::Ready(Err(Closed)));
	}

	#[test]
	fn push_wakes_a_parked_pop() {
		let deque = Deque::new();
		let (waker, w) = counting();
		let mut cx = Context::from_waker(&w);
		let mut cell = WaiterCell::new();

		assert!(deque.poll_pop(cell.register(&mut cx)).is_pending());
		deque.try_push(7).unwrap();
		assert!(waker.count() >= 1, "push should wake the parked pop");
		assert_eq!(deque.poll_pop(cell.register(&mut cx)), Poll::Ready(Ok(7)));
	}

	#[test]
	fn pop_wakes_a_parked_push_and_defers_the_item() {
		let deque = Deque::bounded(1);
		deque.try_push(1).unwrap();

		let (waker, w) = counting();
		let mut cx = Context::from_waker(&w);
		let mut cell = WaiterCell::new();

		// Full: the closure must not run, and the poll parks.
		let made = std::cell::Cell::new(false);
		assert!(
			deque
				.poll_push_with(cell.register(&mut cx), || {
					made.set(true);
					2
				})
				.is_pending()
		);
		assert!(!made.get(), "the item must not be built while full");

		// A pop frees the slot and wakes the parked push.
		assert_eq!(deque.try_pop().unwrap(), Some(1));
		assert!(waker.count() >= 1, "pop should wake the parked push");
		assert_eq!(deque.poll_push_with(cell.register(&mut cx), || 2), Poll::Ready(Ok(())));
		assert_eq!(deque.try_pop().unwrap(), Some(2));
	}

	#[test]
	fn close_wakes_both_sides() {
		let deque = Deque::<u32>::bounded(1);
		deque.try_push(1).unwrap();

		let (pop_waker, w1) = counting();
		let mut cx1 = Context::from_waker(&w1);
		let mut pop_cell = WaiterCell::new();
		// Park a pop on a second handle (the queued item is popped first).
		let popper = deque.clone();
		assert_eq!(popper.poll_pop(pop_cell.register(&mut cx1)), Poll::Ready(Ok(1)));
		assert!(popper.poll_pop(pop_cell.register(&mut cx1)).is_pending());

		// Refill so a push parks too.
		deque.try_push(2).unwrap();
		let (push_waker, w2) = counting();
		let mut cx2 = Context::from_waker(&w2);
		let mut push_cell = WaiterCell::new();
		assert!(deque.poll_push_with(push_cell.register(&mut cx2), || 3).is_pending());

		deque.close();
		assert!(pop_waker.count() >= 1, "close should wake the parked pop");
		assert!(push_waker.count() >= 1, "close should wake the parked push");

		// The parked pop drains the remaining item; the push observes closure.
		assert_eq!(popper.poll_pop(pop_cell.register(&mut cx1)), Poll::Ready(Ok(2)));
		assert_eq!(popper.poll_pop(pop_cell.register(&mut cx1)), Poll::Ready(Err(Closed)));
		assert_eq!(
			deque.poll_push_with(push_cell.register(&mut cx2), || 3),
			Poll::Ready(Err(Closed))
		);
	}

	#[test]
	fn accessors() {
		let deque = Deque::bounded(2);
		assert_eq!(deque.capacity(), Some(2));
		assert!(deque.is_empty());
		deque.try_push(1).unwrap();
		assert_eq!(deque.len(), 1);
		assert!(!deque.is_empty());
		assert!(!deque.is_closed());

		let clone = deque.clone();
		let other = Deque::<u32>::new();
		assert!(deque.same_channel(&clone));
		assert!(!deque.same_channel(&other));
		assert_eq!(other.capacity(), None);
	}

	#[test]
	#[should_panic(expected = "zero-capacity")]
	fn zero_capacity_panics() {
		let _ = Deque::<u32>::bounded(0);
	}

	#[tokio::test]
	async fn async_push_parks_until_popped() {
		let deque = Deque::bounded(1);
		deque.try_push(1u32).unwrap();

		let pusher = deque.clone();
		let task = tokio::spawn(async move { pusher.push(2).await });

		// Let the push park on the full queue before making room.
		tokio::task::yield_now().await;
		assert_eq!(deque.pop().await, Ok(1));

		task.await.unwrap().unwrap();
		assert_eq!(deque.pop().await, Ok(2));
	}

	#[tokio::test]
	async fn async_pop_parks_until_pushed() {
		let deque = Deque::new();
		let popper = deque.clone();
		let task = tokio::spawn(async move { popper.pop().await });

		tokio::task::yield_now().await;
		deque.try_push(9u32).unwrap();

		assert_eq!(task.await.unwrap(), Ok(9));
	}

	#[tokio::test]
	async fn async_ops_observe_close() {
		let deque = Deque::<u32>::bounded(1);
		deque.try_push(1).unwrap();

		let pusher = deque.clone();
		let push = tokio::spawn(async move { pusher.push(2).await });
		tokio::task::yield_now().await;
		deque.close();

		assert_eq!(push.await.unwrap(), Err(Closed));
		// Pops still drain the queued item before reporting closure.
		assert_eq!(deque.pop().await, Ok(1));
		assert_eq!(deque.pop().await, Err(Closed));
	}
}

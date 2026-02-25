use std::{
	ops::{Deref, DerefMut},
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::Poll,
};

use web_async::{Lock, LockGuard};

use crate::{
	Error,
	model::waiter::{Waiter, WaiterList, waiter_fn},
};

#[derive(Debug)]
struct State<T> {
	value: T,
	waiters: WaiterList,
	closed: Result<(), Error>,
}

#[derive(Debug)]
struct Counts {
	producers: AtomicUsize,
	consumers: AtomicUsize,
}

impl<T: Default> Default for State<T> {
	fn default() -> Self {
		Self::new(Default::default())
	}
}

impl<T> State<T> {
	pub fn new(value: T) -> Self {
		Self {
			value,
			closed: Ok(()),
			waiters: WaiterList::new(),
		}
	}
}

impl Default for Counts {
	fn default() -> Self {
		Self {
			producers: AtomicUsize::new(1),
			consumers: AtomicUsize::new(0),
		}
	}
}

impl<T> Deref for State<T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.value
	}
}

impl<T> DerefMut for State<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.value
	}
}

#[derive(Debug)]
pub struct Producer<T> {
	state: Lock<State<T>>,
	counts: Arc<Counts>,
}

impl<T: Default> Default for Producer<T> {
	fn default() -> Self {
		Self {
			state: Lock::new(State::default()),
			counts: Arc::new(Counts::default()),
		}
	}
}

impl<T> Producer<T> {
	pub fn new(value: T) -> Self {
		Self {
			state: Lock::new(State::new(value)),
			counts: Arc::new(Counts::default()),
		}
	}

	pub fn consume(&self) -> Consumer<T> {
		self.counts.consumers.fetch_add(1, Ordering::Relaxed);

		Consumer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	pub fn close(&mut self, err: Error) -> Result<(), Error> {
		self.modify()?.close(err);
		Ok(())
	}

	pub fn modify(&self) -> Result<ProducerMut<'_, T>, Error> {
		let state = self.state.lock();
		state.closed.clone()?;
		Ok(ProducerMut::new(state))
	}

	pub fn poll_modify<F, R>(&self, waiter: &Waiter, mut f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&mut ProducerMut<'_, T>) -> Poll<R>,
	{
		let mut state = self.modify()?;

		if let Poll::Ready(res) = f(&mut state) {
			return Poll::Ready(Ok(res));
		}

		// Reset modified so the drop doesn't immediately wake the waiter we're about to register.
		state.modified = false;

		// Re-extract state from producer_state to register
		let state = state.state.as_mut().unwrap();
		waiter.register(&mut state.waiters);
		Poll::Pending
	}

	pub async fn unused(&self) -> Result<(), Error> {
		waiter_fn(move |waiter| self.poll_unused(waiter)).await
	}

	fn poll_unused(&self, waiter: &Waiter) -> Poll<Result<(), Error>> {
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Ok(()));
		}

		let mut state = self.state.lock();
		if let Err(err) = state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		waiter.register(&mut state.waiters);

		// Re-check after registration to avoid TOCTOU race where the last
		// consumer drops between the initial check and waiter registration.
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Ok(()));
		}

		Poll::Pending
	}

	pub fn borrow(&self) -> ProducerRef<'_, T> {
		ProducerRef {
			state: self.state.lock(),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	pub fn weak(&self) -> Weak<T> {
		Weak {
			state: self.state.clone(),
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
		// Atomically decrement and check if we were the last producer
		let prev = self.counts.producers.fetch_sub(1, Ordering::AcqRel);
		if prev > 1 {
			return;
		}

		// We were the last producer, need to close
		let waiters = {
			let mut state = self.state.lock();
			if state.closed.is_err() {
				return;
			}

			state.closed = Err(Error::Dropped);
			state.waiters.take()
		};

		waiters.wake();
	}
}

/// A weak reference to a Producer/Consumer state.
///
/// Does not affect ref counts, so it won't prevent auto-close when all Producers are dropped.
/// Can be upgraded to a full Producer or Consumer.
#[derive(Debug)]
pub struct Weak<T> {
	state: Lock<State<T>>,
	counts: Arc<Counts>,
}

impl<T> Weak<T> {
	#[allow(dead_code)]
	pub fn produce(&self) -> Result<Producer<T>, Error> {
		// Increment first to prevent the last Producer::drop from
		// closing the state between our check and the return.
		self.counts.producers.fetch_add(1, Ordering::Relaxed);

		{
			let state = self.state.lock();
			if let Err(err) = state.closed.clone() {
				self.counts.producers.fetch_sub(1, Ordering::Relaxed);
				return Err(err);
			}
		}

		Ok(Producer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		})
	}

	pub fn consume(&self) -> Consumer<T> {
		self.counts.consumers.fetch_add(1, Ordering::Relaxed);

		Consumer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}

	pub fn borrow(&self) -> ProducerRef<'_, T> {
		ProducerRef {
			state: self.state.lock(),
		}
	}

	pub fn is_closed(&self) -> bool {
		self.state.lock().closed.is_err()
	}

	pub async fn unused(&self) -> Result<(), Error> {
		waiter_fn(move |waiter| self.poll_unused(waiter)).await
	}

	fn poll_unused(&self, waiter: &Waiter) -> Poll<Result<(), Error>> {
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Ok(()));
		}

		let mut state = self.state.lock();
		if let Err(err) = state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		waiter.register(&mut state.waiters);

		// Re-check after registration to avoid TOCTOU race where the last
		// consumer drops between the initial check and waiter registration.
		if self.counts.consumers.load(Ordering::Relaxed) == 0 {
			return Poll::Ready(Ok(()));
		}

		Poll::Pending
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
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

#[derive(Debug)]
pub struct ProducerMut<'a, T> {
	// Its an option so we can drop it before notifying consumers.
	state: Option<LockGuard<'a, State<T>>>,
	modified: bool,
}

impl<'a, T> ProducerMut<'a, T> {
	fn new(state: LockGuard<'a, State<T>>) -> Self {
		Self {
			state: Some(state),
			modified: false,
		}
	}

	/// NOTE: This takes self so it's impossible to be in a closed state.
	pub fn close(mut self, err: Error) {
		let state = self.state.as_mut().unwrap();
		// We don't need to check for state.closed because we checked when making ProducerMut
		state.closed = Err(err);
		self.modified = true;
	}
}

impl<'a, T> Deref for ProducerMut<'a, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.state.as_ref().unwrap().value
	}
}

impl<'a, T> DerefMut for ProducerMut<'a, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		// If we use the &mut then notify on Drop.
		self.modified = true;
		&mut self.state.as_mut().unwrap().value
	}
}

impl<T> Drop for ProducerMut<'_, T> {
	fn drop(&mut self) {
		let mut state = self.state.take().unwrap();

		if !self.modified {
			return;
		}

		// Drain wakers while holding lock, then wake after releasing
		let waiters = state.waiters.take();
		drop(state); // Release Mutex BEFORE waking

		waiters.wake();
	}
}

pub struct ProducerRef<'a, T> {
	state: LockGuard<'a, State<T>>,
}

impl<'a, T> ProducerRef<'a, T> {
	pub fn is_closed(&self) -> bool {
		self.state.closed.is_err()
	}
}

impl<'a, T> Deref for ProducerRef<'a, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.state.value
	}
}

#[derive(Debug)]
pub struct Consumer<T> {
	state: Lock<State<T>>,
	counts: Arc<Counts>,
}

impl<T> Consumer<T> {
	pub fn poll<F, R>(&self, waiter: &Waiter, mut f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&ConsumerRef<'_, T>) -> Poll<R>,
	{
		let state = self.state.lock();
		let consumer_state = ConsumerRef { state };

		if let Poll::Ready(res) = f(&consumer_state) {
			return Poll::Ready(Ok(res));
		}

		if let Err(err) = consumer_state.state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		// Re-extract state from consumer_state to register
		let mut state = consumer_state.state;
		waiter.register(&mut state.waiters);

		Poll::Pending
	}

	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<Error> {
		let mut state = self.state.lock();
		if let Err(err) = state.closed.clone() {
			return Poll::Ready(err);
		}

		waiter.register(&mut state.waiters);
		Poll::Pending
	}

	pub async fn closed(&self) -> Error {
		waiter_fn(move |waiter| self.poll_closed(waiter)).await
	}

	/// Upgrade to a Producer, failing if the state is already closed.
	pub fn produce(&self) -> Result<Producer<T>, Error> {
		// Increment first to prevent the last Producer::drop from
		// closing the state between our check and the return.
		self.counts.producers.fetch_add(1, Ordering::Relaxed);

		{
			let state = self.state.lock();
			if let Err(err) = state.closed.clone() {
				self.counts.producers.fetch_sub(1, Ordering::Relaxed);
				return Err(err);
			}
		}

		Ok(Producer {
			state: self.state.clone(),
			counts: self.counts.clone(),
		})
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl<T> Drop for Consumer<T> {
	fn drop(&mut self) {
		// Atomically decrement and check if we were the last consumer
		let prev = self.counts.consumers.fetch_sub(1, Ordering::AcqRel);
		if prev > 1 {
			return;
		}

		// We were the last consumer, need to wake waiters
		let waiters = {
			let mut state = self.state.lock();
			state.waiters.take()
		};

		waiters.wake();
	}
}

impl<T> Clone for Consumer<T> {
	fn clone(&self) -> Self {
		self.counts.consumers.fetch_add(1, Ordering::Relaxed);

		Self {
			state: self.state.clone(),
			counts: self.counts.clone(),
		}
	}
}

#[derive(Debug)]
pub struct ConsumerRef<'a, T> {
	state: LockGuard<'a, State<T>>,
}

impl<'a, T> Deref for ConsumerRef<'a, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.state.value
	}
}

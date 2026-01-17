use std::{
	ops::{Deref, DerefMut},
	sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
	task::Poll,
};

use crate::{
	model::waiter::{waiter_fn, Waiter, WaiterList},
	Error,
};

#[derive(Debug)]
struct State<T> {
	value: T,
	waiters: WaiterList,
	closed: Result<(), Error>,
	producers: usize,
	consumers: usize,
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
			producers: 1,
			consumers: 0,
			waiters: WaiterList::new(),
		}
	}

	pub fn closed(&self) -> Result<(), Error> {
		self.closed.clone()
	}

	pub fn is_closed(&self) -> bool {
		self.closed.is_err()
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

#[derive(Default, Debug)]
pub struct Producer<T> {
	state: Arc<RwLock<State<T>>>,
}

impl<T> Producer<T> {
	pub fn new(value: T) -> Self {
		Self {
			state: Arc::new(RwLock::new(State::new(value))),
		}
	}

	pub fn consume(&self) -> Consumer<T> {
		let mut state = self.state.write().unwrap();
		state.consumers += 1; // TODO atomic instead?

		Consumer {
			state: self.state.clone(),
		}
	}

	pub fn close(self, err: Error) -> Result<(), Error> {
		let mut state = self.modify()?;
		state.closed = Err(err);
		state.waiters.notify();

		Ok(())
	}

	pub fn modify(&self) -> Result<ProducerState<'_, T>, Error> {
		let state = self.state.write().unwrap();
		state.closed.clone()?;
		Ok(ProducerState {
			state: Some(state),
			modified: false,
		})
	}

	/*
	pub fn poll_modify<F, R>(&self, waiter: &Waiter<'_>, mut f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&mut ProducerState<'_, T>) -> Poll<R>,
	{
		let mut state = self.modify()?;
		if let Poll::Ready(res) = f(&mut state) {
			return Poll::Ready(Ok(res));
		}

		if let Err(err) = state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		waiter.register(&state.waiters);

		Poll::Pending
	}
	*/

	pub fn poll_closed(&self, waiter: &Waiter<'_>) -> Poll<Error> {
		let state = self.borrow();
		if let Err(err) = state.closed.clone() {
			return Poll::Ready(err);
		}

		waiter.register(&state.waiters);
		Poll::Pending
	}

	pub async fn closed(&self) -> Error {
		waiter_fn(move |waiter| self.poll_closed(waiter)).await
	}

	pub fn poll_unused(&self, waiter: &Waiter<'_>) -> Poll<Result<(), Error>> {
		let state = self.borrow();
		if state.consumers == 0 {
			return Poll::Ready(Ok(()));
		}

		if let Err(err) = state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		waiter.register(&state.waiters);
		Poll::Pending
	}

	pub async fn unused(&self) -> Result<(), Error> {
		waiter_fn(move |waiter| self.poll_unused(waiter)).await
	}

	pub fn borrow(&self) -> ConsumerState<'_, T> {
		ConsumerState {
			state: self.state.read().unwrap(),
		}
	}
}

#[derive(Debug)]
pub struct ProducerState<'a, T> {
	// Its an option so we can drop it before notifying consumers.
	state: Option<RwLockWriteGuard<'a, State<T>>>,
	modified: bool,
}

impl<'a, T> Deref for ProducerState<'a, T> {
	type Target = State<T>;

	fn deref(&self) -> &Self::Target {
		self.state.as_ref().unwrap()
	}
}

impl<'a, T> DerefMut for ProducerState<'a, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		// If we use the &mut then notify on Drop.
		self.modified = true;
		self.state.as_mut().unwrap()
	}
}

impl<T> Drop for ProducerState<'_, T> {
	fn drop(&mut self) {
		let state = self.state.take().unwrap();

		// Notify that the state was changed after this guard is dropped.
		if self.modified {
			state.waiters.notify();
		}
	}
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		self.state.write().unwrap().producers += 1;

		Self {
			state: self.state.clone(),
		}
	}
}

impl<T> Drop for Producer<T> {
	fn drop(&mut self) {
		let mut state = self.state.write().unwrap();
		state.producers -= 1;

		if state.producers > 0 || state.closed.is_ok() {
			return;
		}

		state.closed = Err(Error::Dropped);
		state.waiters.notify();
	}
}

#[derive(Debug)]
pub struct Consumer<T> {
	state: Arc<RwLock<State<T>>>,
}

impl<T> Consumer<T> {
	pub fn borrow(&self) -> ConsumerState<'_, T> {
		ConsumerState {
			state: self.state.read().unwrap(),
		}
	}

	pub fn poll<F, R>(&self, waiter: &Waiter<'_>, mut f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&ConsumerState<'_, T>) -> Poll<R>,
	{
		let state = self.borrow();
		if let Poll::Ready(res) = f(&state) {
			return Poll::Ready(Ok(res));
		}

		if let Err(err) = state.closed.clone() {
			return Poll::Ready(Err(err));
		}

		waiter.register(&state.waiters);

		Poll::Pending
	}

	pub fn poll_closed(&self, waiter: &Waiter<'_>) -> Poll<Error> {
		let state = self.borrow();
		if let Err(err) = state.closed.clone() {
			return Poll::Ready(err);
		}

		waiter.register(&state.waiters);
		Poll::Pending
	}

	pub async fn closed(&self) -> Error {
		waiter_fn(move |waiter| self.poll_closed(waiter)).await
	}
}

impl<T> Drop for Consumer<T> {
	fn drop(&mut self) {
		let mut state = self.state.write().unwrap();

		state.consumers -= 1;
		if state.consumers == 0 {
			state.waiters.notify();
		}
	}
}

impl<T> Clone for Consumer<T> {
	fn clone(&self) -> Self {
		self.state.write().unwrap().consumers += 1;
		Self {
			state: self.state.clone(),
		}
	}
}

#[derive(Debug)]
pub struct ConsumerState<'a, T> {
	state: RwLockReadGuard<'a, State<T>>,
}

impl<'a, T> Deref for ConsumerState<'a, T> {
	type Target = State<T>;

	fn deref(&self) -> &Self::Target {
		&self.state
	}
}

pub struct ProducerConsumer<T> {
	pub producer: Producer<T>,
	pub consumer: Consumer<T>,
}

impl<T> ProducerConsumer<T> {
	pub fn new(value: T) -> Self {
		let producer = Producer::new(value);
		let consumer = producer.consume();
		Self { producer, consumer }
	}
}

impl<T: Default> Default for ProducerConsumer<T> {
	fn default() -> Self {
		Self::new(Default::default())
	}
}

impl<T> Clone for ProducerConsumer<T> {
	fn clone(&self) -> Self {
		Self {
			producer: self.producer.clone(),
			consumer: self.consumer.clone(),
		}
	}
}

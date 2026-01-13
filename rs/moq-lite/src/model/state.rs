use std::{
	fmt,
	future::Future,
	ops::Deref,
	sync::{
		atomic::{self, AtomicUsize},
		Arc,
	},
};

use tokio::sync::watch;

use crate::Error;

#[derive(Default)]
struct State<T> {
	value: T,
	closed: Option<Result<(), Error>>,
}

pub struct Producer<T> {
	state: watch::Sender<State<T>>,
	active: Arc<AtomicUsize>,
}

impl<T> Producer<T> {
	pub fn new(value: T) -> Self {
		Self {
			state: watch::Sender::new(State { value, closed: None }),
			active: Arc::new(AtomicUsize::new(1)),
		}
	}

	pub fn consume(&self) -> Consumer<T> {
		Consumer::new(self.state.subscribe())
	}

	pub fn close(&mut self) -> Result<(), Error> {
		let mut res = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(Err(err)) = state.closed.clone() {
				res = Err(err);
				return false;
			}

			state.closed = Some(Ok(()));
			true
		});

		res
	}

	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut res = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(Err(closed)) = state.closed.clone() {
				res = Err(closed);
				return false;
			}

			state.closed = Some(Err(err));
			true
		});

		res
	}

	pub fn modify<F, R>(&self, modify: F) -> Result<R, Error>
	where
		F: FnOnce(&mut T) -> R,
	{
		// Will be overwritten.
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			if let Some(Err(err)) = state.closed.clone() {
				result = Err(err);
				false
			} else {
				result = Ok(modify(&mut state.value));
				true
			}
		});

		result
	}

	pub fn borrow(&self) -> Ref<'_, T> {
		Ref {
			inner: self.state.borrow(),
		}
	}

	/// Block until there are no more consumers
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.state.clone();
		async move { state.closed().await }
	}

	pub fn weak(&self) -> ProducerWeak<T> {
		ProducerWeak {
			state: self.state.clone(),
			active: self.active.clone(),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl<T: Default> Default for Producer<T> {
	fn default() -> Self {
		Self::new(Default::default())
	}
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		self.active.fetch_add(1, atomic::Ordering::Relaxed);
		Self {
			state: self.state.clone(),
			active: self.active.clone(),
		}
	}
}

impl<T> Drop for Producer<T> {
	fn drop(&mut self) {
		let active = self.active.fetch_sub(1, atomic::Ordering::Release);
		if active != 1 {
			return;
		}

		atomic::fence(atomic::Ordering::Acquire);

		self.state.send_if_modified(|state| {
			if state.closed.is_some() {
				return false;
			}

			state.closed = Some(Err(Error::Dropped));
			true
		});
	}
}

impl<T: fmt::Debug> fmt::Debug for Producer<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let state = self.state.borrow();
		f.debug_struct("Producer")
			.field("state", &state.value)
			.field("closed", &state.closed)
			.finish()
	}
}

pub struct ProducerWeak<T> {
	state: watch::Sender<State<T>>,
	active: Arc<AtomicUsize>,
}

impl<T> ProducerWeak<T> {
	pub fn upgrade(&self) -> Result<Producer<T>, Error> {
		if let Some(Err(err)) = self.state.borrow().closed.clone() {
			return Err(err);
		}

		// Minor race; we could have been closed between the check and the fetch_add.
		// It doesn't matter though, Producer needs to be responsible for other handles closing at random times.
		self.active.fetch_add(1, atomic::Ordering::Relaxed);

		let producer = Producer {
			state: self.state.clone(),
			active: self.active.clone(),
		};

		Ok(producer)
	}

	/*
	pub fn closed(&self) -> impl Future<Output = Result<(), Error>> {
		let mut state = self.state.subscribe();

		async move {
			match state.wait_for(|state| state.closed.is_some()).await {
				Ok(state) => state.closed.clone().unwrap(),
				Err(_) => Err(Error::Cancel),
			}
		}
	}
	*/
}

impl<T: fmt::Debug> fmt::Debug for ProducerWeak<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let state = self.state.borrow();
		f.debug_struct("ProducerWeak")
			.field("state", &state.value)
			.field("closed", &state.closed)
			.finish()
	}
}

impl<T> Clone for ProducerWeak<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			active: self.active.clone(),
		}
	}
}

pub struct Consumer<T> {
	inner: watch::Receiver<State<T>>,
}

impl<T> Consumer<T> {
	fn new(inner: watch::Receiver<State<T>>) -> Self {
		Self { inner }
	}

	pub fn closed(&self) -> impl Future<Output = Result<(), Error>> {
		// TODO Make a more efficient closed() that doesn't clone the inner.
		let mut inner = self.inner.clone();
		async move {
			match inner.wait_for(|state| state.closed.is_some()).await {
				Ok(state) => state.closed.clone().unwrap(),
				Err(_) => unreachable!(),
			}
		}
	}

	// TODO Make a non-mut wait_for
	// Returns when the function returns true or we're closed.
	pub async fn wait_for(&mut self, mut f: impl FnMut(&T) -> bool) -> Result<Ref<'_, T>, Error> {
		let mut matched = false;

		let state = self
			.inner
			.wait_for(|state| {
				// We always want to check the function first, only returning closed if false.
				matched = f(&state.value);
				matched || state.closed.is_some()
			})
			.await
			.expect("not closed properly");

		if !matched {
			if let Some(Err(err)) = state.closed.clone() {
				return Err(err);
			}
		}

		Ok(Ref { inner: state })
	}

	pub fn borrow(&self) -> Ref<'_, T> {
		Ref {
			inner: self.inner.borrow(),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.inner.same_channel(&other.inner)
	}
}

impl<T: fmt::Debug> fmt::Debug for Consumer<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let inner = self.inner.borrow();
		f.debug_struct("Consumer")
			.field("state", &inner.value)
			.field("closed", &inner.closed)
			.finish()
	}
}

impl<T> Clone for Consumer<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

pub struct Ref<'a, T> {
	inner: tokio::sync::watch::Ref<'a, State<T>>,
}

impl<'a, T> Deref for Ref<'a, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.inner.value
	}
}

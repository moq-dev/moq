use std::{
	collections::HashMap,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	sync::{
		atomic::{self, AtomicU64},
		Arc, Mutex,
	},
	task::{Context, Poll, Waker},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct WaiterState {
	id: u64,
	registered: Mutex<Vec<WaiterList>>,
}

impl WaiterState {
	pub fn new() -> Self {
		Self {
			id: NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed),
			registered: Mutex::new(Vec::new()),
		}
	}

	fn clear(&mut self) {
		for list in self.registered.lock().unwrap().drain(..) {
			list.unregister(self.id);
		}
	}

	fn register(&self, list: &WaiterList, waker: Waker) {
		self.registered.lock().unwrap().push(list.clone());
		list.register(self.id, waker);
	}
}

impl Drop for WaiterState {
	fn drop(&mut self) {
		self.clear();
	}
}

/// Handle passed to poll functions
pub struct Waiter<'a> {
	state: Option<&'a mut WaiterState>,
	waker: &'a Waker,
}

impl<'a> Waiter<'a> {
	pub fn register(&self, list: &WaiterList) {
		if let Some(state) = self.state.as_ref() {
			state.register(list, self.waker.clone());
		}
	}

	pub fn noop() -> Self {
		Self {
			state: None,
			waker: Waker::noop(),
		}
	}
}

#[derive(Clone, Debug)]
pub struct WaiterList {
	wakers: Arc<Mutex<HashMap<u64, Waker>>>,
}

impl WaiterList {
	pub fn new() -> Self {
		Self {
			wakers: Arc::new(Mutex::new(HashMap::new())),
		}
	}

	fn register(&self, id: u64, waker: Waker) {
		self.wakers.lock().unwrap().insert(id, waker);
	}

	fn unregister(&self, id: u64) {
		self.wakers.lock().unwrap().remove(&id);
	}

	pub fn notify(&self) {
		for (_, waker) in self.wakers.lock().unwrap().drain() {
			// TODO Clear all registered waiters in other lists.
			waker.wake();
		}
	}
}

pub struct WaiterFn<F, R> {
	poll: F,
	state: WaiterState,
	_marker: PhantomData<R>,
}

pub fn waiter_fn<F, R>(poll: F) -> WaiterFn<F, R>
where
	F: FnMut(&Waiter<'_>) -> Poll<R>,
{
	WaiterFn::<F, R> {
		poll,
		state: WaiterState::new(),
		_marker: PhantomData,
	}
}

impl<F, R> Future for WaiterFn<F, R>
where
	F: FnMut(&Waiter<'_>) -> Poll<R> + Unpin,
	R: Unpin,
{
	type Output = R;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R> {
		let this = &mut *self;

		// Create handle for this poll
		let waiter = Waiter {
			state: Some(&mut this.state),
			waker: cx.waker(),
		};

		let res = (this.poll)(&waiter);
		if res.is_ready() {
			// Clear all registered waiters if we already have a result.
			this.state.clear();
		}

		res
	}
}

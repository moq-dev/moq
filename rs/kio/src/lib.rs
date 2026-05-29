//! Producer/consumer shared state with async waker-based notification.
//!
//! This crate provides [`Producer`] and [`Consumer`] types that share state through
//! a mutex-protected value. Producers can modify the state and consumers are
//! automatically notified via async wakers. The channel auto-closes when all
//! producers are dropped.

use std::{
	ops::{Deref, DerefMut},
	sync::atomic::AtomicUsize,
};

mod lock;
mod waiter;

mod consumer;
mod producer;
mod weak;

pub use consumer::Consumer;
pub use producer::{Mut, Producer, Ref};
pub use waiter::{Waiter, WaiterList, wait};
pub use weak::Weak;

#[derive(Debug)]
pub(crate) struct State<T> {
	pub value: T,
	pub waiters: waiter::WaiterList,
	pub closed: bool,
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
			closed: false,
			waiters: waiter::WaiterList::new(),
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
pub(crate) struct Counts {
	pub producers: AtomicUsize,
	pub consumers: AtomicUsize,
}

impl Default for Counts {
	fn default() -> Self {
		Self {
			producers: AtomicUsize::new(1),
			consumers: AtomicUsize::new(0),
		}
	}
}

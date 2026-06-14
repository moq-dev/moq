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
mod future;
mod producer;
mod weak;

#[cfg(test)]
mod tests;

pub use consumer::Consumer;
pub use future::{Future, Pending};
pub use producer::{Mut, Producer, Ref};
pub use waiter::{Waiter, WaiterList, wait};
pub use weak::Weak;

/// Waiters split by what they're waiting on, so an event only wakes the
/// waiters that care about it. The big win is per-modification writes (the hot
/// path) waking only `value`, leaving the long-lived `closed` and `consumer`
/// waiters untouched.
#[derive(Debug)]
pub(crate) struct State<T> {
	pub value: T,
	/// Value changes (`poll`/`wait`). Woken on every modification.
	pub waiters_value: waiter::WaiterList,
	/// Closure (`closed`). Woken only when the channel closes.
	pub waiters_closed: waiter::WaiterList,
	/// Consumer-count changes (`used`/`unused`). `used`/`unused` are used
	/// sequentially in practice, so they share one list.
	pub waiters_consumer: waiter::WaiterList,
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
			waiters_value: waiter::WaiterList::new(),
			waiters_closed: waiter::WaiterList::new(),
			waiters_consumer: waiter::WaiterList::new(),
		}
	}

	/// Drain every waiter list. Used on close, which all waiters react to.
	/// Caller wakes the returned lists after releasing the lock.
	pub fn take_close_waiters(&mut self) -> [waiter::WaiterList; 3] {
		[
			self.waiters_value.take(),
			self.waiters_closed.take(),
			self.waiters_consumer.take(),
		]
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

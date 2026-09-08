//! Producer/consumer shared state with async waker-based notification.
//!
//! This crate provides [`Producer`] and [`Consumer`] types that share state through
//! a mutex-protected value. Producers can modify the state and consumers are
//! automatically notified via async wakers. The channel auto-closes when all
//! producers are dropped.
//!
//! For state that both sides legitimately mutate (e.g. a reverse request queue),
//! [`Shared`] is a role-less sibling: every handle can lock, read, or park on a
//! predicate, with no liveness of its own.
//!
//! [`Queue`] is a poll-native FIFO queue built in the same style: role-less
//! clone-able handles, bounded or unbounded, with separate wake lists for the
//! push and pop sides.
//!
//! [`Fan`] hands out a [`Waker`](std::task::Waker) that wakes a whole [`WaiterList`], for
//! driving a foreign future that keeps a single waker on behalf of everyone parked on it.
//! It either owns the list or, via [`Fan::project`], wakes one already inside a [`Lock`].

use std::{
	fmt,
	ops::{Deref, DerefMut},
};

use crate::sync::AtomicUsize;

mod lock;
mod sync;
mod waiter;

mod consumer;
mod pollable;
mod producer;
mod queue;
mod send;
mod shared;
mod weak;

#[cfg(feature = "time")]
pub mod time;

#[cfg(feature = "tokio")]
#[doc(hidden)]
pub mod tokio;

#[cfg(all(test, loom))]
mod loom;
#[cfg(all(test, not(loom)))]
mod tests;

pub use consumer::Consumer;
pub use lock::{Lock, LockGuard, WeakLock};
pub use pollable::{Pending, Pollable};
pub use producer::{Mut, Producer, Ref};
pub use queue::{PushError, Queue};
pub use send::MaybeSend;
pub use shared::Shared;
pub use waiter::{Fan, Hold, Park, Waiter, WaiterList, wait};
pub use weak::{ConsumerWeak, ProducerWeak, Weak};

/// Bytes an `Arc<T>` allocation occupies: `T` behind two reference counts, padded to
/// `T`'s alignment and rounded up to the whole layout's.
///
/// The `const` equivalent of `Layout::extend` followed by `pad_to_align`, neither of
/// which is `const`. `arc_heap_matches_layout` holds it to those.
const fn arc_heap<T>() -> usize {
	let counts = 2 * size_of::<usize>();
	let align = align_of::<T>();
	let layout = if align > align_of::<usize>() {
		align
	} else {
		align_of::<usize>()
	};
	(counts.next_multiple_of(align) + size_of::<T>()).next_multiple_of(layout)
}

#[cfg(test)]
mod heap {
	use std::alloc::Layout;

	use super::*;

	/// [`arc_heap`] hand-rolls what `Layout` does, because `Layout` isn't `const`. Hold
	/// it to the real thing, including a `T` aligned past the reference counts.
	#[test]
	fn arc_heap_matches_layout() {
		#[repr(align(64))]
		struct Overaligned;

		fn layout<T>() -> usize {
			Layout::new::<[usize; 2]>()
				.extend(Layout::new::<T>())
				.unwrap()
				.0
				.pad_to_align()
				.size()
		}

		assert_eq!(arc_heap::<u8>(), layout::<u8>());
		assert_eq!(arc_heap::<Overaligned>(), layout::<Overaligned>());
		assert_eq!(arc_heap::<sync::Mutex<State<()>>>(), layout::<sync::Mutex<State<()>>>());
	}
}

/// The channel closed before the awaited condition held.
///
/// The `async` methods report closure with this instead of handing back a [`Ref`],
/// because a guard bound from an `Err` and held across a later `.await` would keep
/// kio's mutex locked and stall every other handle. The synchronous `poll_*`/`write`
/// methods still return a [`Ref`]: there is no await to deadlock across, and the
/// caller already holds the lock. If you need the final state after an `async`
/// method returns `Closed`, call `read()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Closed;

impl fmt::Display for Closed {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "channel closed")
	}
}

impl std::error::Error for Closed {}

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

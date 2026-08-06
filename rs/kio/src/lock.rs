use std::{
	fmt,
	ops::{Deref, DerefMut},
	// std, not `crate::sync`: [`WeakLock`] needs `Arc::downgrade`, which loom's Arc
	// doesn't have. Loom still models the `Mutex` inside, which is the part that
	// orders every handle against every other.
	sync::{Arc, Weak},
};

use crate::sync::{Mutex, MutexGuard};

/// A cloneable mutex wrapper backed by `Arc<Mutex<T>>`.
///
/// Every kio channel keeps its state in one of these, with its [`WaiterList`]s inside,
/// so a state change and the wake it owes are decided under a single lock. [`Fan`] can
/// hand out a [`Waker`](std::task::Waker) for one of those lists. See
/// [`Fan::project`](crate::Fan::project).
///
/// Poisoning is not part of the contract: a panic while the lock is held poisons it, and
/// every method here panics in turn rather than handing back a `Result`.
///
/// [`WaiterList`]: crate::WaiterList
/// [`Fan`]: crate::Fan
pub struct Lock<T> {
	inner: Arc<Mutex<T>>,
}

impl<T> Lock<T> {
	/// Wrap a value.
	pub fn new(value: T) -> Self {
		Self {
			inner: Arc::new(Mutex::new(value)),
		}
	}

	/// Lock the state, blocking until it is free. Panics if the lock was poisoned.
	pub fn lock(&self) -> LockGuard<'_, T> {
		LockGuard {
			inner: self.inner.lock().expect("mutex poisoned"),
		}
	}

	/// Whether both handles point at the same state.
	pub fn is_clone(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// Create a handle that doesn't own the value, so it can be stored inside the
	/// value itself without leaking the allocation.
	pub fn downgrade(&self) -> WeakLock<T> {
		WeakLock {
			inner: Arc::downgrade(&self.inner),
		}
	}
}

/// A [`Lock`] that doesn't own its value, backed by `Weak<Mutex<T>>`.
pub struct WeakLock<T> {
	inner: Weak<Mutex<T>>,
}

impl<T> WeakLock<T> {
	/// A handle that never upgrades, for a placeholder before a value exists.
	pub fn new() -> Self {
		Self { inner: Weak::new() }
	}

	/// Recover the owning [`Lock`], or `None` once the value has been dropped.
	pub fn upgrade(&self) -> Option<Lock<T>> {
		Some(Lock {
			inner: self.inner.upgrade()?,
		})
	}
}

impl<T> Clone for WeakLock<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T> fmt::Debug for WeakLock<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("WeakLock").finish()
	}
}

impl<T> Clone for Lock<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T: fmt::Debug> fmt::Debug for Lock<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.inner.try_lock() {
			Ok(guard) => f.debug_tuple("Lock").field(&*guard).finish(),
			Err(_) => f.debug_tuple("Lock").field(&"<locked>").finish(),
		}
	}
}

/// A guard providing access to the locked value. Releases the lock on drop.
pub struct LockGuard<'a, T> {
	inner: MutexGuard<'a, T>,
}

impl<T: fmt::Debug> fmt::Debug for LockGuard<'_, T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("LockGuard").field(&*self.inner).finish()
	}
}

impl<T> Deref for LockGuard<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T> DerefMut for LockGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<T> Default for WeakLock<T> {
	fn default() -> Self {
		Self::new()
	}
}

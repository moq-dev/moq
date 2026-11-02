use std::{
	fmt,
	ops::{Deref, DerefMut},
	sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// A cloneable mutex wrapper backed by `Arc<Mutex<T>>`.
///
/// [`Queue`](crate::Queue) keeps its state in one of these, with its [`WaiterList`]s
/// inside, so a state change and the wake it owes are decided under a single lock.
///
/// Poisoning is ignored: a panic while the lock is held must not cascade into every other
/// handle sharing this state, or into a double panic (and an abort) from a `Drop` that locks it.
///
/// State that cannot cross threads keeps the lock on its thread too.
///
/// ```compile_fail,E0277
/// fn require_send(_: impl Send) {}
/// require_send(kio::Lock::new(std::rc::Rc::new(())));
/// ```
///
/// ```compile_fail,E0277
/// fn require_sync(_: impl Sync) {}
/// require_sync(kio::Lock::new(std::rc::Rc::new(())));
/// ```
///
/// [`WaiterList`]: crate::WaiterList
pub struct Lock<T> {
	inner: Arc<Mutex<T>>,
}

// SAFETY: Mutex<T> is Send + Sync when T: Send, so this is
// exactly Arc<Mutex<T>>'s derived bound. Stopping here keeps recursive channel
// graphs from expanding the same Arc/Mutex proof at every level.
unsafe impl<T: Send> Send for Lock<T> {}

// SAFETY: Arc<Mutex<T>> has the same T: Send bound for Sync.
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
	/// Wrap a value.
	pub fn new(value: T) -> Self {
		Self {
			inner: Arc::new(Mutex::new(value)),
		}
	}

	/// Lock the state, blocking until it is free.
	pub fn lock(&self) -> LockGuard<'_, T> {
		LockGuard {
			inner: self.inner.lock().unwrap_or_else(PoisonError::into_inner),
		}
	}

	/// Whether both handles point at the same state.
	pub fn is_clone(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}
}

impl<T> Clone for Lock<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T: Default> Default for Lock<T> {
	fn default() -> Self {
		Self::new(T::default())
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn send_state_does_not_have_to_be_sync() {
		fn require_send_sync(_: impl Send + Sync) {}
		require_send_sync(Lock::new(std::cell::Cell::new(1)));
	}

	#[test]
	fn a_panic_under_the_lock_leaves_it_usable() {
		let lock = Lock::new(1);
		let held = lock.clone();
		let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let mut guard = held.lock();
			*guard = 2;
			panic!("panic while holding the lock");
		}));
		assert!(panicked.is_err());
		assert_eq!(*lock.lock(), 2, "every other handle keeps the state");
	}
}

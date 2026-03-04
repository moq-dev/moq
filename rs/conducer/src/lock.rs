use std::{
	fmt,
	ops::{Deref, DerefMut},
	sync::{Arc, Mutex, MutexGuard},
};

/// A cloneable mutex wrapper backed by `Arc<Mutex<T>>`.
pub struct Lock<T> {
	inner: Arc<Mutex<T>>,
}

impl<T> Lock<T> {
	pub fn new(value: T) -> Self {
		Self {
			inner: Arc::new(Mutex::new(value)),
		}
	}

	pub fn lock(&self) -> LockGuard<'_, T> {
		LockGuard {
			inner: self.inner.lock().expect("mutex poisoned"),
		}
	}

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

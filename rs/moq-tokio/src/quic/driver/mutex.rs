pub(crate) use std::sync::MutexGuard;

/// A connection mutex that fails immediately if a holder poisoned it.
#[derive(Debug)]
pub(crate) struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
	pub(crate) fn new(value: T) -> Self {
		Self(std::sync::Mutex::new(value))
	}

	pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
		self.0.lock().unwrap()
	}

	/// The lock for a drop's cleanup, or `None` once a holder panicked.
	///
	/// A drop can run while that panic's victims unwind, where a second panic aborts the process,
	/// and a poisoned connection has no driver left to act on the cleanup anyway.
	pub(crate) fn lock_for_drop(&self) -> Option<MutexGuard<'_, T>> {
		self.0.lock().ok()
	}

	/// The lock even after a holder panicked, as `Err` with the guard it left behind.
	///
	/// Only for failing the connection's waiters, and reading the error that records why.
	pub(crate) fn lock_recovered(&self) -> Result<MutexGuard<'_, T>, MutexGuard<'_, T>> {
		self.0.lock().map_err(std::sync::PoisonError::into_inner)
	}
}

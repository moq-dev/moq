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

	/// The guard a panicked holder left behind, or `None` while the value is intact.
	///
	/// Only for waking waiters, so they reach the poisoned [`lock`](Self::lock) instead of hanging.
	pub(crate) fn poisoned(&self) -> Option<MutexGuard<'_, T>> {
		self.0.lock().err().map(std::sync::PoisonError::into_inner)
	}
}

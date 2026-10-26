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
}

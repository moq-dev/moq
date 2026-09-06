//! Ties a spawned task's lifetime to a value the owner holds.

/// Owns a spawned task, cancelling it when this guard drops.
///
/// A bare [`tokio::task::JoinHandle`] detaches instead, leaving the task running
/// with nothing left able to reach it. Derefs to the handle, so an owner can still
/// await the task's output or abort it early.
#[derive(Debug)]
pub(crate) struct AbortOnDrop<T = ()>(tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
	/// Take ownership of a spawned task.
	pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
		Self(handle)
	}
}

impl<T> std::ops::Deref for AbortOnDrop<T> {
	type Target = tokio::task::JoinHandle<T>;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl<T> std::ops::DerefMut for AbortOnDrop<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

impl<T> Drop for AbortOnDrop<T> {
	fn drop(&mut self) {
		self.0.abort();
	}
}

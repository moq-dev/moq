//! Ties a spawned task's lifetime to a value the owner holds.

/// Owns a spawned task, cancelling it when this guard drops.
///
/// A bare [`tokio::task::JoinHandle`] detaches instead, leaving the task running
/// with nothing left able to reach it. Derefs to the handle, so an owner can still
/// await the task's output or abort it early.
#[derive(Debug)]
#[cfg(any(feature = "quic", all(feature = "watch", feature = "_certs")))]
pub(crate) struct AbortOnDrop<T = ()>(tokio::task::JoinHandle<T>);

#[cfg(any(feature = "quic", all(feature = "watch", feature = "_certs")))]
impl<T> AbortOnDrop<T> {
	/// Take ownership of a spawned task.
	pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
		Self(handle)
	}
}

#[cfg(any(feature = "quic", all(feature = "watch", feature = "_certs")))]
impl<T> std::ops::Deref for AbortOnDrop<T> {
	type Target = tokio::task::JoinHandle<T>;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

#[cfg(any(feature = "quic", all(feature = "watch", feature = "_certs")))]
impl<T> std::ops::DerefMut for AbortOnDrop<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

#[cfg(any(feature = "quic", all(feature = "watch", feature = "_certs")))]
impl<T> Drop for AbortOnDrop<T> {
	fn drop(&mut self) {
		self.0.abort();
	}
}

/// Owns a task spawned where [`Spawner::current`](crate::runtime::Spawner::current) picks,
/// the simulator included, cancelling it when this guard drops.
#[cfg(feature = "net")]
#[derive(Debug)]
pub(crate) struct Detached(futures::future::AbortHandle);

#[cfg(feature = "net")]
impl Detached {
	/// Spawn `future` beside the caller.
	pub(crate) fn spawn(future: impl Future<Output = ()> + Send + 'static) -> Self {
		let (future, handle) = futures::future::abortable(future);
		crate::runtime::Spawner::current().spawn(future);
		Self(handle)
	}

	/// Cancel the task at its next poll.
	pub(crate) fn abort(&self) {
		self.0.abort();
	}
}

#[cfg(feature = "net")]
impl Drop for Detached {
	fn drop(&mut self) {
		self.0.abort();
	}
}

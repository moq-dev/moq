use std::future::Future;
use std::sync::Arc;

use crate::error::MoqError;

#[cfg(not(target_arch = "wasm32"))]
struct AbortOnDrop(tokio::task::AbortHandle);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for AbortOnDrop {
	fn drop(&mut self) {
		self.0.abort();
	}
}

/// A dedicated runtime thread, so a foreign caller's thread never has to drive our futures.
///
/// wasm32 has neither threads nor a tokio driver. uniffi's `RustFuture` is polled by the
/// JS event loop instead, so [`Task::run`] awaits in place there.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) static RUNTIME: std::sync::LazyLock<tokio::runtime::Handle> = std::sync::LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("moq-ffi".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	handle
});

/// Enter the runtime context, so a handle built outside [`Task::run`] can still spawn.
///
/// A no-op on wasm32, where the JS event loop is the only runtime.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn enter() -> tokio::runtime::EnterGuard<'static> {
	RUNTIME.enter()
}

/// Stands in for tokio's `EnterGuard` on wasm32, so callers bind a guard either way.
#[cfg(target_arch = "wasm32")]
pub(crate) struct EnterGuard;

#[cfg(target_arch = "wasm32")]
pub(crate) fn enter() -> EnterGuard {
	EnterGuard
}

/// Run a future to completion off the caller's thread, mapping its error into [`MoqError`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn detached<F, T, E>(future: F) -> Result<T, MoqError>
where
	F: Future<Output = Result<T, E>> + Send + 'static,
	T: Send + 'static,
	E: Into<MoqError> + Send + 'static,
{
	let task = RUNTIME.spawn(future);
	let _abort = AbortOnDrop(task.abort_handle());
	match task.await {
		Ok(result) => result.map_err(Into::into),
		Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
		Err(e) => Err(MoqError::Task(e)),
	}
}

/// Await a future in place: wasm32 has no other thread to hand it to.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn detached<F, T, E>(future: F) -> Result<T, MoqError>
where
	F: Future<Output = Result<T, E>> + 'static,
	T: 'static,
	E: Into<MoqError> + 'static,
{
	future.await.map_err(Into::into)
}

pub(crate) struct Task<T: kio::MaybeSend + 'static> {
	state: Arc<tokio::sync::Mutex<T>>,
	cancel: tokio::sync::watch::Sender<bool>,
}

impl<T: kio::MaybeSend + 'static> Task<T> {
	pub fn new(inner: T) -> Self {
		Self {
			state: Arc::new(tokio::sync::Mutex::new(inner)),
			cancel: tokio::sync::watch::Sender::new(false),
		}
	}

	/// Try to lock the state synchronously. Returns `None` if a task is running.
	pub fn lock(&self) -> Option<tokio::sync::OwnedMutexGuard<T>> {
		self.state.clone().try_lock_owned().ok()
	}

	/// Spawn an async closure on the runtime.
	///
	/// The closure receives an [tokio::sync::OwnedMutexGuard] which derefs to `T`.
	/// If two calls are made concurrently, the second waits for the first to finish.
	#[cfg(not(target_arch = "wasm32"))]
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		R: Send + 'static,
		F: FnOnce(tokio::sync::OwnedMutexGuard<T>) -> Fut + Send + 'static,
		Fut: Future<Output = Result<R, MoqError>> + Send + 'static,
	{
		let cancel = self.cancel.subscribe();
		let state = self.state.clone();

		let handle = RUNTIME.spawn(async move { Self::drive(cancel, state, f).await });
		let _abort = AbortOnDrop(handle.abort_handle());

		match handle.await {
			Ok(result) => result,
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Run an async closure, awaiting it in place.
	///
	/// Unlike the native path this does not detach, so dropping the returned future
	/// cancels the work rather than leaving it running on a runtime thread.
	#[cfg(target_arch = "wasm32")]
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		R: 'static,
		F: FnOnce(tokio::sync::OwnedMutexGuard<T>) -> Fut + 'static,
		Fut: Future<Output = Result<R, MoqError>> + 'static,
	{
		Self::drive(self.cancel.subscribe(), self.state.clone(), f).await
	}

	/// Wait for the lock, then run `f`, with [Self::cancel] able to interrupt either.
	async fn drive<R, F, Fut>(
		mut cancel: tokio::sync::watch::Receiver<bool>,
		state: Arc<tokio::sync::Mutex<T>>,
		f: F,
	) -> Result<R, MoqError>
	where
		F: FnOnce(tokio::sync::OwnedMutexGuard<T>) -> Fut,
		Fut: Future<Output = Result<R, MoqError>>,
	{
		let state = tokio::select! {
			biased;
			Ok(_) = cancel.wait_for(|&c| c) => return Err(MoqError::Cancelled),
			state = state.lock_owned() => state,
		};

		tokio::select! {
			biased;
			Ok(_) = cancel.wait_for(|&c| c) => Err(MoqError::Cancelled),
			result = f(state) => result,
		}
	}

	/// Cancel all current and future [Self::run] calls, causing them to return [MoqError::Cancelled].
	pub fn cancel(&self) {
		self.cancel.send_replace(true);
	}
}

impl<T: kio::MaybeSend + 'static> Drop for Task<T> {
	fn drop(&mut self) {
		self.cancel();
	}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
	use std::sync::Arc;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::Duration;

	use super::*;

	#[tokio::test]
	async fn dropping_run_releases_the_state() {
		let task = Arc::new(Task::new(()));
		let running = task.clone();
		let (started_tx, started_rx) = tokio::sync::oneshot::channel();

		let outer = tokio::spawn(async move {
			running
				.run(move |_state| async move {
					started_tx.send(()).unwrap();
					std::future::pending::<Result<(), MoqError>>().await
				})
				.await
		});

		tokio::time::timeout(Duration::from_secs(5), started_rx)
			.await
			.expect("run did not start")
			.unwrap();
		outer.abort();
		assert!(outer.await.unwrap_err().is_cancelled());

		tokio::time::timeout(Duration::from_secs(5), task.run(|_state| async { Ok(()) }))
			.await
			.expect("cancelled run retained the state")
			.unwrap();
	}

	#[tokio::test]
	async fn cancel_before_run_is_terminal() {
		let task = Task::new(());
		let called = Arc::new(AtomicBool::new(false));
		let closure_called = called.clone();

		task.cancel();
		let error = task
			.run(move |_state| {
				closure_called.store(true, Ordering::SeqCst);
				async { Ok(()) }
			})
			.await
			.expect_err("run after cancel should fail");

		assert!(matches!(error, MoqError::Cancelled));
		assert!(!called.load(Ordering::SeqCst));
	}
}

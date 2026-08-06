use std::future::Future;
use std::sync::{Arc, LazyLock};

use crate::error::MoqError;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
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

pub(crate) struct Task<T: Send + 'static> {
	state: Arc<tokio::sync::Mutex<T>>,
	cancel: tokio::sync::watch::Sender<bool>,
}

impl<T: Send + 'static> Task<T> {
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
	/// The closure receives an [OwnedMutexGuard] which derefs to `T`.
	/// If two calls are made concurrently, the second waits for the first to finish.
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		R: Send + 'static,
		F: FnOnce(tokio::sync::OwnedMutexGuard<T>) -> Fut + Send + 'static,
		Fut: Future<Output = Result<R, MoqError>> + Send + 'static,
	{
		let mut cancel = self.cancel.subscribe();
		let state = self.state.clone();

		let handle = RUNTIME.spawn(async move {
			let state = tokio::select! {
				biased;
				Ok(_) = cancel.wait_for(|&c| c) => return Err(MoqError::Cancelled),
				state = state.lock_owned() => state,
			};

			let mut cancel = cancel;
			tokio::select! {
				biased;
				Ok(_) = cancel.wait_for(|&c| c) => Err(MoqError::Cancelled),
				result = f(state) => result,
			}
		});

		// Dropping a JoinHandle detaches its task rather than stopping it, so a caller
		// that gives up (an `asyncio.wait_for` timeout, a cancelled Swift/Kotlin task)
		// would leave the closure running with the state lock held. It then consumes
		// the very event the next call is waiting for, and that call blocks behind it
		// meanwhile. Tie the work to the caller's future instead.
		let _abort = AbortOnDrop(handle.abort_handle());

		match handle.await {
			Ok(result) => result,
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Cancel all current and future [Self::run] calls, causing them to return [MoqError::Cancelled].
	pub fn cancel(&self) {
		// send_replace, not send: `send` refuses to store the value while no
		// receiver exists, so a cancel BEFORE the first `run` would silently
		// no-op and the run would proceed.
		self.cancel.send_replace(true);
	}
}

impl<T: Send + 'static> Drop for Task<T> {
	fn drop(&mut self) {
		self.cancel();
	}
}

/// Aborts a spawned task when the future awaiting it is dropped.
///
/// Aborting one that already finished is a no-op, so this is inert on the happy path.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
	fn drop(&mut self) {
		self.0.abort();
	}
}

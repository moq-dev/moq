use std::future::Future;
use std::sync::{Arc, LazyLock};

use tokio::task::AbortHandle;

use crate::error::MoqError;

static HANDLE: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
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
	state: Arc<std::sync::Mutex<TaskState<T>>>,
}

struct TaskState<T> {
	inner: Option<T>,
	handles: Vec<AbortHandle>,
}

impl<T: Send + 'static> Task<T> {
	pub fn new(inner: T) -> Self {
		Self {
			state: Arc::new(std::sync::Mutex::new(TaskState {
				inner: Some(inner),
				handles: Vec::new(),
			})),
		}
	}

	/// Enter the tokio runtime context (for sync methods).
	pub fn enter() -> tokio::runtime::EnterGuard<'static> {
		HANDLE.enter()
	}

	/// Access state synchronously.
	pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, MoqError> {
		let mut state = self.state.lock().unwrap();
		let inner = state.inner.as_mut().ok_or(MoqError::Cancelled)?;
		Ok(f(inner))
	}

	/// Spawn an async closure on the runtime, taking ownership of the inner state.
	///
	/// The closure receives owned `T` and must return `(T, Result<R, MoqError>)`.
	/// State is put back automatically by the spawned task.
	/// If cancelled/aborted, the state is lost (which is fine — the object is being destroyed).
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		R: Send + 'static,
		F: FnOnce(T) -> Fut + Send + 'static,
		Fut: Future<Output = (T, Result<R, MoqError>)> + Send + 'static,
	{
		let join_handle = {
			let mut state = self.state.lock().unwrap();
			let inner = state.inner.take().ok_or(MoqError::Cancelled)?;
			let arc = self.state.clone();

			let handle = HANDLE.spawn(async move {
				let (inner, result) = f(inner).await;
				arc.lock().unwrap().inner = Some(inner);
				result
			});
			state.handles.push(handle.abort_handle());
			handle
		};

		match join_handle.await {
			Ok(result) => result,
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Cancel all outstanding tasks and drop the inner state.
	pub fn cancel(&self) {
		let mut state = self.state.lock().unwrap();
		state.inner = None;
		for handle in state.handles.drain(..) {
			handle.abort();
		}
	}
}

impl<T: Send + 'static> Drop for Task<T> {
	fn drop(&mut self) {
		self.cancel();
	}
}

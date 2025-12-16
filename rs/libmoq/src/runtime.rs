use std::{
	ops::{Deref, DerefMut},
	sync::{LazyLock, Mutex, MutexGuard},
};

/// Global tokio runtime handle.
///
/// Runs in a dedicated background thread to process async operations
/// spawned from FFI calls.
static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("libmoq".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	handle
});

/// A global lock that holds a value and provides a guard that enters the tokio runtime context.
#[derive(Default)]
pub struct RuntimeLock<T: Send + Sync> {
	inner: LazyLock<Mutex<T>>,
}

impl<T: Default + Send + Sync> RuntimeLock<T> {
	pub const fn new() -> Self {
		Self {
			inner: LazyLock::new(|| Mutex::new(T::default())),
		}
	}

	/// Lock the global state and enter the tokio runtime context.
	pub fn lock(&self) -> RuntimeGuard<'_, T> {
		let runtime = RUNTIME.enter();
		let inner = self.inner.lock().unwrap();

		RuntimeGuard {
			_runtime: runtime,
			inner,
		}
	}
}

/// Guard that holds the global state lock and tokio runtime context.
///
/// Automatically enters the tokio runtime context when locked, allowing
/// spawning of async tasks from FFI functions.
pub struct RuntimeGuard<'a, T: Send + Sync> {
	_runtime: tokio::runtime::EnterGuard<'static>,
	inner: MutexGuard<'a, T>,
}

impl<T: Send + Sync> Deref for RuntimeGuard<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T: Send + Sync> DerefMut for RuntimeGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

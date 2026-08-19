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

/// Serializes access to `T` across the FFI boundary, and releases it on cancel.
///
/// The state is dropped rather than parked once [Self::cancel] runs, so a handle the foreign
/// side has cancelled but not yet released stops holding whatever it wrapped: a subscription,
/// a codec session.
pub(crate) struct Task<T: Send + 'static> {
	/// `None` once cancelled, which is what makes running against a released state
	/// unrepresentable rather than merely documented.
	state: Arc<tokio::sync::Mutex<Option<T>>>,
	cancel: tokio::sync::watch::Sender<bool>,
}

/// Exclusive access to a [Task]'s state, held for as long as the guard lives.
pub(crate) type Guard<T> = tokio::sync::OwnedMappedMutexGuard<Option<T>, T>;

impl<T: Send + 'static> Task<T> {
	pub fn new(inner: T) -> Self {
		Self {
			state: Arc::new(tokio::sync::Mutex::new(Some(inner))),
			cancel: tokio::sync::watch::Sender::new(false),
		}
	}

	/// Try to lock the state synchronously. Returns `None` if a task is running or it was cancelled.
	pub fn lock(&self) -> Option<Guard<T>> {
		let guard = self.state.clone().try_lock_owned().ok()?;

		// The state is still `Some` between a cancel and the take it schedules, so ask the flag
		// rather than the state. Reading it under the lock is what makes that answer stable:
		// `cancel` publishes the flag before it queues for this same lock, so a cancel we don't
		// see here cannot have taken the state either.
		if *self.cancel.borrow() {
			return None;
		}

		tokio::sync::OwnedMutexGuard::try_map(guard, Option::as_mut).ok()
	}

	/// Whether [Self::cancel] has run, which [Self::lock] folds into the same `None` as a busy
	/// state. The state may still be a moment away from being dropped.
	pub fn is_cancelled(&self) -> bool {
		*self.cancel.borrow()
	}

	/// Spawn an async closure on the runtime.
	///
	/// The closure receives a [Guard] which derefs to `T`.
	/// If two calls are made concurrently, the second waits for the first to finish.
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		R: Send + 'static,
		F: FnOnce(Guard<T>) -> Fut + Send + 'static,
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

			// Cancelled while we queued behind another call, and the state is already gone.
			let state =
				tokio::sync::OwnedMutexGuard::try_map(state, Option::as_mut).map_err(|_| MoqError::Cancelled)?;

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

	/// Cancel all current and future [Self::run] calls, causing them to return [MoqError::Cancelled],
	/// and drop the state.
	///
	/// Terminal: there is no way back from here, and the state is gone rather than idle.
	/// The drop is not synchronous with this call, because an in-flight [Self::run] holds the
	/// state until it observes the flag and unwinds. It also lands on the runtime thread, which
	/// is where the state was built and the only place with a reactor for what it unregisters.
	pub fn cancel(&self) {
		// send_replace, not send: `send` refuses to store the value while no
		// receiver exists, so a cancel BEFORE the first `run` would silently
		// no-op and the run would proceed.
		if self.cancel.send_replace(true) {
			// Already cancelled, so the first call is the one dropping the state.
			return;
		}

		let state = self.state.clone();
		RUNTIME.spawn(async move {
			state.lock().await.take();
		});
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

#[cfg(test)]
mod tests {
	use std::sync::Arc;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::Duration;

	use super::*;

	/// A state that reports its own drop, which is what a terminal cancel has to trigger.
	struct Tracked(Option<tokio::sync::oneshot::Sender<()>>);

	impl Drop for Tracked {
		fn drop(&mut self) {
			let _ = self.0.take().expect("dropped once").send(());
		}
	}

	fn tracked() -> (Task<Tracked>, tokio::sync::oneshot::Receiver<()>) {
		let (tx, rx) = tokio::sync::oneshot::channel();
		(Task::new(Tracked(Some(tx))), rx)
	}

	/// Fail the test instead of hanging when a wakeup goes missing.
	async fn soon<F: Future>(f: F) -> F::Output {
		tokio::time::timeout(Duration::from_secs(5), f)
			.await
			.expect("timed out")
	}

	#[tokio::test]
	async fn cancel_drops_an_idle_state() {
		let (task, dropped) = tracked();

		task.cancel();
		soon(dropped).await.expect("state should be dropped");
	}

	#[tokio::test]
	async fn cancel_drops_the_state_under_an_in_flight_run() {
		let (task, dropped) = tracked();

		// Park a run holding the state, so the drop has to wait for it to unwind.
		let (started, running) = tokio::sync::oneshot::channel();
		let run = task.run(move |_state| async move {
			started.send(()).expect("test should still be waiting");
			std::future::pending::<Result<(), MoqError>>().await
		});
		tokio::pin!(run);

		// Poll the run far enough to take the lock; it parks forever after that.
		soon(async {
			tokio::select! {
				_ = &mut run => panic!("a pending run should not resolve"),
				started = running => started.expect("the run should start"),
			}
		})
		.await;

		task.cancel();

		let err = soon(&mut run).await.expect_err("the in-flight run should be cancelled");
		assert!(matches!(err, MoqError::Cancelled));
		soon(dropped).await.expect("state should be dropped");
	}

	#[tokio::test]
	async fn run_after_cancel_is_cancelled_without_touching_the_state() {
		let (task, dropped) = tracked();

		task.cancel();
		soon(dropped).await.expect("state should be dropped");

		// The closure would deref a state that no longer exists, so it must never be called.
		let called = Arc::new(AtomicBool::new(false));
		let flag = called.clone();
		let err = soon(task.run(move |_state| {
			flag.store(true, Ordering::SeqCst);
			async move { Ok(()) }
		}))
		.await
		.expect_err("a run after cancel should be cancelled");

		assert!(matches!(err, MoqError::Cancelled));
		assert!(!called.load(Ordering::SeqCst));
	}

	#[tokio::test]
	async fn lock_is_empty_from_the_moment_cancel_returns() {
		let (task, dropped) = tracked();
		assert!(task.lock().is_some());

		// No await in between: the answer can't depend on whether the scheduled take has run,
		// or a caller could still reach the state it is about to drop.
		task.cancel();
		assert!(task.lock().is_none());

		soon(dropped).await.expect("state should be dropped");
		assert!(task.lock().is_none());
	}

	#[tokio::test]
	async fn cancel_twice_is_a_no_op() {
		let (task, dropped) = tracked();

		task.cancel();
		task.cancel();
		soon(dropped).await.expect("state should be dropped");
	}
}

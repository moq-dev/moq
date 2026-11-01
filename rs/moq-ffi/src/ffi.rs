use std::future::Future;
use std::sync::Arc;

use crate::error::MoqError;

/// Saturate a duration handed to a binding at `i64::MAX` nanoseconds, about 292 years.
///
/// C and C++ carry a duration as `int64_t` nanoseconds and abort past that, and Go's
/// `time.Duration` wraps negative, so a peer's oversized value must not reach them.
pub(crate) fn duration(duration: std::time::Duration) -> std::time::Duration {
	duration.min(std::time::Duration::from_nanos(i64::MAX as u64))
}

/// A worker runtime driving I/O, timers, and background work beside host-polled calls.
static RUNTIME: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();

/// The worker runtime and the owner thread that joins it on shutdown.
pub(crate) struct Runtime {
	handle: tokio::runtime::Handle,
	/// The clock a test drives, when the runtime started with one.
	#[cfg(feature = "testing")]
	clock: Option<moq_clock::Installed>,
	/// Taken by [`shutdown`], so the thread is stopped and joined once.
	thread: std::sync::Mutex<Option<Thread>>,
	/// Set by [`shutdown`] to wake the [`Task::run`] calls parked in place, so they observe
	/// [`Polls::stopped`] and resolve `Cancelled`.
	stopped: tokio::sync::watch::Sender<bool>,
	/// The [`Task::run`] polls in flight on host threads, which [`shutdown`] waits out.
	polls: std::sync::Mutex<Polls>,
	/// Notified when the last in-flight poll finishes.
	idle: std::sync::Condvar,
}

#[derive(Default)]
struct Polls {
	active: usize,
	/// Set by [`shutdown`]: no poll may start once the drivers are about to stop, because a
	/// tokio timer polled after its driver is gone panics rather than erroring.
	stopped: bool,
}

struct Thread {
	/// Resolves the `block_on` the thread parks in.
	stop: tokio::sync::oneshot::Sender<()>,
	join: std::thread::JoinHandle<()>,
}

impl std::ops::Deref for Runtime {
	type Target = tokio::runtime::Handle;

	fn deref(&self) -> &Self::Target {
		&self.handle
	}
}

/// The runtime, started on first use.
pub(crate) fn runtime() -> &'static Runtime {
	RUNTIME.get_or_init(|| Runtime::new(false))
}

/// Start the runtime on a [`moq_clock::Manual`] clock that only a test moves.
///
/// False when the runtime already started, since its timers are armed on the host clock.
#[cfg(feature = "testing")]
pub(crate) fn start_test_clock() -> bool {
	let mut started = false;
	RUNTIME.get_or_init(|| {
		started = true;
		Runtime::new(true)
	});
	started
}

/// The clock [`start_test_clock`] installed, if it did.
#[cfg(feature = "testing")]
pub(crate) fn test_clock() -> Option<&'static moq_clock::Installed> {
	RUNTIME.get()?.clock.as_ref()
}

impl Runtime {
	fn new(
		#[cfg_attr(
			not(feature = "testing"),
			allow(unused_variables, reason = "only the `testing` feature uses it")
		)]
		test_clock: bool,
	) -> Self {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.thread_name("moq-ffi-worker")
			.enable_all()
			.build()
			.unwrap();
		let handle = runtime.handle().clone();
		// Before any task runs, so every read and timer lands on it.
		#[cfg(feature = "testing")]
		let clock = test_clock.then(|| moq_clock::Manual::new().install(&handle));
		let (stop, stopped) = tokio::sync::oneshot::channel();

		let join = std::thread::Builder::new()
			.name("moq-ffi".into())
			.spawn(move || {
				// `Err` when the sender is dropped, which the shutdown never does; either way the
				// runtime is torn down here, joining the threads that ran its tasks.
				let _ = runtime.block_on(stopped);
				// Join every worker and blocking task before the host can unload callbacks.
				drop(runtime);
			})
			.expect("failed to spawn runtime thread");

		Runtime {
			handle,
			#[cfg(feature = "testing")]
			clock,
			thread: std::sync::Mutex::new(Some(Thread { stop, join })),
			stopped: tokio::sync::watch::Sender::new(false),
			polls: Default::default(),
			idle: Default::default(),
		}
	}
}

/// Stop the runtime workers and wait for them to finish; a no-op when it never started.
///
/// See [`crate::moq_ffi_shutdown`] for what this is for.
pub(crate) fn shutdown() {
	let Some(runtime) = RUNTIME.get() else { return };
	runtime.shutdown();
}

impl Runtime {
	fn shutdown(&self) {
		let runtime = self;
		let mut owner = runtime.thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		let Some(thread) = owner.take() else {
			return;
		};
		// Before the thread stops, so no call awaited in place polls the runtime after it is gone.
		let mut polls = runtime.polls.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		polls.stopped = true;
		drop(
			runtime
				.idle
				.wait_while(polls, |polls| polls.active > 0)
				.unwrap_or_else(std::sync::PoisonError::into_inner),
		);
		// Outside the lock: a waker may poll inline, which takes it.
		runtime.stopped.send_replace(true);
		let _ = thread.stop.send(());
		thread.join.join().expect("runtime owner panicked");
		// Retain the owner lock until the join completes, including concurrent shutdown calls.
		drop(owner);
	}
}

/// Resolves once [`shutdown`] has run.
async fn stopped() {
	let mut stopped = runtime().stopped.subscribe();
	// `Err` only when the sender is dropped, which a static never is.
	let _ = stopped.wait_for(|&stopped| stopped).await;
}

/// Marks a [`Task::run`] poll in flight, so [`shutdown`] waits for it to finish.
struct Polling;

impl Polling {
	/// `None` once [`shutdown`] has begun.
	fn start() -> Option<Self> {
		let mut polls = runtime()
			.polls
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if polls.stopped {
			return None;
		}
		polls.active += 1;
		Some(Self)
	}
}

impl Drop for Polling {
	fn drop(&mut self) {
		let runtime = runtime();
		let mut polls = runtime.polls.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		polls.active -= 1;
		if polls.active == 0 {
			runtime.idle.notify_all();
		}
	}
}

/// Enter the runtime context, so a handle built outside [`Task::run`] can still spawn.
pub(crate) fn enter() -> tokio::runtime::EnterGuard<'static> {
	runtime().enter()
}

/// Completion of an owned operation, retained by the object's host calls.
#[derive(Clone, Default)]
pub(crate) struct Background {
	result: Option<kio::Consumer<Option<moq_tokio::Result<()>>>>,
}

impl Background {
	pub fn new(future: impl Future<Output = moq_tokio::Result<()>> + Send + 'static) -> Self {
		let result = kio::Producer::new(None);
		let receiver = result.consume();
		runtime().spawn(async move {
			let completed = moq_tokio::panic::isolate(future)
				.await
				.unwrap_or(Err(moq_tokio::Error::Panic));
			*result
				.write()
				.unwrap_or_else(|_| unreachable!("only the task closes its completion")) = Some(completed);
		});
		Self { result: Some(receiver) }
	}

	pub fn driver(mut driver: impl moq_net::time::Driver + Send + 'static) -> Self {
		let result = kio::Producer::new(None);
		let receiver = result.consume();
		runtime().spawn(async move {
			// Keep channel teardown outside the unwind and after panic publication.
			let completed = moq_tokio::panic::isolate(async {
				match moq_net::time::run(Borrowed(&mut driver)).await {
					moq_net::Error::Closed => Ok(()),
					error => Err(error.into()),
				}
			})
			.await
			.unwrap_or(Err(moq_tokio::Error::Panic));
			if matches!(&completed, Err(moq_tokio::Error::Panic)) {
				*result
					.write()
					.unwrap_or_else(|_| unreachable!("only the task closes its completion")) = Some(completed.clone());
			}
			let completed = moq_tokio::panic::isolate(async move {
				drop(driver);
				completed
			})
			.await
			.unwrap_or(Err(moq_tokio::Error::Panic));
			*result
				.write()
				.unwrap_or_else(|_| unreachable!("only the task closes its completion")) = Some(completed);
		});
		Self { result: Some(receiver) }
	}

	pub async fn finished(&self) -> Result<(), MoqError> {
		let Some(result) = &self.result else { return Ok(()) };
		let completed = kio::wait(|waiter| {
			match std::task::ready!(result.poll(waiter, |state| {
				match &**state {
					Some(completed) => std::task::Poll::Ready(completed.clone()),
					None => std::task::Poll::Pending,
				}
			})) {
				Ok(completed) => std::task::Poll::Ready(completed),
				Err(_) => std::task::Poll::Ready(Err(moq_net::Error::Cancel.into())),
			}
		})
		.await;
		completed.map_err(crate::session::map_closed_error)
	}
}

impl Background {
	pub fn check(&self) -> Result<(), MoqError> {
		if let Some(result) = &self.result {
			return result
				.read()
				.clone()
				.unwrap_or(Ok(()))
				.map_err(crate::session::map_closed_error);
		}
		Ok(())
	}

	pub async fn watch<T>(&self, future: impl Future<Output = Result<T, MoqError>>) -> Result<T, MoqError> {
		self.check()?;
		let result = if self.result.is_some() {
			tokio::pin!(future);
			tokio::select! {
				biased;
				result = self.finished() => { result?; future.await }
				result = &mut future => result,
			}
		} else {
			future.await
		};
		self.check()?;
		result
	}
}

#[cfg(test)]
pub(crate) fn panicking_origin() -> (moq_net::origin::Producer, Background, kio::Producer<bool>) {
	struct Driver {
		inner: moq_net::origin::Driver,
		failed: kio::Consumer<bool>,
	}
	impl moq_net::time::Driver for Driver {
		fn poll(
			&mut self,
			now: moq_net::time::Instant,
			waiter: &kio::Waiter,
		) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
			let _ = self.failed.poll(waiter, |state| {
				assert!(!**state, "injected origin driver panic");
				std::task::Poll::<()>::Pending
			});
			self.inner.poll(now, waiter)
		}
	}
	let (origin, inner) = moq_net::origin::Producer::new(Default::default());
	let failed = kio::Producer::new(false);
	let driver = Background::driver(Driver {
		inner,
		failed: failed.consume(),
	});
	(origin, driver, failed)
}

struct Borrowed<'a, D>(&'a mut D);

impl<D: moq_net::time::Driver> moq_net::time::Driver for Borrowed<'_, D> {
	fn poll(
		&mut self,
		now: moq_net::time::Instant,
		waiter: &kio::Waiter,
	) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
		self.0.poll(now, waiter)
	}
}

/// Spawn an owned background future on the runtime.
pub(crate) fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Background {
	Background::new(async move {
		future.await;
		Ok(())
	})
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

/// Run a future to completion off the caller's thread, mapping its error into [`MoqError`].
pub(crate) async fn detached<F, T, E>(future: F) -> Result<T, MoqError>
where
	F: Future<Output = Result<T, E>> + Send + 'static,
	T: Send + 'static,
	E: Into<MoqError> + Send + 'static,
{
	let task = runtime().spawn(async move {
		moq_tokio::panic::isolate(async move { future.await.map_err(Into::into) })
			.await
			.unwrap_or_else(|| Err(crate::session::map_closed_error(moq_tokio::Error::Panic)))
	});
	let _abort = AbortOnDrop(task.abort_handle());
	match task.await {
		Ok(result) => result,
		Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
		Err(e) => Err(e.into()),
	}
}

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
	cleanup: std::sync::Mutex<Option<Background>>,
	driver: Background,
}

/// Exclusive access to a [Task]'s state, held for as long as the guard lives.
pub(crate) type Guard<T> = tokio::sync::OwnedMappedMutexGuard<Option<T>, T>;

impl<T: Send + 'static> Task<T> {
	pub fn new(inner: T) -> Self {
		Self {
			state: Arc::new(tokio::sync::Mutex::new(Some(inner))),
			cancel: tokio::sync::watch::Sender::new(false),
			cleanup: Default::default(),
			driver: Background::default(),
		}
	}

	pub fn supervised(mut self, driver: Background) -> Self {
		self.driver = driver;
		self
	}

	/// Try to lock the state synchronously. Returns `None` if a task is running or it was cancelled.
	// Only the server and its requests read state synchronously.
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

	/// Lock for a synchronous configuration write.
	///
	/// [Self::lock] folds a running task and a cancelled handle into one `None`.
	/// Configuration must apply or fail, so this splits them: [`MoqError::Busy`]
	/// while a [`Self::run`] owns the state, [`MoqError::Cancelled`] after
	/// [`Self::cancel`].
	pub fn configure(&self) -> Result<Guard<T>, MoqError> {
		self.driver.check()?;
		if let Some(cleanup) = self.cleanup.lock().unwrap().as_ref() {
			cleanup.check()?;
		}
		match self.lock() {
			Some(guard) => Ok(guard),
			None if self.is_cancelled() => Err(MoqError::Cancelled),
			None => Err(MoqError::Busy),
		}
	}

	/// Run an async closure against the state, awaiting it in place.
	///
	/// The closure receives a [Guard] which derefs to `T`.
	/// If two calls are made concurrently, the second waits for the first to finish.
	///
	/// Not spawned: a foreign cancel (uniffi's `rust_future_cancel`) only stops polling, and the
	/// future is dropped later at `rust_future_free`. Spawned work would keep running until then
	/// and take the very event the next call is waiting for. Each poll enters the runtime, so the
	/// closure can still use tokio timers, sockets, and spawn.
	pub async fn run<R, F, Fut>(&self, f: F) -> Result<R, MoqError>
	where
		F: FnOnce(Guard<T>) -> Fut,
		Fut: Future<Output = Result<R, MoqError>>,
	{
		self.driver.check()?;
		if let Some(cleanup) = self.cleanup.lock().unwrap().as_ref() {
			cleanup.check()?;
		}
		let result = {
			let drive = self
				.driver
				.watch(Self::drive(self.cancel.subscribe(), self.state.clone(), f));
			let mut run = std::pin::pin!(async {
				tokio::select! {
					biased;
					_ = stopped() => Err(MoqError::Cancelled),
					result = drive => result,
				}
			});
			std::future::poll_fn(|cx| {
				let Some(_polling) = Polling::start() else {
					return std::task::Poll::Ready(Err(MoqError::Cancelled));
				};
				let _runtime = enter();
				run.as_mut().poll(cx)
			})
			.await
		};
		// A foreign future may stop polling while holding the state, so cancellation cannot
		// wait for cleanup. Its next call still reports a completed cleanup panic.
		self.driver.check()?;
		if let Some(cleanup) = self.cleanup.lock().unwrap().as_ref() {
			cleanup.check()?;
		}
		result
	}

	/// Wait for the lock, then run `f`, with [Self::cancel] able to interrupt either.
	async fn drive<R, F, Fut>(
		mut cancel: tokio::sync::watch::Receiver<bool>,
		state: Arc<tokio::sync::Mutex<Option<T>>>,
		f: F,
	) -> Result<R, MoqError>
	where
		F: FnOnce(Guard<T>) -> Fut,
		Fut: Future<Output = Result<R, MoqError>>,
	{
		let state = tokio::select! {
			biased;
			Ok(_) = cancel.wait_for(|&c| c) => return Err(MoqError::Cancelled),
			state = state.lock_owned() => state,
		};

		// Cancelled while we queued behind another call, and the state is already gone.
		let state = tokio::sync::OwnedMutexGuard::try_map(state, Option::as_mut).map_err(|_| MoqError::Cancelled)?;

		tokio::select! {
			biased;
			Ok(_) = cancel.wait_for(|&c| c) => Err(MoqError::Cancelled),
			result = f(state) => result,
		}
	}

	/// Move the state to a new owner, ending this handle as [Self::cancel] would.
	///
	/// `check` sees the state first, and an error there leaves this handle untouched.
	/// [`MoqError::Busy`] while a [Self::run] owns the state, [`MoqError::Cancelled`] after
	/// [Self::cancel].
	pub fn take(&self, check: impl FnOnce(&T) -> Result<(), MoqError>) -> Result<T, MoqError> {
		self.driver.check()?;
		if let Some(cleanup) = self.cleanup.lock().unwrap().as_ref() {
			cleanup.check()?;
		}
		if *self.cancel.borrow() {
			return Err(MoqError::Cancelled);
		}
		let mut state = self.state.clone().try_lock_owned().map_err(|_| MoqError::Busy)?;
		// Re-read under the lock, as `lock` does: a cancel we don't see here has not taken the state.
		if *self.cancel.borrow() {
			return Err(MoqError::Cancelled);
		}
		check(state.as_ref().ok_or(MoqError::Cancelled)?)?;
		let inner = state.take().ok_or(MoqError::Cancelled)?;
		// The state is already gone, so this only fails later runs; nothing is left to drop.
		self.cancel.send_replace(true);
		Ok(inner)
	}

	/// Cancel all current and future [Self::run] calls and drop the state.
	///
	/// Calls return [MoqError::Cancelled]; later calls report a completed cleanup panic.
	///
	/// Terminal: there is no way back from here, and the state is gone rather than idle.
	/// The drop is not synchronous with this call, because an in-flight [Self::run] holds the
	/// state until it observes the flag and unwinds, or its caller drops it. It also lands on the
	/// runtime workers, whose entered reactor unregisters the state it drops.
	pub fn cancel(&self) {
		// send_replace, not send: `send` refuses to store the value while no
		// receiver exists, so a cancel BEFORE the first `run` would silently
		// no-op and the run would proceed.
		let mut cleanup = self.cleanup.lock().unwrap();
		if self.cancel.send_replace(true) {
			// Already cancelled, so the first call is the one dropping the state.
			return;
		}

		let state = self.state.clone();
		*cleanup = Some(spawn(async move {
			state.lock().await.take();
		}));
	}
}

impl<T: Send + 'static> Drop for Task<T> {
	fn drop(&mut self) {
		self.cancel();
	}
}

#[cfg(test)]
mod tests {

	#[tokio::test]
	async fn a_driver_teardown_panic_reaches_its_owner() {
		struct Driver;
		impl moq_net::time::Driver for Driver {
			fn poll(
				&mut self,
				_: moq_net::time::Instant,
				_: &kio::Waiter,
			) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
				Err(moq_net::Error::Closed)
			}
		}
		impl Drop for Driver {
			fn drop(&mut self) {
				panic!("injected driver teardown panic");
			}
		}
		let driver = super::Background::driver(Driver);
		assert!(matches!(driver.finished().await, Err(super::MoqError::Task { .. })));
	}
	#[tokio::test]
	async fn a_cleanup_panic_reaches_the_host_while_another_task_keeps_working() {
		struct Panics;
		impl Drop for Panics {
			fn drop(&mut self) {
				panic!("injected cleanup panic");
			}
		}
		let failed = super::Task::new(Panics);
		failed.cancel();
		let cleanup = failed.cleanup.lock().unwrap().clone().unwrap();
		assert!(matches!(cleanup.finished().await, Err(super::MoqError::Task { .. })));
		let result = failed.run(|_| async { Ok(()) }).await;
		assert!(matches!(result, Err(super::MoqError::Task { .. })), "{result:?}");
		let healthy = super::Task::new(7);
		assert_eq!(healthy.run(|state| async move { Ok(*state) }).await.unwrap(), 7);
	}

	use std::sync::Arc;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::Duration;

	use super::*;

	#[test]
	fn shutdown_joins_a_worker_callback() {
		let runtime = Arc::new(Runtime::new(false));
		let (entered, entering) = std::sync::mpsc::channel();
		let (release, released) = std::sync::mpsc::channel();
		let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let callback = finished.clone();
		runtime.spawn(async move {
			entered.send(()).unwrap();
			released.recv().unwrap();
			callback.store(true, std::sync::atomic::Ordering::SeqCst);
		});
		entering.recv().unwrap();

		// Each caller reports whether the callback had finished when its shutdown returned.
		let shutdown = |runtime: Arc<Runtime>, finished: Arc<std::sync::atomic::AtomicBool>| {
			move || {
				runtime.shutdown();
				finished.load(std::sync::atomic::Ordering::SeqCst)
			}
		};

		// Wait until the first caller has flagged the stop, so it holds the owner lock and is
		// joining the workers while the callback is still held.
		let mut stopped = runtime.stopped.subscribe();
		let first = std::thread::spawn(shutdown(runtime.clone(), finished.clone()));
		tokio::runtime::Builder::new_current_thread()
			.build()
			.unwrap()
			.block_on(stopped.wait_for(|&stopped| stopped))
			.unwrap();

		let (started, starting) = std::sync::mpsc::channel();
		let second = std::thread::spawn({
			let shutdown = shutdown(runtime.clone(), finished.clone());
			move || {
				started.send(()).unwrap();
				shutdown()
			}
		});
		starting.recv().unwrap();

		release.send(()).unwrap();
		assert!(first.join().unwrap(), "shutdown returned with a callback in flight");
		assert!(
			second.join().unwrap(),
			"a concurrent shutdown returned with a callback in flight"
		);
		runtime.shutdown();
	}

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
	async fn a_queued_cancel_does_not_wait_for_an_unpolled_owner() {
		let task = Task::new(());
		let first = task.run(|state| async move {
			let _state = state;
			std::future::pending::<Result<(), MoqError>>().await
		});
		tokio::pin!(first);
		assert!(
			std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx)))
				.await
				.is_pending()
		);
		task.cancel();
		let queued = task.run(|_| async { panic!("cancelled calls never touch the state") });
		tokio::pin!(queued);
		let result: std::task::Poll<Result<(), MoqError>> =
			std::future::poll_fn(|cx| std::task::Poll::Ready(queued.as_mut().poll(cx))).await;
		assert!(
			matches!(result, std::task::Poll::Ready(Err(MoqError::Cancelled))),
			"{result:?}"
		);
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

	#[tokio::test]
	async fn configure_is_busy_while_a_run_holds_the_lock() {
		let task = Task::new(());
		let (started, running) = tokio::sync::oneshot::channel();
		let run = task.run(move |state| async move {
			let _state = state;
			started.send(()).expect("test should still be waiting");
			std::future::pending::<Result<(), MoqError>>().await
		});
		tokio::pin!(run);

		soon(async {
			tokio::select! {
				_ = &mut run => panic!("a pending run should not resolve"),
				started = running => started.expect("the run should start"),
			}
		})
		.await;

		assert!(matches!(task.configure(), Err(MoqError::Busy)));
		task.cancel();
		assert!(matches!(task.configure(), Err(MoqError::Cancelled)));

		let err = soon(&mut run).await.expect_err("the in-flight run should be cancelled");
		assert!(matches!(err, MoqError::Cancelled));
	}

	#[tokio::test]
	async fn take_is_busy_while_a_run_holds_the_lock() {
		let task = Task::new(7);
		let (started, running) = tokio::sync::oneshot::channel();
		let run = task.run(move |state| async move {
			let _state = state;
			started.send(()).expect("test should still be waiting");
			std::future::pending::<Result<(), MoqError>>().await
		});
		tokio::pin!(run);

		soon(async {
			tokio::select! {
				_ = &mut run => panic!("a pending run should not resolve"),
				started = running => started.expect("the run should start"),
			}
		})
		.await;
		assert!(matches!(task.take(|_| Ok(())), Err(MoqError::Busy)));
	}

	#[tokio::test]
	async fn take_moves_the_state_unless_refused() {
		let task = Task::new(7);
		assert!(matches!(
			task.take(|_| Err(MoqError::AlreadyCommitted)),
			Err(MoqError::AlreadyCommitted)
		));
		assert_eq!(
			task.take(|state| {
				assert_eq!(*state, 7);
				Ok(())
			})
			.unwrap(),
			7
		);

		let err = task
			.run(|_| async { Ok(()) })
			.await
			.expect_err("taken state cannot run");
		assert!(matches!(err, MoqError::Cancelled));
		assert!(matches!(task.take(|_| Ok(())), Err(MoqError::Cancelled)));
	}

	#[test]
	fn configure_is_cancelled_from_the_moment_cancel_returns() {
		let task = Task::new(());
		assert!(task.configure().is_ok());

		task.cancel();
		assert!(matches!(task.configure(), Err(MoqError::Cancelled)));
	}
}

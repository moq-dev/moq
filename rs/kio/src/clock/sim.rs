//! A single-threaded, deterministic executor with simulated time.
//!
//! Inside [`run`], the ambient clock is the simulator's: [`now`](crate::clock::now) starts
//! at [`Instant::ZERO`], [`wall`](crate::clock::wall) at [`Manual::WALL_START`], and
//! [`sleep`](crate::clock::sleep), [`timeout`](crate::clock::timeout), and
//! [`interval`](crate::clock::interval) arm simulated timers. Time never moves on its own:
//! once every task has stalled, [`run`] jumps the clock to the earliest armed
//! timer and wakes it, like tokio's paused clock, so a test never waits on the
//! host clock. A run whose tasks are all parked with nothing armed can never
//! finish, so it panics rather than hang, unless a task awaits [`idle`], which
//! completes at exactly that point.
//!
//! A task poll that makes more than 4096 ready kio polls panics with the caller's
//! source location, so a loop that forgets to yield fails instead of starving its siblings.
//! Each `crate::Tasks` child poll counts on its own, like a spawned task.
//!
//! Caller-driven state machines take time from their caller:
//! [`drive`](crate::clock::drive) polls one with the simulated instant, and [`attach`]
//! keeps a [`Deadlines`] queue in step.

use std::{
	cell::{Cell, RefCell},
	collections::BTreeMap,
	future::{Future, IntoFuture},
	pin::{Pin, pin},
	rc::Rc,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
	time::Duration,
};

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};

use crate::clock::{Deadlines, Instant, Manual, Wall, deadline::WeakDeadlines};

thread_local! {
	static CURRENT: RefCell<Option<Rc<Runtime>>> = const { RefCell::new(None) };
}

type Task = Pin<Box<dyn Future<Output = ()>>>;

fn poll<F: Future + ?Sized>(future: Pin<&mut F>, cx: &mut Context<'_>) -> Poll<F::Output> {
	let _scope = crate::coop::scope();
	future.poll(cx)
}

struct Runtime {
	/// Tells this run from earlier ones on the thread, so a timer finds its own.
	id: u64,
	/// Spawned since the run loop last collected them.
	spawned: RefCell<Vec<Task>>,
	time: RefCell<Time>,
	/// Queues kept in step with simulated time, until their owners drop them.
	queues: RefCell<Vec<WeakDeadlines>>,
	/// Awaiting [`idle`]: each flag is raised, and its waker woken, once nothing else can run.
	idle: RefCell<Vec<(Rc<Cell<bool>>, Waker)>>,
}

struct Time {
	clock: Manual,
	anchor: Option<(Instant, Wall)>,
	/// Orders timers armed for the same instant by when they were armed.
	next: u64,
	timers: BTreeMap<(Instant, u64), Waker>,
}

impl Runtime {
	fn enter() -> Rc<Self> {
		static RUNS: AtomicU64 = AtomicU64::new(0);
		let runtime = Rc::new(Runtime {
			id: RUNS.fetch_add(1, Ordering::Relaxed),
			spawned: RefCell::new(Vec::new()),
			time: RefCell::new(Time {
				clock: Manual::new(),
				anchor: None,
				next: 0,
				timers: BTreeMap::new(),
			}),
			queues: RefCell::new(Vec::new()),
			idle: RefCell::new(Vec::new()),
		});
		CURRENT.with(|current| {
			let mut current = current.borrow_mut();
			assert!(current.is_none(), "kio::clock::sim::run cannot nest");
			*current = Some(runtime.clone());
		});
		runtime
	}

	fn exit(&self, enter: Enter) {
		drop(enter);
		// Timers and idle waiters may hold wakers into the dropped tasks.
		self.queues.borrow_mut().clear();
		self.idle.borrow_mut().clear();
		self.time.borrow_mut().timers.clear();
	}

	fn now(&self) -> Instant {
		self.time.borrow().clock.now()
	}

	/// Move time to `at` (never backwards), waking every timer and queued deadline due by then.
	fn advance_to(&self, at: Instant) {
		let due = {
			let mut time = self.time.borrow_mut();
			let now = time.clock.now();
			time.clock.advance(at.saturating_duration_since(now));
			let now = time.clock.now();
			let mut due = Vec::new();
			while let Some(entry) = time.timers.first_entry() {
				if entry.key().0 > now {
					break;
				}
				due.push(entry.remove());
			}
			due
		};
		due.into_iter().for_each(Waker::wake);
		self.advance_queues();
	}

	/// Bring every attached queue to the current instant, forgetting dropped ones,
	/// and return their earliest deadline, which is always still ahead.
	fn advance_queues(&self) -> Option<Instant> {
		let now = self.now();
		// Upgraded first, so no borrow is held while advancing wakes tasks.
		let live: Vec<Deadlines> = {
			let mut live = Vec::new();
			self.queues
				.borrow_mut()
				.retain(|queue| queue.upgrade().map(|queue| live.push(queue)).is_some());
			live
		};
		live.iter()
			.filter_map(|queue| {
				queue.sync(now);
				queue.next()
			})
			.min()
	}

	/// Jump to the earliest armed timer or `queued`, the queues' earliest deadline.
	/// False when nothing is armed.
	fn advance(&self, queued: Option<Instant>) -> bool {
		let timer = self.time.borrow().timers.first_key_value().map(|((at, _), _)| *at);
		match timer.into_iter().chain(queued).min() {
			Some(at) => {
				self.advance_to(at);
				true
			}
			None => false,
		}
	}
}

fn current() -> Option<Rc<Runtime>> {
	CURRENT.with(|current| current.borrow().clone())
}

fn expect_current() -> Rc<Runtime> {
	current().expect("not inside kio::clock::sim::run")
}

/// The simulated instant, when the calling thread is inside [`run`].
pub(crate) fn now() -> Option<Instant> {
	current().map(|runtime| runtime.now())
}

/// The simulated wall time, when the calling thread is inside [`run`].
pub(crate) fn wall() -> Option<Wall> {
	current().map(|runtime| runtime.time.borrow().clock.wall())
}

pub(crate) fn anchor() -> Option<(Instant, Wall)> {
	current().map(|runtime| {
		let mut time = runtime.time.borrow_mut();
		if let Some(anchor) = time.anchor {
			return anchor;
		}
		let anchor = (time.clock.now(), time.clock.wall());
		time.anchor = Some(anchor);
		anchor
	})
}

/// Whether the calling thread is inside [`run`].
pub fn is_running() -> bool {
	CURRENT.with(|current| current.borrow().is_some())
}

/// Raises a flag the run loop checks, for the root future and the spawned tasks.
struct Flag(AtomicBool);

impl Wake for Flag {
	fn wake(self: Arc<Self>) {
		self.0.store(true, Ordering::SeqCst);
	}
}

/// Clears the thread's runtime when [`run`] returns or unwinds.
struct Enter;

impl Drop for Enter {
	fn drop(&mut self) {
		CURRENT.with(|current| current.borrow_mut().take());
	}
}

/// Run `future` to completion, along with everything it spawns, in simulated time.
///
/// Spawned tasks still pending when `future` finishes are dropped.
///
/// # Panics
///
/// When called inside another `run`, and when every task is parked with no timer
/// armed and none awaits [`idle`], which is a deadlock. Also when one root poll
/// exceeds the ready kio poll limit, or a detached task panics.
pub fn run<F: IntoFuture>(future: F) -> F::Output {
	let runtime = Runtime::enter();
	let enter = Enter;

	let mut future = pin!(future.into_future());
	let mut output = None;
	{
		// Keep the caller's future and output on its stack while sharing the executor.
		let future = pin!(std::future::poll_fn(|cx| {
			future.as_mut().poll(cx).map(|value| output = Some(value))
		}));
		run_inner(&runtime, future);
	}
	runtime.exit(enter);
	output.expect("root future completed")
}

fn run_inner(runtime: &Runtime, mut future: Pin<&mut dyn Future<Output = ()>>) {
	let root = Arc::new(Flag(AtomicBool::new(true)));
	let root_waker = Waker::from(root.clone());
	let mut root_cx = Context::from_waker(&root_waker);

	let mut tasks = FuturesUnordered::new();
	let pool = Arc::new(Flag(AtomicBool::new(false)));
	let pool_waker = Waker::from(pool.clone());
	let mut pool_cx = Context::from_waker(&pool_waker);

	// The root and the spawned tasks take turns, so a task that keeps waking
	// itself cannot starve the root: `FuturesUnordered` yields after one pass.
	loop {
		if root.0.swap(false, Ordering::SeqCst) && poll(future.as_mut(), &mut root_cx).is_ready() {
			break;
		}
		tasks.extend(runtime.spawned.take());
		pool.0.store(false, Ordering::SeqCst);
		while let Poll::Ready(Some(())) = tasks.poll_next_unpin(&mut pool_cx) {}
		// Bring queues to the current instant before deciding to stall: one attached
		// behind the clock may wake a task without time moving.
		let queued = runtime.advance_queues();
		if root.0.load(Ordering::SeqCst) || pool.0.load(Ordering::SeqCst) || !runtime.spawned.borrow().is_empty() {
			continue;
		}
		if runtime.advance(queued) {
			continue;
		}
		let idle = runtime.idle.take();
		assert!(!idle.is_empty(), "deadlock: every task is parked and no timer is armed");
		for (settled, waker) in idle {
			settled.set(true);
			waker.wake();
		}
	}

	drop(tasks);
	drop(runtime.spawned.take());
}

/// Keep `deadlines` in step with simulated time until every clone of it is dropped.
///
/// The queue is brought to the simulated instant at once, then advanced whenever
/// time moves, and its earliest deadline counts as an armed timer.
///
/// # Panics
///
/// Outside [`run`], and when the queue is ahead of the simulated clock.
pub fn attach(deadlines: &Deadlines) {
	let runtime = expect_current();
	deadlines.sync(runtime.now());
	runtime.queues.borrow_mut().push(WeakDeadlines::new(deadlines));
}

/// Let `duration` of simulated time pass, moving both the instant and the wall
/// time and waking everything due, then yield.
///
/// # Panics
///
/// Outside [`run`].
pub async fn advance(duration: Duration) {
	let runtime = expect_current();
	let at = runtime.now() + duration;
	runtime.advance_to(at);
	yield_now().await;
}

/// Set the simulated wall time, forward or back, as a clock correction would.
/// No time passes and no timer fires.
///
/// # Panics
///
/// Outside [`run`].
pub fn step_wall(wall: Wall) {
	expect_current().time.borrow_mut().clock.step_wall(wall);
}

/// Complete once every other task is parked and no timer or queued deadline is
/// armed, so nothing can happen until something outside the run acts: the system
/// has settled.
///
/// # Panics
///
/// Outside [`run`].
pub fn idle() -> impl Future<Output = ()> {
	let runtime = expect_current();
	let settled = Rc::new(Cell::new(false));
	std::future::poll_fn(move |cx| {
		if settled.get() {
			return Poll::Ready(());
		}
		runtime.idle.borrow_mut().push((settled.clone(), cx.waker().clone()));
		Poll::Pending
	})
}

/// Yield once, letting every other ready task run first.
pub fn yield_now() -> impl Future<Output = ()> {
	let mut yielded = false;
	std::future::poll_fn(move |cx| {
		if yielded {
			return Poll::Ready(());
		}
		yielded = true;
		cx.waker().wake_by_ref();
		Poll::Pending
	})
}

/// A simulated timer behind [`Sleep`](crate::clock::Sleep).
///
/// Holds plain data rather than the runtime, so enabling `sim` in a test build
/// leaves [`Sleep`](crate::clock::Sleep) `Send`. It finds its run as the thread's current one.
pub(crate) struct Timer {
	run: u64,
	at: Instant,
	key: Option<(Instant, u64)>,
}

impl Timer {
	/// A timer for `at`, or `None` outside [`run`].
	pub(crate) fn new(at: Instant) -> Option<Self> {
		Some(Self {
			run: current()?.id,
			at,
			key: None,
		})
	}

	/// The run this timer belongs to, while it is still the thread's current one.
	fn runtime(&self) -> Option<Rc<Runtime>> {
		current().filter(|runtime| runtime.id == self.run)
	}

	pub(crate) fn reset(&mut self, at: Instant) {
		self.at = at;
		// Wake whoever is parked on the old deadline, so it re-arms for the new one;
		// the reset may come from another task.
		if let Some(waker) = self.disarm() {
			waker.wake();
		}
	}

	/// Remove the armed entry, returning the waker parked on it.
	fn disarm(&mut self) -> Option<Waker> {
		// A run that already ended cleared its timers.
		let key = self.key.take()?;
		self.runtime()?.time.borrow_mut().timers.remove(&key)
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		let runtime = self
			.runtime()
			.expect("a simulated sleep polled outside its kio::clock::sim::run");
		let mut time = runtime.time.borrow_mut();
		if time.clock.now() >= self.at {
			if let Some(key) = self.key.take() {
				time.timers.remove(&key);
			}
			return Poll::Ready(());
		}
		let key = *self.key.get_or_insert_with(|| {
			let id = time.next;
			time.next += 1;
			(self.at, id)
		});
		time.timers.insert(key, cx.waker().clone());
		Poll::Pending
	}
}

impl Drop for Timer {
	fn drop(&mut self) {
		self.disarm();
	}
}

/// A spawned task failed to complete: it panicked or was aborted.
pub struct JoinError(Option<Box<dyn std::any::Any + Send>>);

impl JoinError {
	/// Whether [`JoinHandle::abort`] cancelled the task.
	pub fn is_cancelled(&self) -> bool {
		self.0.is_none()
	}

	/// Whether the task panicked.
	pub fn is_panic(&self) -> bool {
		self.0.is_some()
	}
}

impl std::fmt::Debug for JoinError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(self, f)
	}
}

impl std::fmt::Display for JoinError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.0 {
			None => f.write_str("task was cancelled"),
			Some(panic) => match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
				(Some(message), _) => write!(f, "task panicked: {message}"),
				(_, Some(message)) => write!(f, "task panicked: {message}"),
				_ => f.write_str("task panicked"),
			},
		}
	}
}

impl std::error::Error for JoinError {}

struct Join<T> {
	output: Option<Result<T, JoinError>>,
	finished: bool,
	aborted: bool,
	detached: bool,
	task: Option<Waker>,
	joiner: Option<Waker>,
}

/// Await a spawned task's output. Dropping it detaches the task.
pub struct JoinHandle<T> {
	join: Rc<RefCell<Join<T>>>,
}

impl<T> JoinHandle<T> {
	/// Cancel the task: it is dropped before its next poll.
	pub fn abort(&self) {
		let mut join = self.join.borrow_mut();
		join.aborted = true;
		if let Some(task) = join.task.take() {
			task.wake();
		}
	}

	/// Whether the task has completed, panicked, or been cancelled.
	pub fn is_finished(&self) -> bool {
		self.join.borrow().finished
	}
}

impl<T> Future for JoinHandle<T> {
	type Output = Result<T, JoinError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let mut join = self.join.borrow_mut();
		if let Some(output) = join.output.take() {
			return Poll::Ready(output);
		}
		assert!(!join.finished, "JoinHandle polled after completion");
		join.joiner = Some(cx.waker().clone());
		Poll::Pending
	}
}

impl<T> Drop for JoinHandle<T> {
	fn drop(&mut self) {
		let output = {
			let mut join = self.join.borrow_mut();
			join.detached = true;
			join.output.take()
		};
		// A task that already panicked won't finish again to report it.
		if let Some(Err(JoinError(Some(panic)))) = output
			&& !std::thread::panicking()
		{
			std::panic::resume_unwind(panic);
		}
	}
}

/// Spawn a task on the running simulator.
///
/// A panic is kept for the [`JoinHandle`], or propagated out of [`run`] once the
/// handle is dropped, so a detached task cannot fail silently.
///
/// # Panics
///
/// Outside [`run`].
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
	F: Future + 'static,
	F::Output: 'static,
{
	let runtime = expect_current();
	let join = Rc::new(RefCell::new(Join {
		output: None,
		finished: false,
		aborted: false,
		detached: false,
		task: None,
		joiner: None,
	}));
	let state = Cancel(join.clone());
	let mut future = Some(Box::pin(future));
	// Every poll of a spawned task goes through here.
	let task = std::future::poll_fn(move |cx| {
		let finish = |output| {
			let mut join = state.0.borrow_mut();
			join.finished = true;
			if join.detached {
				if let Err(JoinError(Some(panic))) = output {
					std::panic::resume_unwind(panic);
				}
				return;
			}
			join.output = Some(output);
			if let Some(joiner) = join.joiner.take() {
				joiner.wake();
			}
		};

		if state.0.borrow().aborted {
			future = None;
			finish(Err(JoinError(None)));
			return Poll::Ready(());
		}
		state.0.borrow_mut().task = Some(cx.waker().clone());

		let running = future.as_mut().expect("task polled after completion");
		let running = std::future::poll_fn(|cx| poll(running.as_mut(), cx));
		let output = match std::panic::AssertUnwindSafe(running).catch_unwind().poll_unpin(cx) {
			Poll::Pending => return Poll::Pending,
			Poll::Ready(output) => output,
		};
		future = None;
		finish(output.map_err(|panic| JoinError(Some(panic))));
		Poll::Ready(())
	});
	runtime.spawned.borrow_mut().push(Box::pin(task));
	JoinHandle { join }
}

/// Owned by a spawned task, so dropping it unfinished, as [`run`] does with tasks
/// still pending when it returns, tells a handle that outlives the run it was cancelled.
struct Cancel<T>(Rc<RefCell<Join<T>>>);

impl<T> Drop for Cancel<T> {
	fn drop(&mut self) {
		let joiner = {
			let mut join = self.0.borrow_mut();
			if join.finished || join.detached {
				return;
			}
			join.finished = true;
			join.output = Some(Err(JoinError(None)));
			join.joiner.take()
		};
		if let Some(joiner) = joiner {
			joiner.wake();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::clock::{Deadline, Deadlines, Elapsed, MissedTickBehavior};

	fn spin() {
		let producer = crate::Producer::new(());
		let consumer = producer.consume();
		let waiter = crate::Waiter::noop();
		for _ in 0..10_000 {
			assert!(consumer.poll(&waiter, |_| Poll::Ready(())).is_ready());
		}
	}

	#[test]
	#[should_panic(expected = "kio Ready poll limit")]
	fn a_spinning_root_fails() {
		run(async { spin() });
	}

	#[test]
	#[should_panic(expected = "kio Ready poll limit")]
	fn a_spinning_spawned_task_fails() {
		run(async {
			drop(spawn(async { spin() }));
			yield_now().await;
		});
	}

	#[crate::test]
	async fn a_budgeted_loop_lets_a_sibling_run() {
		let turns = Rc::new(Cell::new(0));
		let sibling = spawn({
			let turns = turns.clone();
			async move {
				turns.set(turns.get() + 1);
			}
		});
		let producer = crate::Producer::new(());
		let consumer = producer.consume();
		let mut budget = crate::coop::Budget::new(32);
		for _ in 0..10_000 {
			crate::wait(|waiter| budget.poll_yield(waiter)).await;
			assert!(
				crate::wait(|waiter| consumer.poll(waiter, |_| Poll::Ready(())))
					.await
					.is_ok()
			);
		}
		assert_eq!(turns.get(), 1, "the sibling ran before the loop finished");
		sibling.await.unwrap();
	}

	#[crate::test]
	async fn pending_polls_do_not_count_as_work() {
		let producer = crate::Producer::new(());
		let consumer = producer.consume();
		let waiter = crate::Waiter::noop();
		for _ in 0..10_000 {
			assert!(consumer.poll(&waiter, |_| Poll::<()>::Pending).is_pending());
		}
	}

	#[test]
	fn a_failed_run_restores_poll_accounting() {
		assert!(std::panic::catch_unwind(|| run(async { spin() })).is_err());
		spin();
		run(async {});
	}

	#[crate::test]
	async fn a_large_task_set_counts_each_child_alone() {
		let producer = crate::Producer::new(());
		let mut tasks = crate::Tasks::new();
		for _ in 0..10_000 {
			let consumer = producer.consume();
			tasks.push(move |waiter: &crate::Waiter| consumer.poll(waiter, |_| Poll::Ready(())).map(drop));
		}
		crate::wait(|waiter| tasks.poll(waiter)).await;
	}

	#[test]
	#[should_panic(expected = "kio Ready poll limit")]
	fn a_spinning_task_set_child_fails() {
		run(async {
			let mut tasks = crate::Tasks::new();
			tasks.push(|_: &crate::Waiter| {
				spin();
				Poll::Ready(())
			});
			crate::wait(|waiter| tasks.poll(waiter)).await;
		});
	}

	#[test]
	fn a_task_set_outside_the_simulator_counts_nothing() {
		let mut tasks = crate::Tasks::new();
		tasks.push(|_: &crate::Waiter| {
			spin();
			Poll::Ready(())
		});
		assert!(tasks.poll(&crate::Waiter::noop()).is_ready());
	}

	fn at(secs: u64) -> Instant {
		Instant::ZERO + Duration::from_secs(secs)
	}

	#[test]
	fn into_future_sees_the_runtime_and_can_borrow() {
		struct Start<'a>(&'a Rc<str>);

		impl<'a> IntoFuture for Start<'a> {
			type Output = &'a Rc<str>;
			type IntoFuture = std::future::Ready<Self::Output>;

			fn into_future(self) -> Self::IntoFuture {
				assert!(is_running());
				std::future::ready(self.0)
			}
		}

		let value = Rc::<str>::from("borrowed");
		assert!(Rc::ptr_eq(run(Start(&value)), &value));
		assert!(!is_running());
	}

	#[test]
	fn the_root_future_is_dropped_after_leaving_the_runtime() {
		struct Dropped(Rc<Cell<Option<bool>>>);

		impl Drop for Dropped {
			fn drop(&mut self) {
				self.0.set(Some(is_running()));
			}
		}

		let dropped = Rc::new(Cell::new(None));
		let root = Dropped(dropped.clone());
		run(std::future::poll_fn(move |_| {
			let _root = &root;
			Poll::Ready(())
		}));
		assert_eq!(dropped.get(), Some(false));
	}

	#[crate::test]
	async fn starts_at_zero() {
		assert_eq!(crate::clock::now(), Instant::ZERO);
		assert_eq!(crate::clock::wall(), Manual::WALL_START);
	}

	#[test]
	fn idle_waits_for_every_timer() {
		run(async {
			let fired = Rc::new(Cell::new(0));
			for secs in [1, 5, 3] {
				let fired = fired.clone();
				drop(spawn(async move {
					crate::clock::sleep(Duration::from_secs(secs)).await;
					fired.set(fired.get() + 1);
				}));
			}
			idle().await;
			assert_eq!(fired.get(), 3);
			assert_eq!(crate::clock::now(), at(5));
		});
	}

	#[test]
	fn time_moves_only_when_every_task_stalls() {
		run(async {
			let a = spawn(async {
				crate::clock::sleep(Duration::from_secs(5)).await;
				crate::clock::now()
			});
			let b = spawn(async {
				crate::clock::sleep(Duration::from_secs(2)).await;
				crate::clock::now()
			});
			assert_eq!(crate::clock::now(), Instant::ZERO, "spawning does not move time");
			assert_eq!(b.await.unwrap(), at(2));
			assert_eq!(a.await.unwrap(), at(5));
			assert_eq!(
				crate::clock::wall(),
				Manual::WALL_START + Duration::from_secs(5),
				"wall time moved too"
			);
		});
	}

	#[crate::test]
	async fn timeout_uses_simulated_time() {
		let slow = crate::clock::timeout(Duration::from_secs(1), crate::clock::sleep(Duration::from_secs(60))).await;
		assert_eq!(slow.unwrap_err(), Elapsed);
		assert_eq!(crate::clock::now(), at(1));
		let fast = crate::clock::timeout(Duration::from_secs(60), async { 7 }).await;
		assert_eq!(fast, Ok(7));
	}

	#[crate::test]
	async fn timeout_at_ends_at_the_deadline() {
		let slow = crate::clock::timeout_at(at(2), crate::clock::sleep(Duration::from_secs(60))).await;
		assert_eq!(slow.unwrap_err(), Elapsed);
		assert_eq!(crate::clock::now(), at(2));
		let late = crate::clock::timeout_at(at(1), async { 7 }).await;
		assert_eq!(late, Ok(7), "a ready future wins even past the deadline");
	}

	#[crate::test]
	async fn advance_wakes_due_timers() {
		let timer = spawn(crate::clock::sleep(Duration::from_secs(3)));
		yield_now().await;
		advance(Duration::from_secs(3)).await;
		assert!(timer.is_finished());
		assert_eq!(crate::clock::now(), at(3));
	}

	#[crate::test]
	async fn a_wall_step_passes_no_time() {
		advance(Duration::from_secs(10)).await;
		step_wall(Manual::WALL_START + Duration::from_secs(4));
		assert_eq!(crate::clock::now(), at(10), "a step back passes no time");
		assert_eq!(crate::clock::wall(), Manual::WALL_START + Duration::from_secs(4));
		advance(Duration::from_secs(1)).await;
		assert_eq!(crate::clock::wall(), Manual::WALL_START + Duration::from_secs(5));
	}

	#[crate::test]
	async fn cancellation() {
		let fired = Rc::new(Cell::new(false));
		let task = spawn({
			let fired = fired.clone();
			async move {
				crate::clock::sleep(Duration::from_secs(1)).await;
				fired.set(true);
			}
		});
		yield_now().await;
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
		// Nothing else is armed: a dropped sleep left no timer behind, so time jumps here.
		crate::clock::sleep(Duration::from_secs(5)).await;
		assert!(!fired.get());
		assert_eq!(crate::clock::now(), at(5));
	}

	#[crate::test]
	async fn rearming() {
		let mut sleep = crate::clock::sleep(Duration::from_secs(1));
		sleep.reset(at(3));
		(&mut sleep).await;
		assert_eq!(crate::clock::now(), at(3), "the old instant is gone");
		sleep.reset(at(4));
		sleep.await;
		assert_eq!(crate::clock::now(), at(4), "a completed sleep re-arms");
	}

	#[crate::test]
	async fn an_expired_deadline_fires_at_once() {
		advance(Duration::from_secs(10)).await;
		crate::clock::sleep_until(at(5)).await;
		crate::clock::sleep_until(at(10)).await;
		crate::clock::sleep(Duration::ZERO).await;
		assert_eq!(crate::clock::now(), at(10));
	}

	#[crate::test]
	async fn equal_deadlines_fire_in_arming_order() {
		let order = Rc::new(RefCell::new(Vec::new()));
		// Created in one order; a sleep arms on its first poll, so spawning them in
		// the other order arms them in that order.
		let first = crate::clock::sleep_until(at(1));
		let second = crate::clock::sleep_until(at(1));
		let record = |name: &'static str, sleep: crate::clock::Sleep| {
			let order = order.clone();
			spawn(async move {
				sleep.await;
				order.borrow_mut().push(name);
			})
		};
		let a = record("second", second);
		let b = record("first", first);
		a.await.unwrap();
		b.await.unwrap();
		assert_eq!(*order.borrow(), ["second", "first"]);
	}

	#[crate::test]
	async fn interval_bursts_missed_ticks_by_default() {
		let mut ticks = crate::clock::interval(Duration::from_secs(1));
		assert_eq!(ticks.tick().await, at(0), "the first tick is immediate");
		advance(Duration::from_millis(3500)).await;
		assert_eq!(ticks.tick().await, at(1));
		assert_eq!(ticks.tick().await, at(2));
		assert_eq!(ticks.tick().await, at(3));
		assert_eq!(
			crate::clock::now(),
			Instant::ZERO + Duration::from_millis(3500),
			"caught up at once"
		);
		assert_eq!(ticks.tick().await, at(4));
		assert_eq!(crate::clock::now(), at(4));
	}

	#[crate::test]
	async fn interval_at_starts_at_its_first_tick() {
		let mut ticks = crate::clock::interval_at(at(2), Duration::from_secs(1));
		assert_eq!(ticks.tick().await, at(2));
		assert_eq!(crate::clock::now(), at(2), "nothing ticks before the start");
		assert_eq!(ticks.tick().await, at(3));
	}

	#[crate::test]
	async fn interval_delay_restarts_from_the_late_tick() {
		let mut ticks = crate::clock::interval(Duration::from_secs(1));
		ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
		ticks.tick().await;
		advance(Duration::from_millis(3500)).await;
		assert_eq!(ticks.tick().await, at(1));
		assert_eq!(ticks.tick().await, Instant::ZERO + Duration::from_millis(4500));
	}

	#[crate::test]
	async fn interval_skip_resumes_the_schedule() {
		let mut ticks = crate::clock::interval(Duration::from_secs(1));
		ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
		ticks.tick().await;
		advance(Duration::from_millis(3500)).await;
		assert_eq!(ticks.tick().await, at(1));
		assert_eq!(ticks.tick().await, at(4));
	}

	#[crate::test]
	async fn abort_cancels_a_task() {
		let task = spawn(std::future::pending::<()>());
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
	}

	#[crate::test]
	async fn a_panic_waits_in_the_handle() {
		let task = spawn(async { panic!("boom") });
		yield_now().await;
		assert!(task.is_finished());
		assert!(task.await.unwrap_err().is_panic());
	}

	#[test]
	#[should_panic(expected = "boom")]
	fn a_detached_panic_fails_the_run() {
		run(async {
			drop(spawn(async { panic!("boom") }));
			yield_now().await;
		});
	}

	#[test]
	#[should_panic(expected = "boom")]
	fn dropping_the_handle_of_a_panicked_task_fails_the_run() {
		run(async {
			let task = spawn(async { panic!("boom") });
			yield_now().await;
			assert!(task.is_finished());
			drop(task);
		});
	}

	#[crate::test]
	async fn a_self_waking_task_cannot_starve_the_root() {
		let done = Rc::new(Cell::new(false));
		let task = spawn({
			let done = done.clone();
			async move {
				while !done.get() {
					yield_now().await;
				}
			}
		});
		yield_now().await;
		done.set(true);
		task.await.unwrap();
	}

	#[test]
	#[should_panic(expected = "deadlock")]
	fn a_deadlock_panics() {
		run(std::future::pending::<()>());
	}

	/// A sleep past the clock's range is never armed, so waiting on it alone is a
	/// deadlock rather than a jump to the end of time.
	#[test]
	#[should_panic(expected = "deadlock")]
	fn a_sleep_past_the_range_never_fires() {
		run(async { crate::clock::sleep(Duration::MAX).await });
	}

	/// The last instant is a real deadline, not the never-firing overflow.
	#[crate::test]
	async fn the_last_instant_is_reachable() {
		// From the epoch, so the wall clock has room to reach it too.
		step_wall(Wall::UNIX_EPOCH);
		let task = spawn(crate::clock::sleep_until(Instant::MAX));
		yield_now().await;
		advance(Duration::from_nanos(u64::MAX)).await;
		task.await.unwrap();
	}

	/// A tick past the clock's range never comes, rather than firing at its end.
	#[test]
	#[should_panic(expected = "deadlock")]
	fn an_interval_past_the_range_ticks_once() {
		run(async {
			// From the epoch, so the wall clock could reach the last instant.
			step_wall(Wall::UNIX_EPOCH);
			let mut interval = crate::clock::interval(Duration::MAX);
			interval.tick().await;
			interval.tick().await;
		});
	}

	/// A queue attached behind the clock catches up before the run decides it stalled.
	#[crate::test]
	async fn a_queue_attached_behind_the_clock_wakes_at_once() {
		let deadlines = Deadlines::new(crate::clock::now());
		let mut deadline = Deadline::after(&deadlines, Duration::from_secs(5));
		advance(Duration::from_secs(10)).await;
		let task = spawn(async move { crate::wait(|waiter| deadline.poll(waiter)).await });
		yield_now().await;

		attach(&deadlines);
		task.await.unwrap();
		assert_eq!(crate::clock::now(), at(10));
	}

	/// Attaching brings the queue to the simulated instant at once, so a deadline
	/// armed right after counts from it.
	#[crate::test]
	async fn attaching_syncs_the_queue() {
		let deadlines = Deadlines::new(crate::clock::now());
		advance(Duration::from_secs(10)).await;
		attach(&deadlines);
		assert_eq!(deadlines.now(), at(10));
	}

	#[test]
	#[should_panic(expected = "time moved backwards")]
	fn attaching_a_queue_ahead_of_the_clock_panics() {
		run(async { attach(&Deadlines::new(at(1))) });
	}

	/// A pending sleep reset by another task wakes its poller to re-arm.
	#[crate::test]
	async fn a_sleep_reset_by_another_task_rearms() {
		// Armed, and past the clock's range so never firing.
		for duration in [Duration::from_secs(10), Duration::MAX] {
			let sleep = Rc::new(RefCell::new(crate::clock::sleep(duration)));
			let polled = sleep.clone();
			let task = spawn(std::future::poll_fn(move |cx| {
				Pin::new(&mut *polled.borrow_mut()).poll(cx)
			}));
			yield_now().await;
			let target = crate::clock::now() + Duration::from_secs(3);
			sleep.borrow_mut().reset(target);
			task.await.unwrap();
			assert_eq!(crate::clock::now(), target);
		}
	}

	/// Keeping a queue in step leaves its parked owner registered, so a deadline
	/// armed afterwards still wakes it.
	#[crate::test]
	async fn keeping_a_queue_in_step_leaves_its_owner_parked() {
		struct Count(std::sync::atomic::AtomicUsize);
		impl Wake for Count {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}
		let count = Arc::new(Count(Default::default()));
		let owner = crate::Waiter::new(Waker::from(count.clone()));

		let deadlines = Deadlines::new(crate::clock::now());
		attach(&deadlines);
		deadlines.advance(crate::clock::now());
		deadlines.park(&owner);
		crate::clock::sleep(Duration::from_secs(1)).await;

		let mut deadline = Deadline::new(&deadlines);
		deadline.set(Some(crate::clock::now() + Duration::from_secs(1)));
		assert_eq!(count.0.load(Ordering::SeqCst), 1, "the parked owner was not woken");
	}

	/// The run holds an attached queue weakly and forgets it once its owner drops it.
	#[crate::test]
	async fn an_attached_queue_detaches_once_dropped() {
		let deadlines = Deadlines::new(crate::clock::now());
		attach(&deadlines);
		drop(deadlines);
		crate::clock::sleep(Duration::from_secs(1)).await;
		assert!(expect_current().queues.borrow().is_empty());
	}

	/// A run drops its pending tasks, so a handle that escapes it reports the task
	/// cancelled instead of pending forever.
	#[test]
	#[expect(clippy::async_yields_async, reason = "the handle escapes the run on purpose")]
	fn a_handle_outliving_its_run_is_cancelled() {
		let handle = run(async { spawn(std::future::pending::<()>()) });
		assert!(run(handle).unwrap_err().is_cancelled());
	}

	/// A sleep answers to the clock it was created on, not to the poller's.
	#[test]
	#[should_panic(expected = "polled outside its kio::clock::sim::run")]
	#[expect(clippy::async_yields_async, reason = "the sleep escapes the run on purpose")]
	fn a_sleep_polled_in_another_run_panics() {
		let mut sleep = run(async { crate::clock::sleep(Duration::from_secs(1)) });
		run(async move {
			advance(Duration::from_secs(2)).await;
			(&mut sleep).await;
		});
	}

	#[crate::test]
	async fn attached_deadlines_advance_with_time() {
		let deadlines = Deadlines::new(crate::clock::now());
		attach(&deadlines);
		let mut deadline = Deadline::after(&deadlines, Duration::from_secs(4));
		// Nothing else is armed, so the stall jumps straight to the queue's deadline.
		crate::wait(|waiter| deadline.poll(waiter)).await;
		assert_eq!(crate::clock::now(), at(4));
	}

	#[crate::test]
	async fn drive_polls_at_each_requested_instant() {
		let mut polls = Vec::new();
		let err = crate::clock::drive(|now, _waiter| {
			polls.push(now);
			match polls.len() {
				3 => Err("done"),
				n => Ok(Some(now + Duration::from_secs(n as u64))),
			}
		})
		.await;
		assert_eq!(err, "done");
		assert_eq!(polls, [at(0), at(1), at(3)]);
	}
}

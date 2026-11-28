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
//!
//! Each run seeds [`rng`](crate::clock::rng) and [`seed`](crate::clock::seed) from `KIO_SIM_SEED`,
//! or at random when it is unset, and a run that panics prints its seed and schedule settings,
//! so setting all of them as printed replays the run bit-exact.
//!
//! Experimental: `KIO_SIM_SCHEDULE` picks the order ready tasks run in (wake order by
//! default, or `random` or `pct`, drawn from the seed; a [`Tasks`](crate::Tasks) set keeps
//! its own slot order), and `KIO_SIM_PROBE=1` hunts lost wakeups: whenever every task has
//! stalled, each task and each task set child is polled once more before time moves, and
//! one that makes progress (completes, wakes or spawns a task, arms a timer, attaches an
//! armed queue, or publishes to a channel) panics with where it was spawned or pushed,
//! since nothing woke it. The probe needs every wake to come from the run's thread: one
//! from another thread lands at a moment no seed replays, so it fails the run.

use std::{
	cell::{Cell, RefCell},
	collections::{BTreeMap, BTreeSet, VecDeque},
	future::{Future, IntoFuture},
	panic::Location,
	pin::{Pin, pin},
	rc::Rc,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
	time::Duration,
};

use futures::FutureExt;
use rand::SeedableRng;

mod schedule;

use schedule::{Config, Priority, Scheduler};

use crate::clock::{
	Deadlines, Instant, Manual, Wall,
	deadline::WeakDeadlines,
	manual::{Key, Timers},
};

thread_local! {
	static CURRENT: RefCell<Option<Rc<Runtime>>> = const { RefCell::new(None) };
	/// Counts what makes progress visible to the run: wakes, spawns, armed timers, and
	/// channel publishes. A probe compares it across a re-poll.
	static EVENTS: Cell<u64> = const { Cell::new(0) };
	/// Set while a probe re-polls parked tasks, so a task set polls every child.
	static PROBING: Cell<bool> = const { Cell::new(false) };
	/// The first child a probe saw make progress, with where it was pushed.
	static BLAMED: Cell<Option<(&'static Location<'static>, usize)>> = const { Cell::new(None) };
	/// Set when [`yield_now`] parks the task being polled, which then runs behind every
	/// task already ready. Any other wake it makes of itself keeps its place in wake order.
	static YIELDED: Cell<bool> = const { Cell::new(false) };
}

/// Count an event a probe watches for.
#[inline]
pub(crate) fn event() {
	// A wake from a thread-local's destructor counts nothing.
	let _ = EVENTS.try_with(|events| events.set(events.get().wrapping_add(1)));
}

/// Whether a probe is re-polling parked tasks, which must not count a budget's yield.
pub(crate) fn probing() -> bool {
	PROBING.get()
}

/// The probe's event count, to compare across a poll.
pub(crate) fn events() -> u64 {
	EVENTS.get()
}

/// Blame the [`Tasks`](crate::Tasks) child in `slot`, pushed at `location`, for progress
/// the probe saw, unless an earlier one already is.
pub(crate) fn blame(location: &'static Location<'static>, slot: usize) {
	if BLAMED.get().is_none() {
		BLAMED.set(Some((location, slot)));
	}
}

type Task = Pin<Box<dyn Future<Output = ()>>>;

fn poll<F: Future + ?Sized>(future: Pin<&mut F>, cx: &mut Context<'_>) -> Poll<F::Output> {
	let _scope = crate::coop::scope();
	future.poll(cx)
}

struct Runtime {
	/// Tells this run from earlier ones on the thread, so a timer finds its own.
	id: u64,
	/// Spawned since the run loop last collected them, with where.
	spawned: RefCell<Vec<(usize, Spawned)>>,
	/// The id the next spawned task takes.
	next: Cell<usize>,
	/// The tasks woken since they last ran, in wake order.
	ready: Arc<Ready>,
	schedule: RefCell<Scheduler>,
	/// Each live task's priority, under `pct`.
	priorities: RefCell<BTreeMap<usize, Priority>>,
	probe: bool,
	time: RefCell<Timers>,
	/// Queues kept in step with simulated time, until their owners drop them.
	queues: RefCell<Vec<WeakDeadlines>>,
	/// Awaiting [`idle`], each settled and woken once nothing else can run; held weakly.
	idle: RefCell<Vec<std::rc::Weak<Idle>>>,
	seed: u64,
	rng: RefCell<rand::rngs::StdRng>,
}

impl Runtime {
	fn enter(seed: u64, config: Config) -> Rc<Self> {
		static RUNS: AtomicU64 = AtomicU64::new(0);
		let runtime = Rc::new(Runtime {
			id: RUNS.fetch_add(1, Ordering::Relaxed),
			spawned: RefCell::new(Vec::new()),
			next: Cell::new(ROOT + 1),
			ready: Arc::new(Ready {
				queue: Mutex::default(),
				thread: config.probe.then(|| std::thread::current().id()),
				foreign: AtomicBool::new(false),
			}),
			schedule: RefCell::new(Scheduler::new(config.policy, seed)),
			priorities: RefCell::default(),
			probe: config.probe,
			time: RefCell::new(Timers::new(Manual::new())),
			queues: RefCell::new(Vec::new()),
			idle: RefCell::new(Vec::new()),
			seed,
			rng: RefCell::new(rand::rngs::StdRng::seed_from_u64(seed)),
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
		self.time.borrow_mut().clear();
	}

	fn now(&self) -> Instant {
		self.time.borrow().clock.now()
	}

	/// Move time to `at` (never backwards), waking every timer and queued deadline due by then.
	fn advance_to(&self, at: Instant) {
		let due = {
			let mut time = self.time.borrow_mut();
			let passed = at.saturating_duration_since(time.clock.now());
			time.advance(passed)
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
		let timer = self.time.borrow().next();
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
	current().map(|runtime| runtime.time.borrow_mut().anchor())
}

/// The run's seed, when the calling thread is inside [`run`].
pub(crate) fn seed() -> Option<u64> {
	current().map(|runtime| runtime.seed)
}

/// The run's generator behind [`Rng`](crate::clock::Rng).
///
/// Holds plain data rather than the runtime, like [`Timer`], so it finds its run as the
/// thread's current one.
pub(crate) struct Rng {
	run: u64,
}

impl Rng {
	/// The current run's generator, or `None` outside [`run`].
	pub(crate) fn new() -> Option<Self> {
		Some(Self { run: current()?.id })
	}

	pub(crate) fn with<T>(&self, f: impl FnOnce(&mut rand::rngs::StdRng) -> T) -> T {
		let runtime = current()
			.filter(|runtime| runtime.id == self.run)
			.expect("a simulated rng used outside its kio::clock::sim::run");
		f(&mut runtime.rng.borrow_mut())
	}
}

/// The seed `KIO_SIM_SEED` names, or a random one when it is unset.
fn seed_from_env() -> u64 {
	match std::env::var("KIO_SIM_SEED") {
		Ok(seed) => seed
			.parse()
			.unwrap_or_else(|_| panic!("KIO_SIM_SEED must be a u64, not {seed:?}")),
		Err(std::env::VarError::NotPresent) => rand::random(),
		Err(err) => panic!("KIO_SIM_SEED: {err}"),
	}
}

/// The run the calling thread is inside, if any.
pub(crate) fn run_id() -> Option<u64> {
	current().map(|runtime| runtime.id)
}

/// Run `id`, which the calling thread must be inside.
fn expect_run(id: u64) -> Rc<Runtime> {
	current()
		.filter(|runtime| runtime.id == id)
		.expect("a clock captured in a kio::clock::sim::run read outside it")
}

/// The simulated instant of run `id`.
pub(crate) fn now_in(id: u64) -> Instant {
	expect_run(id).now()
}

/// The simulated wall time of run `id`.
pub(crate) fn wall_in(id: u64) -> Wall {
	expect_run(id).time.borrow().clock.wall()
}

/// How long run `id` has spent suspended.
pub(crate) fn suspended_in(id: u64) -> Duration {
	expect_run(id).time.borrow().clock.suspended()
}

/// Whether the calling thread is inside [`run`].
pub fn is_running() -> bool {
	CURRENT.with(|current| current.borrow().is_some())
}

/// The root future's id; spawned tasks count up from it.
const ROOT: usize = 0;

/// The tasks woken since they last ran, each once, in wake order.
struct Ready {
	queue: Mutex<VecDeque<usize>>,
	/// The run's thread, while the probe runs: a wake from any other lands at a moment
	/// the probe cannot tell from a lost wakeup.
	thread: Option<std::thread::ThreadId>,
	/// Set by a wake from another thread while probing, for the run's thread to fail on.
	foreign: AtomicBool,
}

impl Ready {
	fn is_empty(&self) -> bool {
		self.queue.lock().unwrap().is_empty()
	}

	/// Fail the run on a wake that came from another thread while probing.
	fn check(&self) {
		assert!(
			!self.foreign.load(Ordering::Acquire),
			"KIO_SIM_PROBE=1 saw a wake from a thread other than the run's: it lands at a host-timing \
			 point no seed replays, which the probe cannot tell from a lost wakeup, so probe only runs \
			 whose wakes all come from the run's thread"
		);
	}
}

/// Queues its task on the run's ready list.
struct TaskWaker {
	id: usize,
	/// Set while the task is on the list, so a burst of wakes queues it once.
	queued: AtomicBool,
	/// Set once the task finished or its run ended, so a stale wake neither queues it nor
	/// counts as progress.
	retired: AtomicBool,
	ready: Arc<Ready>,
}

impl TaskWaker {
	fn new(id: usize, ready: &Arc<Ready>) -> Arc<Self> {
		Arc::new(Self {
			id,
			queued: AtomicBool::new(false),
			retired: AtomicBool::new(false),
			ready: ready.clone(),
		})
	}
}

impl Wake for TaskWaker {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		if self.retired.load(Ordering::Acquire) {
			return;
		}
		event();
		if self
			.ready
			.thread
			.is_some_and(|thread| thread != std::thread::current().id())
		{
			self.ready.foreign.store(true, Ordering::Release);
		}
		if !self.queued.swap(true, Ordering::AcqRel) {
			self.ready.queue.lock().unwrap().push_back(self.id);
		}
	}
}

/// Clears the thread's runtime when [`run`] returns or unwinds, printing the settings
/// that replay an unwinding run when [`run`] seeded it.
struct Enter {
	replay: Option<String>,
}

impl Drop for Enter {
	fn drop(&mut self) {
		CURRENT.with(|current| current.borrow_mut().take());
		if let Some(replay) = &self.replay
			&& std::thread::panicking()
		{
			eprintln!("kio::clock::sim run panicked; replay it with {replay}");
		}
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
/// exceeds the ready kio poll limit, a detached task panics, `KIO_SIM_SEED` is
/// not a `u64`, or `KIO_SIM_SCHEDULE` or `KIO_SIM_PROBE` is malformed, and on a lost
/// wakeup while probing.
#[track_caller]
pub fn run<F: IntoFuture>(future: F) -> F::Output {
	let seed = seed_from_env();
	enter(seed, Some(seed), Config::from_env(), future)
}

/// [`run`] with [`rng`](crate::clock::rng) and [`seed`](crate::clock::seed) drawn from
/// `seed`, ignoring `KIO_SIM_SEED`: for a sweep that derives each case's seed itself,
/// so one setting replays a case.
///
/// # Panics
///
/// As [`run`] does, except on `KIO_SIM_SEED`.
#[track_caller]
pub fn run_seeded<F: IntoFuture>(seed: u64, future: F) -> F::Output {
	enter(seed, None, Config::from_env(), future)
}

/// [`run_seeded`] scheduled by `config` rather than the environment.
#[cfg(test)]
#[track_caller]
fn run_with<F: IntoFuture>(seed: u64, config: Config, future: F) -> F::Output {
	enter(seed, None, config, future)
}

#[track_caller]
fn enter<F: IntoFuture>(seed: u64, replay: Option<u64>, config: Config, future: F) -> F::Output {
	let location = Location::caller();
	let replay = replay.map(|seed| format!("KIO_SIM_SEED={seed}{config}"));
	let runtime = Runtime::enter(seed, config);
	let enter = Enter { replay };

	let mut future = pin!(future.into_future());
	let mut output = None;
	{
		// Keep the caller's future and output on its stack while sharing the executor.
		let future = pin!(std::future::poll_fn(|cx| {
			future.as_mut().poll(cx).map(|value| output = Some(value))
		}));
		run_inner(&runtime, future, location);
	}
	runtime.exit(enter);
	output.expect("root future completed")
}

/// A spawned task the run loop owns.
struct Spawned {
	task: Task,
	queue: Arc<TaskWaker>,
	waker: Waker,
	location: &'static Location<'static>,
}

/// A task that finished, or whose run ended, retires its waker.
impl Drop for Spawned {
	fn drop(&mut self) {
		self.queue.retired.store(true, Ordering::Release);
	}
}

/// The root future, which borrows the caller's stack, kept apart from the spawned tasks.
struct Root<'a> {
	future: Pin<&'a mut dyn Future<Output = ()>>,
	queue: Arc<TaskWaker>,
	waker: Waker,
	location: &'static Location<'static>,
}

/// A later run may still hold the root's waker.
impl Drop for Root<'_> {
	fn drop(&mut self) {
		self.queue.retired.store(true, Ordering::Release);
	}
}

impl Root<'_> {
	/// Poll task `id`, returning whether it finished and whether it woke itself.
	fn poll(&mut self, spawned: &mut BTreeMap<usize, Spawned>, id: usize) -> (bool, bool) {
		if id == ROOT {
			self.queue.queued.store(false, Ordering::Release);
			let done = poll(self.future.as_mut(), &mut Context::from_waker(&self.waker)).is_ready();
			return (done, self.queue.queued.load(Ordering::Acquire));
		}
		let task = spawned.get_mut(&id).expect("a live task");
		task.queue.queued.store(false, Ordering::Release);
		// The spawned task's own wrapper opens its poll accounting scope.
		let done = task
			.task
			.as_mut()
			.poll(&mut Context::from_waker(&task.waker))
			.is_ready();
		(done, task.queue.queued.load(Ordering::Acquire))
	}
}

fn run_inner(runtime: &Runtime, future: Pin<&mut dyn Future<Output = ()>>, location: &'static Location<'static>) {
	let queue = TaskWaker::new(ROOT, &runtime.ready);
	let mut root = Root {
		future,
		waker: Waker::from(queue.clone()),
		queue,
		location,
	};
	// In id order, so a run that ends drops what is still pending in the same order on replay.
	let mut spawned = BTreeMap::new();
	// Tasks that called `yield_now` in their last poll, which wait behind every task ready ahead of them.
	let mut yielded = BTreeSet::new();
	let fifo = runtime.schedule.borrow().is_fifo();
	if let Some(priority) = runtime.schedule.borrow_mut().priority() {
		runtime.priorities.borrow_mut().insert(ROOT, priority);
	}
	root.waker.wake_by_ref();

	loop {
		// Already queued: a spawn wakes its task where it happens in the spawner's poll.
		spawned.extend(runtime.spawned.take());

		runtime.ready.check();
		let picked = {
			let mut ready = runtime.ready.queue.lock().unwrap();
			loop {
				let index = match (fifo, ready.is_empty()) {
					(_, true) => break None,
					(true, false) => 0,
					(false, false) => {
						// A task that yielded waits for every task queued ahead of it, as
						// `yield_now` promises, whatever the schedule.
						let eligible = match yielded.is_empty() {
							true => ready.len(),
							false => match ready.iter().position(|id| yielded.contains(id)) {
								Some(0) => {
									yielded.remove(&ready[0]);
									1
								}
								Some(at) => at,
								None => ready.len(),
							},
						};
						let priorities = runtime.priorities.borrow();
						let priority = |at: usize| priorities.get(&ready[at]).copied().unwrap_or(0);
						runtime.schedule.borrow_mut().pick(eligible, priority)
					}
				};
				let id = ready.remove(index).expect("picked a queued task");
				// A task that finished while queued names nothing to run.
				if id == ROOT || spawned.contains_key(&id) {
					break Some(id);
				}
			}
		};
		if let Some(id) = picked {
			YIELDED.set(false);
			let (done, woke) = root.poll(&mut spawned, id);
			let yields = woke && YIELDED.take();
			if done && id == ROOT {
				// The root may have joined a thread that woke a task on its way out.
				runtime.ready.check();
				break;
			}
			if done {
				retire(runtime, &mut spawned, &mut yielded, id);
				continue;
			}
			if yields {
				// Its wake may have coalesced into a place it already held near the front:
				// it goes behind every task that was ready, as a yield promises.
				let mut ready = runtime.ready.queue.lock().unwrap();
				if let Some(at) = ready.iter().position(|&queued| queued == id) {
					ready.remove(at);
					ready.push_back(id);
				}
				yielded.insert(id);
			}
			if let Some(priority) = runtime.priorities.borrow_mut().get_mut(&id) {
				runtime.schedule.borrow_mut().ran(priority, woke);
			}
			continue;
		}

		// Bring queues to the current instant before deciding to stall: one attached
		// behind the clock may wake a task without time moving.
		let mut queued = runtime.advance_queues();
		if !runtime.ready.is_empty() || !runtime.spawned.borrow().is_empty() {
			continue;
		}
		if runtime.probe {
			probe(runtime, &mut root, &mut spawned);
			// A probed task may have attached a queue, which wakes nothing.
			queued = runtime.advance_queues();
		}
		if runtime.advance(queued) {
			continue;
		}
		let idle: Vec<Rc<Idle>> = runtime.idle.take().iter().filter_map(std::rc::Weak::upgrade).collect();
		assert!(!idle.is_empty(), "deadlock: every task is parked and no timer is armed");
		for idle in idle {
			idle.settled.set(true);
			if let Some(waker) = idle.waker.take() {
				waker.wake();
			}
		}
	}

	drop(spawned);
	drop(runtime.spawned.take());
}

/// Drop a spawned task that finished, retiring its waker.
fn retire(runtime: &Runtime, spawned: &mut BTreeMap<usize, Spawned>, yielded: &mut BTreeSet<usize>, id: usize) {
	spawned.remove(&id);
	yielded.remove(&id);
	// Only `pct` ranks tasks, and it counts a finishing step too.
	if let Some(mut priority) = runtime.priorities.borrow_mut().remove(&id) {
		runtime.schedule.borrow_mut().ran(&mut priority, false);
	}
}

/// Poll every parked task once more, panicking at the first that makes progress: nothing
/// woke it, so without the probe it would sleep through the change it missed.
///
/// A task set polls every child while probing, and blames the first that progresses.
fn probe(runtime: &Runtime, root: &mut Root<'_>, spawned: &mut BTreeMap<usize, Spawned>) {
	struct Probing;
	impl Drop for Probing {
		fn drop(&mut self) {
			PROBING.set(false);
			BLAMED.set(None);
		}
	}
	PROBING.set(true);
	let _probing = Probing;

	let ids: Vec<usize> = std::iter::once(ROOT).chain(spawned.keys().copied()).collect();
	for id in ids {
		let before = events();
		BLAMED.set(None);
		let (done, _) = root.poll(spawned, id);
		runtime.ready.check();
		if !done && events() == before && BLAMED.get().is_none() {
			continue;
		}
		let task = match id {
			ROOT => format!("the root future run at {}", root.location),
			id => format!("the task spawned at {}", spawned[&id].location),
		};
		let child = BLAMED
			.get()
			.map(|(at, slot)| format!(", in its kio::Tasks child in slot {slot} pushed at {at},"))
			.unwrap_or_default();
		panic!("lost wakeup: {task}{child} made progress when polled again at a stall, though nothing woke it");
	}
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
	let mut queues = runtime.queues.borrow_mut();
	if queues.iter().any(|queue| queue.is(deadlines)) {
		return;
	}
	// An armed deadline joins the run as a timer would, so the probe counts it.
	if deadlines.next().is_some() {
		event();
	}
	queues.push(WeakDeadlines::new(deadlines));
}

/// Let `duration` of simulated time pass, moving both the instant and the wall
/// time and waking everything due, then yield.
///
/// # Panics
///
/// Outside [`run`].
pub async fn advance(duration: Duration) {
	let runtime = expect_current();
	let at = runtime.now().saturating_add(duration);
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

/// Run the simulated wall clock `ppm` parts per million fast of the instant from now on, slow
/// when negative, as a host's oscillator runs off the UTC its wall clock follows.
///
/// # Panics
///
/// Outside [`run`].
pub fn set_drift(ppm: i32) {
	expect_current().time.borrow_mut().clock.set_drift(ppm);
}

/// Suspend the simulated host for `duration`: the wall time moves and the instant does not, so no
/// timer fires, as a monotonic clock stops while the host sleeps. A captured
/// [`Clock`](crate::clock::Clock) reports it as [`suspended`](crate::clock::Clock::suspended).
///
/// # Panics
///
/// Outside [`run`].
pub fn suspend(duration: Duration) {
	expect_current().time.borrow_mut().clock.suspend(duration);
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
	let idle = Rc::new(Idle {
		settled: Cell::new(false),
		waker: RefCell::new(None),
	});
	std::future::poll_fn(move |cx| {
		if idle.settled.get() {
			return Poll::Ready(());
		}
		// Registered once, its waker kept current, so a probe's re-polls add nothing;
		// the run holds it weakly, so a dropped future awaits nothing. The first
		// registration is progress, as arming a timer is: it changes how the run ends.
		if idle.waker.replace(Some(cx.waker().clone())).is_none() {
			event();
			runtime.idle.borrow_mut().push(Rc::downgrade(&idle));
		}
		Poll::Pending
	})
}

/// An [`idle`] future's registration with its run.
struct Idle {
	settled: Cell<bool>,
	waker: RefCell<Option<Waker>>,
}

/// Yield once, letting every other ready task run first.
pub fn yield_now() -> impl Future<Output = ()> {
	let mut yielded = false;
	std::future::poll_fn(move |cx| {
		if yielded {
			return Poll::Ready(());
		}
		yielded = true;
		YIELDED.set(true);
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
	key: Option<Key>,
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
		// The same deadline keeps its entry and wakes nobody, like tokio's
		// `Sleep::reset`, so a task resetting on every poll still parks.
		if at == self.at && self.key.is_some() {
			return;
		}
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
		self.runtime()?.time.borrow_mut().disarm(key)
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		let runtime = self
			.runtime()
			.expect("a simulated sleep polled outside its kio::clock::sim::run");
		runtime.time.borrow_mut().poll(self.at, &mut self.key, cx.waker())
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
#[track_caller]
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
	let id = runtime.next.get();
	runtime.next.set(id + 1);
	if let Some(priority) = runtime.schedule.borrow_mut().priority() {
		runtime.priorities.borrow_mut().insert(id, priority);
	}
	let queue = TaskWaker::new(id, &runtime.ready);
	let task = Spawned {
		task: Box::pin(task),
		waker: Waker::from(queue.clone()),
		queue,
		location: Location::caller(),
	};
	// Queued here, so it runs ahead of whatever the spawner wakes next.
	task.waker.wake_by_ref();
	runtime.spawned.borrow_mut().push((id, task));
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
	use std::collections::HashSet;

	use crate::clock::{Deadline, Deadlines, Elapsed, MissedTickBehavior};

	fn spin() {
		let producer = crate::Producer::new(());
		let consumer = producer.consume();
		let waiter = crate::Waiter::noop();
		for _ in 0..10_000 {
			assert!(consumer.poll(&waiter, |_| Poll::Ready(())).is_ready());
		}
	}

	/// Run `future` in wake order without probing, for a test that counts polls or
	/// observes the order tasks run in, whatever the environment asks for.
	fn fifo<F: IntoFuture>(future: F) -> F::Output {
		run_with(0, Config::default(), future)
	}

	/// A task resetting its sleep to the same deadline on every poll still parks
	/// until that deadline, rather than waking itself and spinning.
	#[test]
	fn a_same_deadline_reset_does_not_wake() {
		fifo(async {
			let at = crate::clock::now().saturating_add(Duration::from_secs(1));
			let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(1)));
			let mut polls = 0;
			std::future::poll_fn(|cx| {
				polls += 1;
				assert!(polls <= 3, "the sleep woke its task without time passing");
				// The first poll arms the sleep and wakes once, so the next poll
				// resets an armed entry before the deadline.
				if polls == 1 {
					cx.waker().wake_by_ref();
				} else {
					sleep.as_mut().reset(at);
				}
				sleep.as_mut().poll(cx)
			})
			.await;
			assert_eq!(crate::clock::now(), at);
		});
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

	/// One child more than the limit, so counting the set as one poll would trip it.
	#[crate::test]
	async fn a_large_task_set_counts_each_child_alone() {
		let producer = crate::Producer::new(());
		let mut tasks = crate::Tasks::new();
		for _ in 0..=crate::coop::READY_LIMIT {
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
		Instant::ZERO.saturating_add(Duration::from_secs(secs))
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

	/// The origin is fixed at its first read: a wall step or drift moves the wall time,
	/// never the origin.
	#[crate::test]
	async fn the_origin_holds_through_wall_corrections() {
		advance(Duration::from_secs(10)).await;
		let origin = crate::clock::origin();
		assert_eq!(origin, (at(10), Manual::WALL_START + Duration::from_secs(10)));

		step_wall(Manual::WALL_START);
		set_drift(1_000);
		advance(Duration::from_secs(1)).await;
		assert_eq!(
			crate::clock::wall(),
			Manual::WALL_START + Duration::from_millis(1_001),
			"the drift skews the wall time"
		);
		assert_eq!(crate::clock::origin(), origin);
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

	/// Observed through the order the woken tasks run in, so in wake order.
	#[test]
	fn equal_deadlines_fire_in_arming_order() {
		fifo(async {
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
		});
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
			Instant::ZERO.saturating_add(Duration::from_millis(3500)),
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
		assert_eq!(
			ticks.tick().await,
			Instant::ZERO.saturating_add(Duration::from_millis(4500))
		);
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
			let target = crate::clock::now().saturating_add(Duration::from_secs(3));
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
		deadline.set(Some(crate::clock::now().saturating_add(Duration::from_secs(1))));
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

	/// The machine counts its polls, so a probe's extra poll would move it.
	#[test]
	fn drive_polls_at_each_requested_instant() {
		fifo(async {
			let mut polls = Vec::new();
			let err = crate::clock::drive(|now, _waiter| {
				polls.push(now);
				match polls.len() {
					3 => Err("done"),
					n => Ok(Some(now.saturating_add(Duration::from_secs(n as u64)))),
				}
			})
			.await;
			assert_eq!(err, "done");
			assert_eq!(polls, [at(0), at(1), at(3)]);
		});
	}

	fn probing() -> Config {
		Config {
			probe: true,
			..Config::default()
		}
	}

	fn random() -> Config {
		Config {
			policy: schedule::Policy::Random,
			probe: false,
		}
	}

	fn pct() -> Config {
		Config {
			policy: schedule::Policy::Pct { depth: 3, steps: 20 },
			probe: false,
		}
	}

	/// The message `f` panics with, for a report naming a location in this file, which
	/// `#[should_panic]` cannot spell: the path separator is the host's.
	fn panic_message(f: impl FnOnce()) -> String {
		let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("expected a panic");
		*panic.downcast::<String>().expect("a formatted panic message")
	}

	/// A task that a flag lets finish, though raising the flag wakes nobody: only a
	/// timer at 5 s, which hides the lost wakeup, wakes it.
	fn unwoken() -> impl FnMut(&crate::Waiter) -> Poll<()> + use<> {
		let flag = Rc::new(Cell::new(false));
		let raise = flag.clone();
		let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(5)));
		let mut raised = false;
		move |waiter| {
			if !std::mem::replace(&mut raised, true) {
				// Raised once the first poll has parked, with nobody woken.
				drop(spawn({
					let raise = raise.clone();
					async move { raise.set(true) }
				}));
			}
			match flag.get() {
				true => Poll::Ready(()),
				false => waiter.poll_future(sleep.as_mut()),
			}
		}
	}

	/// Without the probe, the timer hides the lost wakeup.
	#[test]
	fn a_masked_lost_wakeup_passes_unprobed() {
		fifo(async {
			spawn(crate::wait(unwoken())).await.unwrap();
			assert_eq!(crate::clock::now(), at(5), "only the timer woke it");
		});
	}

	#[test]
	fn the_probe_catches_a_lost_wakeup_a_timer_masks() {
		let message = panic_message(|| {
			run_with(0, probing(), async {
				spawn(crate::wait(unwoken())).await.unwrap();
			});
		});
		assert!(
			message.contains(&format!("lost wakeup: the task spawned at {}:", file!())),
			"{message}"
		);
	}

	/// Likewise a wake of an earlier run's root, which retired when its run ended.
	#[test]
	fn the_probe_ignores_an_earlier_runs_root_wake() {
		let old = fifo(std::future::poll_fn(|cx| Poll::Ready(cx.waker().clone())));
		run_with(0, probing(), async {
			let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(1)));
			crate::wait(|waiter| {
				old.wake_by_ref();
				waiter.poll_future(sleep.as_mut())
			})
			.await;
		});
	}

	/// A wake of a finished task names nothing to run, so the probe does not count it.
	#[test]
	fn the_probe_ignores_a_finished_task_wake() {
		run_with(0, probing(), async {
			let stale: Rc<RefCell<Option<std::task::Waker>>> = Rc::default();
			spawn({
				let stale = stale.clone();
				std::future::poll_fn(move |cx| {
					*stale.borrow_mut() = Some(cx.waker().clone());
					Poll::Ready(())
				})
			})
			.await
			.unwrap();
			let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(1)));
			crate::wait(|waiter| {
				stale.borrow().iter().for_each(std::task::Waker::wake_by_ref);
				waiter.poll_future(sleep.as_mut())
			})
			.await;
		});
	}

	/// A queue only the probe's re-poll attaches, holding an armed deadline, would have
	/// left the run deadlocked without the probe: the probe reports it.
	#[test]
	fn the_probe_reports_a_queue_attached_on_its_re_poll() {
		let message = panic_message(|| {
			run_with(0, probing(), async {
				let deadlines = Deadlines::new(crate::clock::now());
				let mut deadline = crate::clock::Deadline::after(&deadlines, Duration::from_secs(1));
				let mut polls = 0;
				crate::wait(|waiter| {
					polls += 1;
					if polls == 2 {
						attach(&deadlines);
					}
					deadline.poll(waiter)
				})
				.await;
			});
		});
		assert!(
			message.contains(&format!("lost wakeup: the root future run at {}:", file!())),
			"{message}"
		);
	}

	/// A root that reaches `idle` only on its probe re-poll would have deadlocked unprobed:
	/// the probe reports it rather than settling the wait.
	#[test]
	fn the_probe_reports_an_idle_wait_its_re_poll_registers() {
		let message = panic_message(|| {
			run_with(0, probing(), async {
				let milestone = Rc::new(Cell::new(false));
				drop(spawn({
					let milestone = milestone.clone();
					async move { milestone.set(true) }
				}));
				let mut waiting = None;
				crate::wait(|waiter| {
					if !milestone.get() {
						return Poll::Pending;
					}
					let idle = waiting.get_or_insert_with(|| Box::pin(idle()));
					waiter.poll_future(idle.as_mut())
				})
				.await;
			});
		});
		assert!(
			message.contains(&format!("lost wakeup: the root future run at {}:", file!())),
			"{message}"
		);
	}

	/// A root that finishes right after another thread woke a task still fails the run.
	#[test]
	#[should_panic(expected = "KIO_SIM_PROBE=1 saw a wake from a thread other than the run's")]
	fn the_probe_fails_a_wake_from_another_thread_as_the_root_finishes() {
		run_with(0, probing(), async {
			let parked: Arc<Mutex<Option<std::task::Waker>>> = Arc::default();
			drop(spawn({
				let parked = parked.clone();
				std::future::poll_fn(move |cx| {
					*parked.lock().unwrap() = Some(cx.waker().clone());
					Poll::<()>::Pending
				})
			}));
			yield_now().await;
			let waker = parked.lock().unwrap().take().expect("parked");
			std::thread::spawn(move || waker.wake()).join().unwrap();
		});
	}

	/// The probe fails a run woken from another thread, whose timing no seed replays.
	#[test]
	#[should_panic(expected = "KIO_SIM_PROBE=1 saw a wake from a thread other than the run's")]
	fn the_probe_fails_a_wake_from_another_thread() {
		run_with(0, probing(), async {
			let parked: Arc<Mutex<Option<std::task::Waker>>> = Arc::default();
			let mut woken = false;
			let mut task = Box::pin(spawn({
				let parked = parked.clone();
				std::future::poll_fn(move |cx| match std::mem::replace(&mut woken, true) {
					false => {
						*parked.lock().unwrap() = Some(cx.waker().clone());
						Poll::Pending
					}
					true => Poll::Ready(()),
				})
			}));
			let mut signalled = false;
			crate::wait(|waiter| {
				if super::probing() && !std::mem::replace(&mut signalled, true) {
					let waker = parked.lock().unwrap().take().expect("parked");
					std::thread::spawn(move || waker.wake()).join().unwrap();
				}
				waiter.poll_future(task.as_mut())
			})
			.await
			.unwrap();
		});
	}

	/// Under `pct`, a wake of an earlier run's task with the same id is no yield.
	#[test]
	fn an_earlier_runs_wake_is_not_a_yield() {
		let old = run_with(0, pct(), std::future::poll_fn(|cx| Poll::Ready(cx.waker().clone())));
		run_with(0, pct(), async {
			let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(1)));
			let mut polls = 0;
			crate::wait(|waiter| {
				polls += 1;
				if polls == 1 {
					old.wake_by_ref();
				} else {
					let priority = expect_current().priorities.borrow()[&ROOT];
					assert!(priority >= 1, "demoted to {priority}");
				}
				waiter.poll_future(sleep.as_mut())
			})
			.await;
		});
	}

	/// A budget spent on the poll that parked the task does not wake it when probed.
	#[test]
	fn the_probe_ignores_a_spent_budget() {
		run_with(0, probing(), async {
			let mut budget = crate::coop::Budget::new(1);
			let mut sleep = Box::pin(crate::clock::sleep(Duration::from_secs(1)));
			crate::wait(|waiter| {
				std::task::ready!(budget.poll_yield(waiter));
				waiter.poll_future(sleep.as_mut())
			})
			.await;
		});
	}

	#[test]
	fn the_probe_blames_a_task_set_child() {
		let message = panic_message(|| {
			run_with(0, probing(), async {
				type Boxed = Box<dyn FnMut(&crate::Waiter) -> Poll<()>>;
				let mut tasks: crate::Tasks<Boxed> = crate::Tasks::new();
				tasks.push(Box::new(|_: &crate::Waiter| Poll::Pending));
				tasks.push(Box::new(unwoken()));
				crate::wait(|waiter| tasks.poll(waiter)).await;
			});
		});
		assert!(
			message.contains(&format!("in its kio::Tasks child in slot 1 pushed at {}:", file!())),
			"{message}"
		);
	}

	/// Every task that was ready runs before a yielding task resumes, whatever the schedule.
	#[test]
	fn a_yield_lets_every_ready_task_run_first() {
		for config in [Config::default(), random(), pct()] {
			for seed in 0..20 {
				run_with(seed, config.clone(), async {
					let ran = Rc::new(Cell::new(0));
					for _ in 0..4 {
						let ran = ran.clone();
						drop(spawn(async move { ran.set(ran.get() + 1) }));
					}
					yield_now().await;
					assert_eq!(ran.get(), 4, "{config:?} seed {seed}");
				});
			}
		}
	}

	/// A task woken while it runs, then yielding, still goes behind a task woken after it.
	#[test]
	fn a_yield_runs_behind_tasks_woken_before_it() {
		for config in [Config::default(), random(), pct()] {
			for seed in 0..10 {
				run_with(seed, config.clone(), async {
					let mut sleep = pin!(crate::clock::sleep(Duration::from_secs(1)));
					// Armed first, so advancing wakes this task ahead of the other.
					let armed = std::future::poll_fn(|cx| Poll::Ready(sleep.as_mut().poll(cx))).await;
					assert!(armed.is_pending());
					let task = spawn(crate::clock::sleep(Duration::from_secs(1)));
					yield_now().await;
					advance(Duration::from_secs(1)).await;
					assert!(task.is_finished(), "{config:?} seed {seed}");
				});
			}
		}
	}

	/// A task that wakes itself and then a sibling, without yielding, runs first in FIFO.
	#[test]
	fn a_self_wake_keeps_its_place_in_wake_order() {
		fifo(async {
			let order = Rc::new(RefCell::new(Vec::new()));
			let parked: Rc<RefCell<Option<std::task::Waker>>> = Rc::default();
			let sibling = spawn({
				let (order, parked) = (order.clone(), parked.clone());
				std::future::poll_fn(move |cx| match parked.borrow_mut().replace(cx.waker().clone()) {
					None => Poll::Pending,
					Some(_) => {
						order.borrow_mut().push("sibling");
						Poll::Ready(())
					}
				})
			});
			yield_now().await;
			let mut woke = false;
			std::future::poll_fn(|cx| {
				if woke {
					order.borrow_mut().push("root");
					return Poll::Ready(());
				}
				woke = true;
				cx.waker().wake_by_ref();
				parked.borrow().as_ref().expect("the sibling parked").wake_by_ref();
				Poll::Pending
			})
			.await;
			sibling.await.unwrap();
			assert_eq!(*order.borrow(), ["root", "sibling"]);
		});
	}

	/// An `idle` future polled at every stall keeps one registration.
	#[test]
	fn idle_registers_once_however_often_probed() {
		run_with(0, probing(), async {
			let steps = spawn(async {
				for _ in 0..50 {
					crate::clock::sleep(Duration::from_secs(1)).await;
				}
				assert_eq!(expect_current().idle.borrow().len(), 1);
			});
			idle().await;
			steps.await.unwrap();
		});
	}

	/// The order four spawned tasks first run in.
	fn first_order(config: &Config, seed: u64) -> Vec<usize> {
		run_with(seed, config.clone(), async move {
			let order = Rc::new(RefCell::new(Vec::new()));
			let tasks: Vec<_> = (0..4)
				.map(|id| {
					let order = order.clone();
					spawn(async move { order.borrow_mut().push(id) })
				})
				.collect();
			for task in tasks {
				task.await.unwrap();
			}
			order.take()
		})
	}

	/// FIFO keeps wake order; the adversarial schedules vary it, replaying it from the seed.
	#[test]
	fn adversarial_schedules_reorder_tasks() {
		let orders =
			|config: &Config| -> HashSet<Vec<usize>> { (0..20).map(|seed| first_order(config, seed)).collect() };
		assert_eq!(orders(&Config::default()), HashSet::from([vec![0, 1, 2, 3]]));
		for config in [random(), pct()] {
			assert!(orders(&config).len() > 1, "{config:?}");
			assert_eq!(first_order(&config, 7), first_order(&config, 7));
		}
	}

	/// A poll asking for no instant drops the timer it armed before and waits only on a wake.
	/// It counts polls, so it pins FIFO without the probe.
	#[test]
	fn drive_waits_only_on_a_wake_when_no_instant_is_asked() {
		fifo(async {
			let producer = crate::Producer::new(0usize);
			let consumer = producer.consume();
			let writer = spawn(async move {
				for value in [1, 2] {
					crate::clock::sleep(Duration::from_secs(value as u64 * 2 - 1)).await;
					*producer.write().ok().expect("open") = value;
				}
			});

			let mut polls = Vec::new();
			let err = crate::clock::drive(|now, waiter| {
				polls.push(now);
				let wanted = polls.len();
				let _ = consumer.poll(waiter, |value| {
					if **value >= wanted {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				});
				match wanted {
					1 => Ok(Some(at(2))),
					2 => Ok(None),
					_ => Err("done"),
				}
			})
			.await;
			assert_eq!(err, "done");
			assert_eq!(polls, [at(0), at(1), at(4)], "the dropped timer still fired at 2");
			writer.await.unwrap();
		});
	}

	/// Draws of every kind across awaits and spawned tasks, so replay covers the run's whole schedule.
	async fn draws() -> (u64, Vec<u64>) {
		use rand::Rng as _;
		let task = spawn(async {
			crate::clock::sleep(Duration::from_secs(1)).await;
			crate::clock::rng().next_u64()
		});
		let first = crate::clock::rng().next_u64();
		let second = task.await.unwrap();
		let mut rng = crate::clock::rng();
		let mut bytes = [0; 8];
		rng.fill_bytes(&mut bytes);
		(
			crate::clock::seed(),
			vec![first, second, rng.next_u32().into(), u64::from_le_bytes(bytes)],
		)
	}

	#[test]
	fn a_seed_replays_its_draws() {
		let (seed, replayed) = run_seeded(7, draws());
		assert_eq!(seed, 7);
		assert_eq!(replayed, run_seeded(7, draws()).1);
		assert_ne!(replayed, run_seeded(8, draws()).1);
	}

	#[test]
	#[should_panic(expected = "used outside its kio::clock::sim::run")]
	fn an_rng_drawn_in_another_run_panics() {
		use rand::Rng as _;
		let mut rng = run(async { crate::clock::rng() });
		run(async move { rng.next_u64() });
	}
}

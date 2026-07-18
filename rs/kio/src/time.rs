//! Runtime-free timers backed by one process-wide driver thread.
//!
//! Intervals delay after a missed tick: they fire once, then schedule the next
//! tick from the current time instead of replaying every missed deadline.

use std::{
	cmp::Ordering,
	collections::BinaryHeap,
	fmt,
	future::Future,
	ops::{Add, AddAssign, Sub, SubAssign},
	pin::Pin,
	sync::{
		Arc, Condvar, LazyLock, Mutex, Weak,
		atomic::{AtomicU64, Ordering as AtomicOrdering},
	},
	task::{Context, Poll},
	thread,
	time::Duration,
};

use crate::{Pollable, Waiter, WaiterList};

/// A monotonic clock instant that follows the thread-local mock clock when enabled.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(std::time::Instant);

impl Instant {
	/// Returns the current monotonic time.
	pub fn now() -> Self {
		#[cfg(any(test, feature = "test-util"))]
		if let Some(now) = mock::now() {
			return Self(now);
		}

		Self(std::time::Instant::now())
	}

	/// Returns the time elapsed since this instant.
	pub fn elapsed(&self) -> Duration {
		Self::now().saturating_duration_since(*self)
	}

	/// Returns the duration since `earlier`, panicking if it is later.
	pub fn duration_since(&self, earlier: Self) -> Duration {
		self.0.duration_since(earlier.0)
	}

	/// Returns the duration since `earlier`, or zero if it is later.
	pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
		self.0.saturating_duration_since(earlier.0)
	}

	/// Returns the duration since `earlier`, if it is not later.
	pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
		self.0.checked_duration_since(earlier.0)
	}

	/// Returns this instant plus `duration`, or `None` on overflow.
	pub fn checked_add(&self, duration: Duration) -> Option<Self> {
		self.0.checked_add(duration).map(Self)
	}

	/// Returns this instant minus `duration`, or `None` on underflow.
	pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
		self.0.checked_sub(duration).map(Self)
	}
}

impl fmt::Debug for Instant {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl From<std::time::Instant> for Instant {
	fn from(value: std::time::Instant) -> Self {
		Self(value)
	}
}

impl From<Instant> for std::time::Instant {
	fn from(value: Instant) -> Self {
		value.0
	}
}

impl Add<Duration> for Instant {
	type Output = Self;

	fn add(self, rhs: Duration) -> Self {
		Self(self.0 + rhs)
	}
}

impl AddAssign<Duration> for Instant {
	fn add_assign(&mut self, rhs: Duration) {
		self.0 += rhs;
	}
}

impl Sub<Duration> for Instant {
	type Output = Self;

	fn sub(self, rhs: Duration) -> Self {
		Self(self.0 - rhs)
	}
}

impl SubAssign<Duration> for Instant {
	fn sub_assign(&mut self, rhs: Duration) {
		self.0 -= rhs;
	}
}

impl Sub for Instant {
	type Output = Duration;

	fn sub(self, rhs: Self) -> Duration {
		self.0 - rhs.0
	}
}

#[derive(Clone)]
enum Clock {
	Real,
	#[cfg(any(test, feature = "test-util"))]
	Mock(mock::Clock),
}

impl Clock {
	fn current() -> Self {
		#[cfg(any(test, feature = "test-util"))]
		if let Some(clock) = mock::current() {
			return Self::Mock(clock);
		}

		Self::Real
	}

	fn now(&self) -> Instant {
		match self {
			Self::Real => Instant(std::time::Instant::now()),
			#[cfg(any(test, feature = "test-util"))]
			Self::Mock(clock) => Instant(clock.now()),
		}
	}

	fn register(&self, entry: Entry) {
		match self {
			Self::Real => DRIVER.register(entry),
			#[cfg(any(test, feature = "test-util"))]
			Self::Mock(clock) => clock.register(entry),
		}
	}

	fn assert_thread(&self) {
		#[cfg(any(test, feature = "test-util"))]
		if let Self::Mock(clock) = self {
			clock.assert_thread();
		}
	}

	#[cfg(any(test, feature = "test-util"))]
	fn is_tokio(&self) -> bool {
		matches!(self, Self::Mock(clock) if clock.is_tokio())
	}
}

struct Timer {
	clock: Clock,
	state: Mutex<TimerState>,
}

struct TimerState {
	deadline: Instant,
	generation: u64,
	waiters: WaiterList,
	#[cfg(any(test, feature = "test-util"))]
	tokio: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Timer {
	fn new(deadline: Instant) -> Arc<Self> {
		let clock = Clock::current();
		let timer = Arc::new(Self {
			#[cfg(any(test, feature = "test-util"))]
			state: Mutex::new(TimerState {
				deadline,
				generation: 0,
				waiters: WaiterList::new(),
				tokio: clock
					.is_tokio()
					.then(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.0)))),
			}),
			#[cfg(not(any(test, feature = "test-util")))]
			state: Mutex::new(TimerState {
				deadline,
				generation: 0,
				waiters: WaiterList::new(),
			}),
			clock,
		});
		timer.register();
		timer
	}

	fn register(self: &Arc<Self>) {
		let state = self.state.lock().expect("timer state poisoned");
		let entry = Entry::new(state.deadline, state.generation, Arc::downgrade(self));
		drop(state);
		self.clock.register(entry);
	}

	fn poll(&self, waiter: &Waiter) -> Poll<()> {
		self.clock.assert_thread();
		let mut state = self.state.lock().expect("timer state poisoned");
		#[cfg(any(test, feature = "test-util"))]
		if let Some(sleep) = state.tokio.as_mut() {
			return waiter.poll_future(sleep.as_mut());
		}
		if self.clock.now() >= state.deadline {
			return Poll::Ready(());
		}
		waiter.register(&mut state.waiters);
		Poll::Pending
	}

	fn reset(self: &Arc<Self>, deadline: Instant) {
		self.clock.assert_thread();
		let mut state = self.state.lock().expect("timer state poisoned");
		state.deadline = deadline;
		state.generation = state.generation.wrapping_add(1);
		#[cfg(any(test, feature = "test-util"))]
		if let Some(sleep) = state.tokio.as_mut() {
			sleep.as_mut().reset(tokio::time::Instant::from_std(deadline.0));
		}
		drop(state);
		self.register();
	}

	fn fire(&self, deadline: Instant, generation: u64) {
		let mut state = self.state.lock().expect("timer state poisoned");
		if state.generation != generation || state.deadline != deadline {
			return;
		}
		let mut waiters = state.waiters.take();
		drop(state);
		waiters.wake();
	}
}

struct Entry {
	deadline: Instant,
	sequence: u64,
	generation: u64,
	timer: Weak<Timer>,
}

impl Entry {
	fn new(deadline: Instant, generation: u64, timer: Weak<Timer>) -> Self {
		static SEQUENCE: AtomicU64 = AtomicU64::new(0);
		Self {
			deadline,
			sequence: SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed),
			generation,
			timer,
		}
	}

	fn fire(self) {
		if let Some(timer) = self.timer.upgrade() {
			timer.fire(self.deadline, self.generation);
		}
	}
}

impl PartialEq for Entry {
	fn eq(&self, other: &Self) -> bool {
		self.deadline == other.deadline && self.sequence == other.sequence
	}
}

impl Eq for Entry {}

impl PartialOrd for Entry {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Entry {
	fn cmp(&self, other: &Self) -> Ordering {
		other
			.deadline
			.cmp(&self.deadline)
			.then_with(|| other.sequence.cmp(&self.sequence))
	}
}

struct Driver {
	heap: Mutex<BinaryHeap<Entry>>,
	wake: Condvar,
}

impl Driver {
	fn new() -> Arc<Self> {
		let driver = Arc::new(Self {
			heap: Mutex::new(BinaryHeap::new()),
			wake: Condvar::new(),
		});
		let worker = driver.clone();
		thread::Builder::new()
			.name("kio-time".into())
			.spawn(move || worker.run())
			.expect("failed to spawn kio timer driver");
		driver
	}

	fn register(&self, entry: Entry) {
		self.heap.lock().expect("timer heap poisoned").push(entry);
		self.wake.notify_one();
	}

	fn run(&self) {
		loop {
			let mut heap = self.heap.lock().expect("timer heap poisoned");
			let Some(entry) = heap.peek() else {
				drop(self.wake.wait(heap).expect("timer heap poisoned"));
				continue;
			};

			let delay = entry.deadline.0.saturating_duration_since(std::time::Instant::now());
			if !delay.is_zero() {
				let _ = self.wake.wait_timeout(heap, delay).expect("timer heap poisoned");
				continue;
			}

			let entry = heap.pop().expect("timer heap was non-empty");
			drop(heap);
			entry.fire();
		}
	}
}

static DRIVER: LazyLock<Arc<Driver>> = LazyLock::new(Driver::new);

/// A resettable wait until a monotonic deadline.
pub struct Sleep {
	timer: Arc<Timer>,
	waiter: Option<Waiter>,
}

impl Sleep {
	/// Polls whether the deadline has elapsed, registering `waiter` if not.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		self.timer.poll(waiter)
	}

	/// Changes the deadline and re-arms the timer.
	pub fn reset(&mut self, deadline: Instant) {
		self.timer.reset(deadline);
	}

	/// Waits until the deadline has elapsed.
	pub async fn wait(&mut self) {
		crate::wait(|waiter| self.poll(waiter)).await
	}
}

impl Pollable for Sleep {
	type Output = ();

	fn poll(&self, waiter: &Waiter) -> Poll<()> {
		self.timer.poll(waiter)
	}
}

impl Future for Sleep {
	type Output = ();

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
		let waiter = Waiter::new(cx.waker().clone());
		let result = self.timer.poll(&waiter);
		self.waiter = Some(waiter);
		result
	}
}

/// Creates a timer that completes after `duration`.
pub fn sleep(duration: Duration) -> Sleep {
	sleep_until(Instant::now() + duration)
}

/// Creates a timer that completes at `deadline`.
pub fn sleep_until(deadline: Instant) -> Sleep {
	Sleep {
		timer: Timer::new(deadline),
		waiter: None,
	}
}

/// A periodic timer that delays its next deadline after a missed tick.
pub struct Interval {
	sleep: Sleep,
	period: Duration,
}

impl Interval {
	/// Polls for the next tick.
	pub fn poll_tick(&mut self, waiter: &Waiter) -> Poll<Instant> {
		std::task::ready!(self.sleep.poll(waiter));
		let now = Instant::now();
		self.sleep.reset(now + self.period);
		Poll::Ready(now)
	}

	/// Waits for the next tick.
	pub async fn tick(&mut self) -> Instant {
		crate::wait(|waiter| self.poll_tick(waiter)).await
	}
}

/// Creates an interval whose first tick completes immediately.
pub fn interval(period: Duration) -> Interval {
	assert!(!period.is_zero(), "interval period must be non-zero");
	Interval {
		sleep: sleep_until(Instant::now()),
		period,
	}
}

#[cfg(any(test, feature = "test-util"))]
mod mock {
	use std::{cell::RefCell, thread::ThreadId};

	use super::*;

	thread_local! {
		static CURRENT: RefCell<Option<Clock>> = const { RefCell::new(None) };
	}

	#[derive(Clone)]
	pub(super) struct Clock {
		owner: ThreadId,
		kind: Kind,
	}

	#[derive(Clone)]
	enum Kind {
		Manual(Arc<Mutex<State>>),
		Tokio,
	}

	struct State {
		now: std::time::Instant,
		heap: BinaryHeap<Entry>,
	}

	impl Clock {
		fn manual() -> Self {
			Self {
				owner: thread::current().id(),
				kind: Kind::Manual(Arc::new(Mutex::new(State {
					now: std::time::Instant::now(),
					heap: BinaryHeap::new(),
				}))),
			}
		}

		fn tokio() -> Self {
			Self {
				owner: thread::current().id(),
				kind: Kind::Tokio,
			}
		}

		pub(super) fn assert_thread(&self) {
			assert_eq!(
				self.owner,
				thread::current().id(),
				"mock timers must stay on their creating thread"
			);
		}

		pub(super) fn now(&self) -> std::time::Instant {
			self.assert_thread();
			match &self.kind {
				Kind::Manual(inner) => inner.lock().expect("mock clock poisoned").now,
				Kind::Tokio => tokio::time::Instant::now().into_std(),
			}
		}

		pub(super) fn register(&self, entry: Entry) {
			self.assert_thread();
			match &self.kind {
				Kind::Manual(inner) => inner.lock().expect("mock clock poisoned").heap.push(entry),
				Kind::Tokio => {}
			}
		}

		pub(super) fn is_tokio(&self) -> bool {
			matches!(self.kind, Kind::Tokio)
		}

		fn advance_manual(&self, duration: Duration) {
			self.assert_thread();
			let Kind::Manual(inner) = &self.kind else {
				panic!("manual advance used with the tokio mock clock");
			};
			let mut state = inner.lock().expect("mock clock poisoned");
			state.now = state.now.checked_add(duration).expect("mock clock overflow");
			let now = state.now;
			let mut due = Vec::new();
			while state.heap.peek().is_some_and(|entry| entry.deadline.0 <= now) {
				due.push(state.heap.pop().expect("mock timer heap was non-empty"));
			}
			drop(state);
			for entry in due {
				entry.fire();
			}
		}
	}

	pub(super) fn current() -> Option<Clock> {
		CURRENT.with(|clock| clock.borrow().clone())
	}

	pub(super) fn now() -> Option<std::time::Instant> {
		current().map(|clock| clock.now())
	}

	pub(super) fn pause() {
		let clock = if tokio::runtime::Handle::try_current().is_ok() {
			tokio::time::pause();
			Clock::tokio()
		} else {
			Clock::manual()
		};
		CURRENT.with(|current| *current.borrow_mut() = Some(clock));
	}

	pub(super) async fn advance(duration: Duration) {
		let clock = current().expect("time is not paused");
		match &clock.kind {
			Kind::Manual(_) => clock.advance_manual(duration),
			Kind::Tokio => tokio::time::advance(duration).await,
		}
	}

	#[cfg(test)]
	pub(super) fn advance_manual(duration: Duration) {
		current().expect("time is not paused").advance_manual(duration);
	}
}

/// Freezes the clock on the current thread for deterministic tests.
///
/// Mock timers must be created and polled on this same thread. Inside a
/// current-thread tokio runtime, the mock uses tokio's clock and auto-advances
/// when the executor is idle. Other callers use a manually advanced clock.
#[cfg(any(test, feature = "test-util"))]
pub fn pause() {
	mock::pause();
}

/// Advances the current thread's mock clock and wakes every due timer.
#[cfg(any(test, feature = "test-util"))]
pub async fn advance(duration: Duration) {
	mock::advance(duration).await;
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	};

	#[tokio::test]
	async fn real_sleep_fires() {
		let start = std::time::Instant::now();
		sleep(Duration::from_millis(10)).await;
		assert!(start.elapsed() >= Duration::from_millis(5));
	}

	#[tokio::test]
	async fn tokio_mock_auto_advances() {
		pause();
		let start = Instant::now();
		let wall = std::time::Instant::now();
		sleep(Duration::from_secs(1)).await;
		assert!(Instant::now() - start >= Duration::from_secs(1));
		assert!(wall.elapsed() < Duration::from_millis(100));
	}

	#[test]
	fn mock_sleep_fires_at_deadline() {
		pause();
		let sleep = sleep(Duration::from_millis(50));
		let woke = Arc::new(AtomicBool::new(false));
		let flag = woke.clone();
		let waker = std::task::Waker::from(Arc::new(WakeFlag(flag)));
		let waiter = Waiter::new(waker);

		assert!(sleep.poll(&waiter).is_pending());
		mock::advance_manual(Duration::from_millis(49));
		assert!(!woke.load(Ordering::SeqCst));
		mock::advance_manual(Duration::from_millis(1));
		assert!(woke.load(Ordering::SeqCst));
		assert!(sleep.poll(&waiter).is_ready());
	}

	#[test]
	fn interval_delays_after_a_missed_tick() {
		pause();
		let mut interval = interval(Duration::from_millis(10));
		let waiter = Waiter::noop();
		assert!(interval.poll_tick(&waiter).is_ready());
		mock::advance_manual(Duration::from_millis(35));
		assert!(interval.poll_tick(&waiter).is_ready());
		assert!(interval.poll_tick(&waiter).is_pending());
		mock::advance_manual(Duration::from_millis(10));
		assert!(interval.poll_tick(&waiter).is_ready());
	}

	struct WakeFlag(Arc<AtomicBool>);

	impl std::task::Wake for WakeFlag {
		fn wake(self: Arc<Self>) {
			self.0.store(true, Ordering::SeqCst);
		}
	}
}

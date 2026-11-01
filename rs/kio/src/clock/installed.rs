//! A [`Manual`] clock installed as one tokio runtime's ambient clock.
//!
//! Unlike paused tokio and the simulator, it never jumps to the next timer when the
//! runtime idles: only [`Installed::advance`] moves it, so a host outside the
//! runtime (a foreign test loop) decides exactly when native time passes.

use std::{
	collections::{BTreeMap, HashMap, hash_map::Entry},
	sync::{
		Arc, LazyLock, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Waker},
	time::Duration,
};

use crate::clock::{Instant, Manual, Wall};

/// Installed clocks by runtime. Never pruned: one entry per runtime a test installs on.
static CLOCKS: LazyLock<Mutex<HashMap<tokio::runtime::Id, Installed>>> = LazyLock::new(Default::default);
/// Whether [`CLOCKS`] has any entry, so reads elsewhere skip its lock.
static ANY: AtomicBool = AtomicBool::new(false);

/// A [`Manual`] clock that one tokio runtime reads as its ambient clock.
///
/// Inside that runtime, or a thread that entered it, [`now`](crate::clock::now) and
/// [`wall`](crate::clock::wall) read this clock and timers fire only once
/// [`advance`](Self::advance) reaches them. Clones share the clock.
#[derive(Clone)]
pub struct Installed(Arc<Mutex<State>>);

struct State {
	clock: Manual,
	anchor: Option<(Instant, Wall)>,
	/// Orders timers armed for the same instant by when they were armed.
	next: u64,
	timers: BTreeMap<(Instant, u64), Waker>,
}

impl Manual {
	/// Make this clock the ambient clock of `runtime`, from its current instant and wall time.
	///
	/// Install before anything on the runtime reads the clock or arms a timer: those
	/// already armed stay on the runtime's own clock.
	///
	/// # Panics
	///
	/// When `runtime` already has an installed clock.
	pub fn install(self, runtime: &tokio::runtime::Handle) -> Installed {
		let installed = Installed(Arc::new(Mutex::new(State {
			clock: self,
			anchor: None,
			next: 0,
			timers: BTreeMap::new(),
		})));
		// Checked after the lock is released, so the panic leaves it unpoisoned.
		let fresh = match CLOCKS.lock().unwrap().entry(runtime.id()) {
			Entry::Occupied(_) => false,
			Entry::Vacant(entry) => {
				entry.insert(installed.clone());
				true
			}
		};
		assert!(fresh, "the runtime already has an installed clock");
		ANY.store(true, Ordering::Release);
		installed
	}
}

impl Installed {
	/// The clock installed on the current tokio runtime, if any.
	///
	/// For a runtime of its own, such as an io_uring worker, created on a thread that
	/// entered the tokio runtime, to keep that runtime's clock.
	pub fn current() -> Option<Self> {
		current()
	}

	/// Complete once this clock reaches `at`, whichever thread polls it.
	pub fn sleep_until(&self, at: Instant) -> crate::clock::Sleep {
		crate::clock::Sleep::installed(self.clone(), at)
	}

	/// The current instant.
	pub fn now(&self) -> Instant {
		self.0.lock().unwrap().clock.now()
	}

	/// The current wall time.
	pub fn wall(&self) -> Wall {
		self.0.lock().unwrap().clock.wall()
	}

	/// Let `duration` pass, moving both the instant and the wall time, and wake every timer due by then.
	///
	/// Returns the new instant, or `None` with the clock unchanged when either would run past
	/// its range. Checked under the clock's lock, so racing advances never overshoot.
	pub fn advance(&self, duration: Duration) -> Option<Instant> {
		let (now, due) = {
			let mut state = self.0.lock().unwrap();
			let now = state.clock.now().checked_add(duration)?;
			state.clock.wall().checked_add(duration)?;
			state.clock.advance(duration);
			let mut due = Vec::new();
			while let Some(entry) = state.timers.first_entry() {
				if entry.key().0 > now {
					break;
				}
				due.push(entry.remove());
			}
			(now, due)
		};
		// Outside the lock: a woken task may run inline and read the clock.
		due.into_iter().for_each(Waker::wake);
		Some(now)
	}

	/// Set the wall time, forward or back, as a clock correction would. No time passes and no timer fires.
	pub fn step_wall(&self, wall: Wall) {
		self.0.lock().unwrap().clock.step_wall(wall);
	}

	pub(crate) fn anchor(&self) -> (Instant, Wall) {
		let mut state = self.0.lock().unwrap();
		if let Some(anchor) = state.anchor {
			return anchor;
		}
		let anchor = (state.clock.now(), state.clock.wall());
		state.anchor = Some(anchor);
		anchor
	}
}

impl std::fmt::Debug for Installed {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("Installed").field(&self.now()).finish()
	}
}

/// The clock installed on the current runtime, if any.
pub(crate) fn current() -> Option<Installed> {
	if !ANY.load(Ordering::Acquire) {
		return None;
	}
	let runtime = tokio::runtime::Handle::try_current().ok()?;
	CLOCKS.lock().unwrap().get(&runtime.id()).cloned()
}

/// A timer on an installed clock, bound to it wherever it is polled.
pub(crate) struct Timer {
	clock: Installed,
	at: Instant,
	key: Option<(Instant, u64)>,
}

impl Timer {
	pub(crate) fn new(clock: Installed, at: Instant) -> Self {
		Self { clock, at, key: None }
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
		let key = self.key.take()?;
		self.clock.0.lock().unwrap().timers.remove(&key)
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		let mut state = self.clock.0.lock().unwrap();
		if state.clock.now() >= self.at {
			if let Some(key) = self.key.take() {
				state.timers.remove(&key);
			}
			return Poll::Ready(());
		}
		let key = *self.key.get_or_insert_with(|| {
			let id = state.next;
			state.next += 1;
			(self.at, id)
		});
		state.timers.insert(key, cx.waker().clone());
		Poll::Pending
	}
}

impl Drop for Timer {
	fn drop(&mut self) {
		self.disarm();
	}
}

#[cfg(test)]
mod tests {
	use std::{future::Future, pin::pin, task::Poll};

	use super::*;

	fn runtime(paused: bool) -> tokio::runtime::Runtime {
		tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(paused)
			.build()
			.unwrap()
	}

	/// Poll `future` once, without waiting.
	fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
		let mut cx = Context::from_waker(Waker::noop());
		std::pin::Pin::new(future).poll(&mut cx)
	}

	/// A pending timer holds the clock still even when the runtime idles, which is
	/// what paused tokio and the simulator would skip ahead on.
	#[test]
	fn a_pending_timer_waits_for_advance() {
		// Paused, so tokio's own clock auto-advances while this test idles.
		let runtime = runtime(true);
		let clock = Manual::new().install(runtime.handle());
		runtime.block_on(async {
			let start = crate::clock::now();
			assert_eq!(start, Instant::ZERO);
			let mut sleep = pin!(crate::clock::sleep(Duration::from_secs(1)));
			assert!(poll_once(&mut sleep).is_pending());

			// Idle on tokio's paused clock for an hour: the installed clock stays put.
			#[expect(
				clippy::disallowed_methods,
				reason = "the test idles on tokio's clock, not the installed one"
			)]
			tokio::time::sleep(Duration::from_secs(3_600)).await;
			assert_eq!(crate::clock::now(), start);
			assert!(poll_once(&mut sleep).is_pending());

			clock.advance(Duration::from_millis(999));
			assert!(poll_once(&mut sleep).is_pending());
			clock.advance(Duration::from_millis(1));
			sleep.await;
			assert_eq!(crate::clock::now(), start + Duration::from_secs(1));
		});
	}

	/// Advancing wakes the task parked on a due timer.
	#[test]
	fn advance_wakes_a_parked_timer() {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_time()
			.build()
			.unwrap();
		let clock = Manual::new().install(runtime.handle());
		let (armed, armed_rx) = std::sync::mpsc::channel();
		let task = runtime.spawn(async move {
			let sleep = crate::clock::sleep(Duration::from_secs(5));
			armed.send(()).unwrap();
			sleep.await;
			crate::clock::now()
		});
		armed_rx.recv().unwrap();
		clock.advance(Duration::from_secs(5));
		assert_eq!(runtime.block_on(task).unwrap(), Instant::ZERO + Duration::from_secs(5));
	}

	/// Resetting a timer re-arms it at the new deadline.
	#[test]
	fn reset_rearms() {
		let runtime = runtime(false);
		let clock = Manual::new().install(runtime.handle());
		runtime.block_on(async {
			let mut sleep = pin!(crate::clock::sleep(Duration::from_secs(1)));
			assert!(poll_once(&mut sleep).is_pending());
			sleep.as_mut().reset(Instant::ZERO + Duration::from_secs(2));
			clock.advance(Duration::from_secs(1));
			assert!(poll_once(&mut sleep).is_pending());
			clock.advance(Duration::from_secs(1));
			assert!(poll_once(&mut sleep).is_ready());
		});
	}

	/// Wall time and the origin come from the installed clock, and a wall step passes no time.
	#[test]
	fn wall_reads_the_installed_clock() {
		let runtime = runtime(false);
		let clock = Manual::new().install(runtime.handle());
		let _entered = runtime.enter();
		assert_eq!(crate::clock::wall(), Manual::WALL_START);
		assert_eq!(crate::clock::origin(), (Instant::ZERO, Manual::WALL_START));

		clock.advance(Duration::from_secs(5));
		assert_eq!(crate::clock::wall(), Manual::WALL_START + Duration::from_secs(5));
		clock.step_wall(Manual::WALL_START);
		assert_eq!(crate::clock::wall(), Manual::WALL_START);
		assert_eq!(crate::clock::now(), Instant::ZERO + Duration::from_secs(5));
		assert_eq!(
			crate::clock::origin(),
			(Instant::ZERO, Manual::WALL_START),
			"the origin is fixed at its first read"
		);
	}

	/// Other runtimes and plain threads keep the host clock.
	#[test]
	fn only_its_runtime_reads_it() {
		let installed = runtime(false);
		let _clock = Manual::new().install(installed.handle());
		let other = runtime(false);
		// The host clock reads far past zero from its first instant.
		let host = Instant::ZERO + Duration::from_secs(1 << 31);
		assert!(other.block_on(async { crate::clock::now() }) > host);
		assert!(crate::clock::now() > host);
		assert_eq!(installed.block_on(async { crate::clock::now() }), Instant::ZERO);
	}

	/// An advance past the range is refused whole, leaving the clock where it was.
	#[test]
	fn advance_past_the_range_changes_nothing() {
		let runtime = runtime(false);
		let clock = Manual::new().install(runtime.handle());
		assert_eq!(
			clock.advance(Duration::from_secs(1)),
			Some(Instant::ZERO + Duration::from_secs(1))
		);
		// The wall clock starts in 2025, so it runs out first.
		assert_eq!(clock.advance(Duration::from_nanos(u64::MAX - 1_000_000_000)), None);
		assert_eq!(clock.now(), Instant::ZERO + Duration::from_secs(1));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(1));
	}

	#[test]
	#[should_panic(expected = "already has an installed clock")]
	fn installing_twice_panics() {
		let runtime = runtime(false);
		let _first = Manual::new().install(runtime.handle());
		let _second = Manual::new().install(runtime.handle());
	}
}

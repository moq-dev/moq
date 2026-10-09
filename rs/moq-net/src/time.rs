//! Caller-supplied time for protocol and model drivers.
//!
//! Drivers take the current [`Instant`] on every poll and never read a clock.

use crate::Error;
use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex},
	task::Poll,
};

/// The instant type drivers are polled with.
///
/// [`std::time::Instant`] on native. The browser has no monotonic std clock, so
/// wasm substitutes an equivalent backed by `performance.now()`.
#[cfg(not(target_family = "wasm"))]
pub type Instant = std::time::Instant;
/// The instant type drivers are polled with (wasm shim).
#[cfg(target_family = "wasm")]
pub type Instant = web_async::time::Instant;

/// A state machine polled with caller-supplied time.
///
/// Each poll advances the driver to `now`, processes ready work, and registers
/// `waiter` for external activity. `Ok(Some(at))` asks to be polled again by
/// `at` (or sooner, on a wake); `Ok(None)` means only external activity can
/// make progress. `Err` is terminal: the driver has finished and must not be
/// polled again. A clean finish is [`Error::Closed`].
pub trait Driver {
	/// Advance to `now` and process ready work.
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error>;
}

/// Run a driver to completion on the ambient runtime, sleeping until each deadline.
///
/// Tokio on native, `setTimeout` in the browser. Resolves with the driver's
/// terminal error, [`Error::Closed`] for a clean finish.
pub async fn run<D: Driver>(mut driver: D) -> Error {
	let mut timer: Option<std::pin::Pin<Box<web_async::time::Sleep>>> = None;
	kio::wait(|waiter| {
		loop {
			let now = web_async::time::Instant::now();
			#[cfg(not(target_family = "wasm"))]
			let now = now.into_std();
			let at = match driver.poll(now, waiter) {
				Ok(Some(at)) => at,
				Ok(None) => {
					timer = None;
					return Poll::Pending;
				}
				Err(err) => return Poll::Ready(err),
			};
			#[cfg(not(target_family = "wasm"))]
			let at = web_async::time::Instant::from_std(at);
			let sleep = timer.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(at)));
			if sleep.deadline() != at {
				sleep.as_mut().reset(at);
			}
			if waiter.poll_future(sleep.as_mut()).is_pending() {
				return Poll::Pending;
			}
		}
	})
	.await
}

/// Run a driver to completion on the test executor, polled with its simulated time.
#[cfg(test)]
pub(crate) async fn run_sim<D: Driver + Unpin>(mut driver: D) -> Error {
	moq_net_sim::drive(move |now, waiter| driver.poll(now, waiter)).await
}

/// A clock private to one driver, advanced only by the instants it is polled with.
///
/// Clones share the clock. Everything the driver owns arms its [`Deadline`]s here,
/// and [`Self::timeout`] reports the earliest so the driver can ask to be polled
/// again by then.
#[derive(Clone, Default)]
pub(crate) struct Clock(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
	now: Option<Instant>,
	next: u64,
	deadlines: BTreeMap<(Instant, u64), Arc<Mutex<kio::WaiterList>>>,
	/// The driver's poll, woken when a deadline armed outside it becomes the earliest,
	/// so the driver's sleep never outlasts a deadline it did not see.
	driver: kio::WaiterList,
}

impl Clock {
	/// A clock the test executor advances with its simulated time. Outside the
	/// executor, a synchronous test never waits on a deadline, so nothing advances it.
	#[cfg(test)]
	pub(crate) fn sim() -> Self {
		if !moq_net_sim::is_running() {
			return Self::new(Instant::now());
		}
		let clock = Self::new(moq_net_sim::now());
		let driven = clock.clone();
		moq_net_sim::attach(move |now| {
			driven.advance(now);
			driven.timeout()
		});
		clock
	}

	pub(crate) fn new(now: Instant) -> Self {
		let clock = Self::default();
		clock.advance(now);
		clock
	}

	/// Move to `now`, waking every deadline at or before it.
	pub(crate) fn advance(&self, now: Instant) {
		let due = {
			let mut state = self.0.lock().unwrap();
			assert!(state.now.is_none_or(|prev| now >= prev), "driver time moved backwards");
			state.now = Some(now);
			let mut due = Vec::new();
			while state.deadlines.first_key_value().is_some_and(|((at, _), _)| *at <= now) {
				due.push(state.deadlines.pop_first().unwrap().1);
			}
			due
		};
		for waiters in due {
			// Take the registrations before waking: wake may immediately poll a deadline.
			let mut ready = waiters.lock().unwrap().take();
			ready.wake();
		}
	}

	/// The latest supplied instant, or `None` before the first.
	pub(crate) fn try_now(&self) -> Option<Instant> {
		self.0.lock().unwrap().now
	}

	/// Register the driver's poll to be woken by a new earliest deadline.
	pub(crate) fn register_driver(&self, waiter: &kio::Waiter) {
		waiter.register(&mut self.0.lock().unwrap().driver);
	}

	/// The earliest armed deadline still in the future.
	pub(crate) fn timeout(&self) -> Option<Instant> {
		self.0
			.lock()
			.unwrap()
			.deadlines
			.first_key_value()
			.map(|((at, _), _)| *at)
	}

	/// The latest instant supplied by the owning driver.
	pub(crate) fn now(&self) -> Instant {
		self.0.lock().unwrap().now.expect("driver has not been polled")
	}
}

/// A re-armable deadline on a [`Clock`].
///
/// Arm it, poll it from a `poll_*` function, re-arm or disarm as the deadline
/// moves. Re-setting the instant it already holds does nothing, so a poll loop can
/// recompute its deadline every turn without restarting the countdown.
pub(crate) struct Deadline {
	clock: Clock,
	id: u64,
	at: Option<Instant>,
	waiters: Arc<Mutex<kio::WaiterList>>,
}

impl Deadline {
	/// A disarmed deadline, which never fires until [`set`](Self::set) arms it.
	pub(crate) fn new(clock: &Clock) -> Self {
		let mut state = clock.0.lock().unwrap();
		let id = state.next;
		state.next = state.next.checked_add(1).expect("deadline identifier overflow");
		Self {
			clock: clock.clone(),
			id,
			at: None,
			waiters: Arc::new(Mutex::new(kio::WaiterList::new())),
		}
	}

	/// A deadline armed for `at`.
	pub(crate) fn at(clock: &Clock, at: Instant) -> Self {
		let mut deadline = Self::new(clock);
		deadline.set(Some(at));
		deadline
	}

	/// A deadline armed for `duration` past the clock's [`now`](Clock::now).
	///
	/// A duration the clock cannot represent (e.g. [`std::time::Duration::MAX`])
	/// leaves the deadline disarmed, so it never fires rather than panicking on
	/// the overflow.
	pub(crate) fn after(clock: &Clock, duration: std::time::Duration) -> Self {
		let mut deadline = Self::new(clock);
		deadline.set(clock.now().checked_add(duration));
		deadline
	}

	/// Arm, re-arm, or disarm (`None`) the deadline.
	///
	/// Re-arming an elapsed deadline for a later instant makes it pend again;
	/// re-arming for an instant already in the past leaves it elapsed.
	pub(crate) fn set(&mut self, at: Option<Instant>) {
		if self.at == at {
			return;
		}
		let mut state = self.clock.0.lock().unwrap();
		if let Some(old) = self.at.take() {
			state.deadlines.remove(&(old, self.id));
		}
		self.at = at;
		if let Some(at) = at
			&& state.now.is_none_or(|now| at > now)
		{
			state.deadlines.insert((at, self.id), self.waiters.clone());
			if state
				.deadlines
				.first_key_value()
				.is_some_and(|(key, _)| *key == (at, self.id))
			{
				let mut driver = state.driver.take();
				drop(state);
				driver.wake();
			}
		}
	}

	/// The instant this fires at, or `None` while disarmed.
	pub(crate) fn deadline(&self) -> Option<Instant> {
		self.at
	}

	/// `Ready` once the clock reaches the armed instant, registering `waiter`
	/// otherwise. Fused until re-armed; a disarmed deadline never fires.
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else { return Poll::Pending };
		// Register under the clock lock, so an `advance` past `at` either happened
		// first (and we see it) or wakes this registration.
		let state = self.clock.0.lock().unwrap();
		if state.now.is_some_and(|now| now >= at) {
			return Poll::Ready(());
		}
		waiter.register(&mut self.waiters.lock().unwrap());
		Poll::Pending
	}
}

impl Drop for Deadline {
	fn drop(&mut self) {
		self.set(None);
	}
}

impl std::fmt::Debug for Deadline {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Deadline").field("at", &self.at).finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	#[test]
	fn deadlines_follow_only_supplied_time() {
		let now = Instant::now();
		let clock = Clock::new(now);
		let at = now + Duration::from_secs(3);
		let mut deadline = Deadline::at(&clock, at);
		assert_eq!(clock.timeout(), Some(at));
		assert!(deadline.poll(&kio::Waiter::noop()).is_pending());
		clock.advance(at);
		assert!(deadline.poll(&kio::Waiter::noop()).is_ready());
		assert!(
			deadline.poll(&kio::Waiter::noop()).is_ready(),
			"an elapsed deadline stays fused"
		);
		assert_eq!(clock.timeout(), None);
		deadline.set(Some(at + Duration::from_secs(1)));
		assert!(deadline.poll(&kio::Waiter::noop()).is_pending());
		drop(deadline);
		assert_eq!(clock.timeout(), None);
	}

	#[test]
	fn disarmed_never_fires() {
		let now = Instant::now();
		let clock = Clock::new(now);
		let mut deadline = Deadline::after(&clock, Duration::from_secs(1));
		deadline.set(None);
		assert_eq!(clock.timeout(), None);
		clock.advance(now + Duration::from_secs(3600));
		assert!(deadline.poll(&kio::Waiter::noop()).is_pending());

		let mut unrepresentable = Deadline::after(&clock, Duration::MAX);
		assert_eq!(unrepresentable.deadline(), None);
		assert!(unrepresentable.poll(&kio::Waiter::noop()).is_pending());
	}

	#[test]
	fn advance_wakes_a_parked_waiter() {
		struct Flag(std::sync::atomic::AtomicBool);
		impl std::task::Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, std::sync::atomic::Ordering::SeqCst);
			}
		}

		let now = Instant::now();
		let clock = Clock::new(now);
		let mut deadline = Deadline::after(&clock, Duration::from_secs(1));
		let flag = Arc::new(Flag(std::sync::atomic::AtomicBool::new(false)));
		let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));

		assert!(deadline.poll(&waiter).is_pending());
		clock.advance(now + Duration::from_secs(1));
		assert!(
			flag.0.load(std::sync::atomic::Ordering::SeqCst),
			"the deadline wake was lost"
		);
		assert!(deadline.poll(&waiter).is_ready());
	}

	#[test]
	#[should_panic(expected = "driver time moved backwards")]
	fn time_cannot_move_backwards() {
		let now = Instant::now();
		let clock = Clock::new(now);
		clock.advance(now - Duration::from_secs(1));
	}
}

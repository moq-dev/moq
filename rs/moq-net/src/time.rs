//! Caller-supplied time for protocol and model drivers.

use crate::Error;
use crate::runtime::{Timer as _, Timers};
use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex},
	task::Poll,
};

pub use crate::runtime::Instant;

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

/// A private clock shared only by work owned by one driver.
#[derive(Clone, Default)]
pub(crate) struct Clock(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
	#[cfg(test)]
	automatic: bool,
	now: Option<Instant>,
	next: u64,
	timers: BTreeMap<(Instant, u64), Arc<Mutex<kio::WaiterList>>>,
}

impl Clock {
	#[cfg(test)]
	pub(crate) fn tokio() -> Self {
		let clock = Self::new(tokio::time::Instant::now().into_std());
		clock.0.lock().unwrap().automatic = true;
		clock
	}

	pub(crate) fn new(now: Instant) -> Self {
		let clock = Self::default();
		clock.advance(now);
		clock
	}

	pub(crate) fn advance(&self, now: Instant) {
		let due = {
			let mut state = self.0.lock().unwrap();
			assert!(state.now.is_none_or(|prev| now >= prev), "driver time moved backwards");
			state.now = Some(now);
			let mut due = Vec::new();
			while state.timers.first_key_value().is_some_and(|((at, _), _)| *at <= now) {
				due.push(state.timers.pop_first().unwrap().1);
			}
			due
		};
		for waiters in due {
			// Take the registrations before waking: wake may immediately poll a timer.
			let mut ready = waiters.lock().unwrap().take();
			ready.wake();
		}
	}

	pub(crate) fn timeout(&self) -> Option<Instant> {
		self.0.lock().unwrap().timers.first_key_value().map(|((at, _), _)| *at)
	}
}

impl Timers for Clock {
	type Timer = Timer;
	fn now(&self) -> Instant {
		let state = self.0.lock().unwrap();
		#[cfg(test)]
		if state.automatic {
			return tokio::time::Instant::now().into_std();
		}
		state.now.expect("driver has not been polled")
	}
	fn timer(&self) -> Timer {
		let mut state = self.0.lock().unwrap();
		let id = state.next;
		state.next = state.next.checked_add(1).expect("timer identifier overflow");
		Timer {
			#[cfg(test)]
			automatic: state
				.automatic
				.then(|| crate::runtime::tokio_test::Tokio::new().timer()),
			clock: self.clone(),
			id,
			at: None,
			waiters: Arc::new(Mutex::new(kio::WaiterList::new())),
		}
	}
}

pub(crate) struct Timer {
	#[cfg(test)]
	automatic: Option<crate::runtime::tokio_test::TokioTimer>,
	clock: Clock,
	id: u64,
	at: Option<Instant>,
	waiters: Arc<Mutex<kio::WaiterList>>,
}

impl crate::runtime::Timer for Timer {
	fn set(&mut self, at: Option<Instant>) {
		#[cfg(test)]
		if let Some(timer) = &mut self.automatic {
			timer.set(at);
			return;
		}
		if self.at == at {
			return;
		}
		let mut state = self.clock.0.lock().unwrap();
		if let Some(old) = self.at.take() {
			state.timers.remove(&(old, self.id));
		}
		self.at = at;
		if let Some(at) = at
			&& state.now.is_none_or(|now| at > now)
		{
			state.timers.insert((at, self.id), self.waiters.clone());
		}
	}
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		#[cfg(test)]
		if let Some(timer) = &mut self.automatic {
			return timer.poll(waiter);
		}
		let state = self.clock.0.lock().unwrap();
		match self.at {
			Some(at) if state.now.is_some_and(|now| now >= at) => Poll::Ready(()),
			Some(_) => {
				waiter.register(&mut self.waiters.lock().unwrap());
				Poll::Pending
			}
			None => Poll::Pending,
		}
	}
}

impl Drop for Timer {
	fn drop(&mut self) {
		self.set(None);
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
		let mut timer = clock.timer();
		let at = now + Duration::from_secs(3);
		timer.set(Some(at));
		assert_eq!(clock.timeout(), Some(at));
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());
		clock.advance(at);
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());
		assert_eq!(clock.timeout(), None);
		timer.set(Some(at + Duration::from_secs(1)));
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());
		drop(timer);
		assert_eq!(clock.timeout(), None);
	}

	#[test]
	#[should_panic(expected = "driver time moved backwards")]
	fn time_cannot_move_backwards() {
		let now = Instant::now();
		let clock = Clock::new(now);
		clock.advance(now - Duration::from_secs(1));
	}
}

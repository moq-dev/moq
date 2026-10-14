use std::{
	fmt,
	future::{Future, IntoFuture},
	pin::Pin,
	task::{Context, Poll},
	time::Duration,
};

use crate::Instant;

/// A future that completes once the ambient clock reaches an instant.
///
/// Bound to the clock current when it was created, so poll it there: instants
/// from different clocks don't compare. Dropping it cancels it.
pub struct Sleep {
	at: Instant,
	timer: Timer,
}

enum Timer {
	Host(crate::runtime::Timer),
	#[cfg(feature = "sim")]
	Sim(crate::sim::Timer),
}

/// Complete once the ambient clock reaches `at`; at once if it already has.
pub fn sleep_until(at: Instant) -> Sleep {
	#[cfg(feature = "sim")]
	if let Some(timer) = crate::sim::Timer::new(at) {
		return Sleep {
			at,
			timer: Timer::Sim(timer),
		};
	}
	Sleep {
		at,
		timer: Timer::Host(crate::runtime::Timer::new(at)),
	}
}

/// Complete once `duration` has passed on the ambient clock.
///
/// A duration past the clock's range never completes.
pub fn sleep(duration: Duration) -> Sleep {
	// Instant's range is 584 years, so a saturated deadline is never reached.
	let at = crate::now().checked_add(duration).unwrap_or(Instant::MAX);
	sleep_until(at)
}

impl Sleep {
	/// The instant this completes at.
	pub fn deadline(&self) -> Instant {
		self.at
	}

	/// Complete at `at` instead, re-arming if it already completed.
	pub fn reset(&mut self, at: Instant) {
		self.at = at;
		match &mut self.timer {
			Timer::Host(timer) => timer.reset(at),
			#[cfg(feature = "sim")]
			Timer::Sim(timer) => timer.reset(at),
		}
	}
}

impl Future for Sleep {
	type Output = ();

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
		// Each backend compares against its own clock, not the poller's.
		match &mut self.timer {
			Timer::Host(timer) => timer.poll(cx),
			#[cfg(feature = "sim")]
			Timer::Sim(timer) => timer.poll(cx),
		}
	}
}

impl fmt::Debug for Sleep {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Sleep").field("deadline", &self.at).finish()
	}
}

/// [`timeout`] ran out before its future completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("deadline elapsed")
	}
}

impl std::error::Error for Elapsed {}

/// Run `future` for at most `duration` on the ambient clock, counted from this call.
///
/// The future is polled first, so one that is already ready wins even at a zero duration.
pub fn timeout<F: IntoFuture>(duration: Duration, future: F) -> Timeout<F::IntoFuture> {
	Timeout {
		future: Box::pin(future.into_future()),
		sleep: sleep(duration),
	}
}

/// The future [`timeout`] returns.
pub struct Timeout<F> {
	future: Pin<Box<F>>,
	sleep: Sleep,
}

impl<F: Future> Future for Timeout<F> {
	type Output = Result<F::Output, Elapsed>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		if let Poll::Ready(output) = self.future.as_mut().poll(cx) {
			return Poll::Ready(Ok(output));
		}
		Pin::new(&mut self.sleep).poll(cx).map(|()| Err(Elapsed))
	}
}

/// What an [`Interval`] does about ticks it missed because it was not polled in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissedTickBehavior {
	/// Fire every missed tick at once, keeping the original schedule.
	#[default]
	Burst,
	/// Fire one tick now and schedule the rest a full period from it.
	Delay,
	/// Fire one tick now and resume the original schedule at its next tick.
	Skip,
}

/// Ticks on the ambient clock every period, starting at once.
pub struct Interval {
	sleep: Sleep,
	period: Duration,
	missed: MissedTickBehavior,
}

/// Tick every `period` on the ambient clock; the first tick completes at once.
///
/// # Panics
///
/// When `period` is zero.
pub fn interval(period: Duration) -> Interval {
	assert!(!period.is_zero(), "interval period must be non-zero");
	Interval {
		sleep: sleep_until(crate::now()),
		period,
		missed: MissedTickBehavior::default(),
	}
}

impl Interval {
	/// Wait for the next tick, returning the instant it was scheduled for.
	pub async fn tick(&mut self) -> Instant {
		std::future::poll_fn(|cx| self.poll_tick(cx)).await
	}

	/// Poll for the next tick, returning the instant it was scheduled for.
	pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<Instant> {
		if Pin::new(&mut self.sleep).poll(cx).is_pending() {
			return Poll::Pending;
		}
		let scheduled = self.sleep.deadline();
		let now = crate::now();
		let mut next = scheduled.checked_add(self.period).unwrap_or(Instant::MAX);
		if next <= now {
			next = match self.missed {
				MissedTickBehavior::Burst => next,
				MissedTickBehavior::Delay => now.checked_add(self.period).unwrap_or(Instant::MAX),
				MissedTickBehavior::Skip => {
					// Back on the original schedule: its first tick after now.
					let into = now.duration_since(scheduled).as_nanos() % self.period.as_nanos();
					let into = Duration::from_nanos(into as u64);
					now.checked_add(self.period - into).unwrap_or(Instant::MAX)
				}
			};
		}
		self.sleep.reset(next);
		Poll::Ready(scheduled)
	}

	/// Set what happens to missed ticks.
	pub fn set_missed_tick_behavior(&mut self, missed: MissedTickBehavior) {
		self.missed = missed;
	}

	/// The time between ticks.
	pub fn period(&self) -> Duration {
		self.period
	}
}

/// Run a caller-driven state machine on the ambient clock until it returns an error.
///
/// `poll` is called with the current instant and returns when it next wants to be
/// polled: `Ok(Some(at))` by `at` or on a wake, `Ok(None)` only on a wake.
pub async fn drive<E>(mut poll: impl FnMut(Instant, &kio::Waiter) -> Result<Option<Instant>, E> + Unpin) -> E {
	let mut timer: Option<Sleep> = None;
	kio::wait(move |waiter| {
		loop {
			let at = match poll(crate::now(), waiter) {
				Ok(Some(at)) => at,
				Ok(None) => {
					timer = None;
					return Poll::Pending;
				}
				Err(err) => return Poll::Ready(err),
			};
			let sleep = timer.get_or_insert_with(|| sleep_until(at));
			if sleep.deadline() != at {
				sleep.reset(at);
			}
			if waiter.poll_future(Pin::new(sleep)).is_pending() {
				return Poll::Pending;
			}
		}
	})
	.await
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Test builds enable `sim`, so this also proves the simulator backend does not
	/// make the ambient timers `!Send` for code that spawns them on tokio.
	#[test]
	fn timers_are_send() {
		fn send<T: Send>() {}
		send::<Sleep>();
		send::<Interval>();
		send::<Timeout<std::future::Ready<()>>>();
	}
}

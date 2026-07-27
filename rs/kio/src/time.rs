//! Poll-driven wall-clock deadlines.
//!
//! Behind the `time` feature. Built on [`web_async::time`], which is `tokio::time` on
//! native and `wasmtimer` in the browser, so the rest of kio stays runtime-free.
//!
//! On native, a timer must first be polled inside a tokio runtime with the time driver
//! enabled, or tokio panics. Reading the clock ([`Instant::now`]) has no such
//! requirement. Since the clock is tokio's, `tokio::time::pause()` advances these
//! deadlines in tests.

use std::{pin::Pin, task::Poll};

/// Re-exported from `web-async`, so a major bump of that crate is a breaking change
/// for these types.
pub use web_async::time::{Duration, Instant};

use crate::Waiter;

/// A wall-clock deadline driven by kio's poll model.
///
/// Arm it with an [`Instant`], poll it from a `poll_*` function, and re-arm or disarm it
/// as the deadline moves. A disarmed deadline never fires, and an elapsed one stays
/// ready until it is armed for a different instant.
///
/// ```no_run
/// # async fn example(next_expiry: Option<kio::time::Instant>) {
/// let mut deadline = kio::time::Deadline::new();
/// deadline.set(next_expiry);
/// kio::wait(|waiter| deadline.poll(waiter)).await;
/// # }
/// ```
pub struct Deadline {
	at: Option<Instant>,

	// Allocated on the first poll after arming, then re-armed in place via `Sleep::reset`.
	// Construction is deferred because on native it panics without a live tokio time
	// driver, and only the poll is guaranteed to run inside the executor.
	sleep: Option<Pin<Box<web_async::time::Sleep>>>,
}

impl Deadline {
	/// A disarmed deadline, which never fires until [`set`](Self::set) arms it.
	pub fn new() -> Self {
		Self { at: None, sleep: None }
	}

	/// A deadline armed for `at`.
	pub fn at(at: Instant) -> Self {
		Self {
			at: Some(at),
			sleep: None,
		}
	}

	/// A deadline armed for `duration` from now.
	///
	/// A duration the clock cannot represent (e.g. [`Duration::MAX`]) leaves the deadline
	/// disarmed, so it never fires rather than panicking on the overflow.
	pub fn after(duration: Duration) -> Self {
		Self {
			at: Instant::now().checked_add(duration),
			sleep: None,
		}
	}

	/// Arm, re-arm, or disarm (`None`) the deadline.
	///
	/// Setting the instant it already holds does nothing, so a poll loop can recompute
	/// its deadline every turn without restarting the countdown.
	pub fn set(&mut self, at: Option<Instant>) {
		if self.at == at {
			return;
		}
		self.at = at;

		// Reuse the allocation when there is one; `reset` also clears `is_elapsed`.
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(at);
		}
	}

	/// The instant this fires at, or `None` while disarmed.
	pub fn deadline(&self) -> Option<Instant> {
		self.at
	}

	/// Poll the deadline, registering `waiter` so the poll re-fires once it elapses.
	///
	/// `Ready` once the instant has passed, `Pending` before then and while disarmed.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		// Disarmed: register nothing. Only `set` can arm it, and the caller driving this
		// poll is the one that calls `set`.
		let Some(at) = self.at else { return Poll::Pending };

		let sleep = self
			.sleep
			.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(at)));

		// Fused, so a caller that keeps polling after the deadline keeps seeing `Ready`
		// rather than re-polling a completed future.
		if sleep.is_elapsed() {
			return Poll::Ready(());
		}

		waiter.poll_future(sleep.as_mut())
	}

	/// Wait for the deadline to elapse. Parks forever while disarmed.
	pub async fn wait(&mut self) {
		crate::wait(|waiter| self.poll(waiter)).await
	}
}

impl Default for Deadline {
	fn default() -> Self {
		Self::new()
	}
}

impl std::fmt::Debug for Deadline {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Deadline").field("at", &self.at).finish()
	}
}

#[cfg(test)]
mod tests {
	use std::task::Waker;

	use super::*;

	/// Poll once without parking, for asserting `Pending` without hanging the test.
	fn poll_once(deadline: &mut Deadline) -> Poll<()> {
		let waiter = Waiter::new(Waker::noop().clone());
		deadline.poll(&waiter)
	}

	#[tokio::test(start_paused = true)]
	async fn fires_at_its_deadline() {
		let at = Instant::now() + Duration::from_secs(5);
		let mut deadline = Deadline::at(at);

		deadline.wait().await;
		assert!(Instant::now() >= at, "returned before the deadline");
	}

	#[tokio::test(start_paused = true)]
	async fn an_unrepresentable_duration_disarms_instead_of_panicking() {
		let mut deadline = Deadline::after(Duration::MAX);
		assert_eq!(deadline.deadline(), None);
		assert!(poll_once(&mut deadline).is_pending());
	}

	#[tokio::test(start_paused = true)]
	async fn disarmed_never_fires() {
		let mut deadline = Deadline::new();
		assert!(poll_once(&mut deadline).is_pending());

		// Auto-advance would fire any armed timer well inside this window.
		let res = tokio::time::timeout(Duration::from_secs(60), deadline.wait()).await;
		assert!(res.is_err(), "a disarmed deadline fired");
	}

	#[tokio::test(start_paused = true)]
	async fn stays_ready_once_elapsed() {
		let mut deadline = Deadline::after(Duration::from_secs(1));
		deadline.wait().await;

		// Re-polling a completed timer must keep reporting the deadline as passed.
		assert!(poll_once(&mut deadline).is_ready());
		assert!(poll_once(&mut deadline).is_ready());
	}

	#[tokio::test(start_paused = true)]
	async fn re_arming_to_the_same_instant_does_not_restart() {
		let at = Instant::now() + Duration::from_secs(1);
		let mut deadline = Deadline::at(at);
		deadline.wait().await;

		// The instant really has passed, so an idempotent `set` must not rewind it.
		deadline.set(Some(at));
		assert!(poll_once(&mut deadline).is_ready());
	}

	#[tokio::test(start_paused = true)]
	async fn re_arming_later_defers_the_fire() {
		let start = Instant::now();
		let mut deadline = Deadline::after(Duration::from_secs(1));

		// Force the allocation so the re-arm goes through `Sleep::reset`.
		assert!(poll_once(&mut deadline).is_pending());

		let later = start + Duration::from_secs(10);
		deadline.set(Some(later));
		deadline.wait().await;

		assert!(Instant::now() >= later, "fired at the original deadline");
	}

	#[tokio::test(start_paused = true)]
	async fn disarming_a_live_countdown_stops_it() {
		let mut deadline = Deadline::after(Duration::from_secs(1));
		assert!(poll_once(&mut deadline).is_pending());

		deadline.set(None);
		assert_eq!(deadline.deadline(), None);

		let res = tokio::time::timeout(Duration::from_secs(60), deadline.wait()).await;
		assert!(res.is_err(), "a disarmed deadline fired");
	}

	#[tokio::test(start_paused = true)]
	async fn re_arming_after_disarm_fires_again() {
		let mut deadline = Deadline::after(Duration::from_secs(1));
		assert!(poll_once(&mut deadline).is_pending());
		deadline.set(None);

		let at = Instant::now() + Duration::from_secs(3);
		deadline.set(Some(at));
		deadline.wait().await;

		assert!(Instant::now() >= at, "returned before the re-armed deadline");
	}
}

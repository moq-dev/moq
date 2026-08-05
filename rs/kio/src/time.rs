//! Poll-driven wall-clock deadlines and retry schedules.
//!
//! [`Deadline`] is a single instant to poll on. [`Backoff`] is the escalating delay a loop waits
//! between attempts at something that failed, with a budget that eventually stops it.
//!
//! Behind the `time` feature. Built on [`web_async::time`], which is `tokio::time` on
//! native and `wasmtimer` in the browser, so the rest of kio stays runtime-free.
//!
//! On native, a timer must first be polled inside a tokio runtime with the time driver
//! enabled, or tokio panics. Reading the clock ([`Instant::now`]) has no such
//! requirement. Since the clock is tokio's, `tokio::time::pause()` advances these
//! deadlines in tests.

use std::{pin::Pin, task::Poll};

use rand::RngExt;

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

/// How long to wait between attempts, and how long to keep making them.
///
/// The defaults suit a long-lived connection: a second before the first retry, doubling to a
/// half-minute ceiling, giving up after five minutes. A one-shot request wants a much smaller
/// [`timeout`](Self::timeout); a supervisor that must never stop wants a zero one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Delay before the first retry.
	pub initial: Duration,

	/// Multiplier applied to the delay after each failure.
	pub multiplier: u32,

	/// Ceiling on the delay, however many failures have piled up.
	pub max: Duration,

	/// How long to keep trying before giving up, measured from construction or the last
	/// [`reset`](Backoff::reset). This covers the attempts themselves, not just the waits between
	/// them, so a caller whose every attempt hangs still gives up on schedule. [`Duration::ZERO`]
	/// retries forever, which only belongs in a supervisor whose job is to outlive an outage.
	pub timeout: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(30),
			timeout: Duration::from_secs(300),
		}
	}
}

/// The escalating delay a loop waits between attempts at something that failed.
///
/// Capped exponential backoff with jitter and a give-up budget. It answers *when* to try again and,
/// through the budget, when to stop. It deliberately has no opinion on *whether* a given failure is
/// worth repeating: deciding that means guessing, and a wrong guess either strands something a retry
/// would have recovered or hammers something already dead. The budget bounds the damage instead.
///
/// Each delay is drawn from the top half of the current window (equal jitter), so a fleet that fails
/// together doesn't retry together, while still waiting at least half the escalating delay. The
/// window doubles per failure up to [`Config::max`], and [`Config::timeout`] bounds the whole
/// sequence.
///
/// Call [`sleep`](Self::sleep) (or [`delay`](Self::delay), if the caller owns the waiting) after each
/// failure and [`reset`](Self::reset) after a success worth trusting. Nothing else may own a competing
/// schedule for the same operation: an outer supervisor that rebuilds an inner loop restarts its
/// backoff at the initial delay and the escalation never happens.
#[derive(Debug)]
pub struct Backoff {
	config: Config,

	/// The current window's upper bound, doubled per failure.
	window: Duration,

	/// When the budget runs out, or `None` when there isn't one.
	deadline: Option<Instant>,
}

impl Backoff {
	/// A backoff following `config`, with a full budget starting now.
	pub fn new(config: Config) -> Self {
		Self {
			window: config.initial,
			deadline: Self::deadline(&config),
			config,
		}
	}

	/// When the budget runs out, or `None` when there isn't one.
	///
	/// An unlimited budget never reads the clock, which is what lets a blocking thread with its own
	/// [`std::time::Instant`] bookkeeping drive this too.
	fn deadline(config: &Config) -> Option<Instant> {
		match config.timeout.is_zero() {
			true => None,
			// An unrepresentable deadline is treated as no deadline.
			false => Instant::now().checked_add(config.timeout),
		}
	}

	/// How long to wait before the next attempt, or `None` once the budget is spent.
	///
	/// For callers that do their own waiting (a blocking thread, a poll loop with other arms).
	/// Everything else wants [`sleep`](Self::sleep).
	pub fn delay(&mut self) -> Option<Duration> {
		let mut remaining = None;
		if let Some(deadline) = self.deadline {
			let now = Instant::now();

			// Out of budget. The clock runs from construction or the last `reset`, so it covers the
			// attempts as well as the waits between them: a caller whose every attempt hangs for
			// most of the budget would otherwise outlive it many times over.
			if now >= deadline {
				return None;
			}
			remaining = deadline.checked_duration_since(now);
		}

		let delay = self.jitter(self.window);
		self.window = self
			.window
			.saturating_mul(self.config.multiplier.max(1))
			.min(self.config.max);

		// Never sleep past the deadline: the budget says how long to keep retrying, so overshooting
		// it by a whole window would spend more than the caller asked for and skip the attempt that
		// still fit. A truncated final delay is the point, not a rounding error.
		Some(match remaining {
			Some(remaining) => delay.min(remaining),
			None => delay,
		})
	}

	/// Wait out the next delay, returning `false` once the budget is spent.
	///
	/// A `false` means stop retrying: the caller should surface the failure that got it here rather
	/// than loop again.
	pub async fn sleep(&mut self) -> bool {
		let Some(delay) = self.delay() else { return false };
		web_async::time::sleep(delay).await;
		true
	}

	/// Start over: the next delay is [`Config::initial`] again and the budget is full.
	///
	/// Only call this after an outcome that says the earlier failures no longer describe reality: a
	/// session that stayed up, a request that succeeded, a changed destination. Resetting on an
	/// attempt that failed immediately turns the escalation into a tight loop.
	pub fn reset(&mut self) {
		self.window = self.config.initial;
		self.deadline = Self::deadline(&self.config);
	}

	/// Draw the actual delay from the top half of `window`, so peers that failed together spread out.
	fn jitter(&self, window: Duration) -> Duration {
		let half = window / 2;

		// A window past ~584 years holds more nanoseconds than a `u64`, and truncating one would
		// hand `random_range` an empty range to panic on. `max` is caller-configurable (a humantime
		// string on the CLI), so saturate rather than trust it to be sane.
		let span = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
		if span == 0 {
			return window;
		}

		half.saturating_add(Duration::from_nanos(rand::rng().random_range(0..span)))
	}
}

impl Default for Backoff {
	fn default() -> Self {
		Self::new(Config::default())
	}
}

#[cfg(all(test, not(loom)))]
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

	fn config() -> Config {
		Config {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(8),
			timeout: Duration::ZERO,
		}
	}

	/// The window doubles per failure and stops at the cap, and jitter keeps every draw inside the
	/// top half of its window.
	#[tokio::test(start_paused = true)]
	async fn escalates_to_the_cap_within_the_jitter_band() {
		let mut backoff = Backoff::new(config());

		for expected in [1, 2, 4, 8, 8, 8].map(Duration::from_secs) {
			let delay = backoff.delay().expect("unlimited budget");
			assert!(
				delay >= expected / 2 && delay <= expected,
				"{delay:?} outside the jitter band for {expected:?}"
			);
		}
	}

	/// Two backoffs with the same settings must not step in lockstep, or a fleet that failed
	/// together retries together.
	#[tokio::test(start_paused = true)]
	async fn jitter_separates_identical_schedules() {
		let mut a = Backoff::new(config());
		let mut b = Backoff::new(config());

		// One shared draw could collide by chance; a run of them colliding means no jitter at all.
		let differs = (0..8).any(|_| a.delay() != b.delay());
		assert!(differs, "identical backoffs produced identical delays");
	}

	/// `max` comes from a caller-supplied humantime string, so an absurd one has to degrade rather
	/// than panic: a window past ~584 years has more nanoseconds than the jitter sample can hold.
	#[tokio::test(start_paused = true)]
	async fn an_absurd_window_does_not_panic() {
		let mut backoff = Backoff::new(Config {
			initial: Duration::new(36_893_488_147, 419_103_232),
			max: Duration::MAX,
			..config()
		});

		let delay = backoff.delay().expect("unlimited budget");
		assert!(
			delay >= Duration::new(18_446_744_073, 709_551_616),
			"{delay:?} below half"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn reset_returns_to_the_initial_window() {
		let mut backoff = Backoff::new(config());
		for _ in 0..4 {
			backoff.delay();
		}

		backoff.reset();
		let delay = backoff.delay().expect("unlimited budget");
		assert!(delay <= Duration::from_secs(1), "{delay:?} did not return to initial");
	}

	/// An initial delay longer than the whole budget must not sleep past it: the budget is the
	/// promise, and one oversized window would blow through it before a single retry lands.
	#[tokio::test(start_paused = true)]
	async fn a_delay_never_outlives_the_budget() {
		let mut backoff = Backoff::new(Config {
			initial: Duration::from_secs(60),
			timeout: Duration::from_millis(50),
			..config()
		});

		let delay = backoff.delay().expect("budget available");
		assert!(delay <= Duration::from_millis(50), "{delay:?} outlived the budget");
	}

	/// The budget is a wall-clock deadline over the whole sequence, not a per-attempt one, and the
	/// sequence lands on it rather than overshooting by a whole window.
	#[tokio::test(start_paused = true)]
	async fn gives_up_once_the_budget_is_spent() {
		let timeout = Duration::from_secs(10);
		let mut backoff = Backoff::new(Config { timeout, ..config() });

		let started = tokio::time::Instant::now();
		while let Some(delay) = backoff.delay() {
			tokio::time::sleep(delay).await;
			assert!(started.elapsed() < Duration::from_secs(60), "budget never ran out");
		}

		// Tokio's paused clock rounds each sleep to its timer granularity, so the sequence can land a
		// hair either side of the deadline it aimed for.
		let elapsed = started.elapsed();
		assert!(
			elapsed >= timeout - Duration::from_millis(10),
			"gave up after only {elapsed:?}"
		);
		assert!(
			elapsed < timeout + config().max,
			"overshot the budget by a whole window: {elapsed:?}"
		);
	}

	/// A zero timeout is the supervisor case: keep retrying however long the outage lasts.
	#[tokio::test(start_paused = true)]
	async fn a_zero_timeout_never_gives_up() {
		let mut backoff = Backoff::new(config());
		for _ in 0..64 {
			assert!(backoff.sleep().await);
		}
	}

	/// The budget covers the retry sequence, so a reset after a healthy stretch buys a fresh one.
	#[tokio::test(start_paused = true)]
	async fn reset_refills_the_budget() {
		let mut backoff = Backoff::new(Config {
			timeout: Duration::from_secs(10),
			..config()
		});

		while backoff.sleep().await {}
		backoff.reset();
		assert!(backoff.sleep().await, "reset did not refill the budget");
	}
}

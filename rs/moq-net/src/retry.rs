//! The retry schedule shared by every loop that re-attempts a failed operation.
//!
//! Two halves, kept apart on purpose. [`Error::is_retryable`](crate::Error::is_retryable) (and its
//! counterparts in the crates above) answers *whether* an attempt is worth repeating; [`Backoff`]
//! answers *when*. A loop that only has the second half retries deterministic failures forever, which
//! is the bug this module exists to prevent, so classify first and back off second.
//!
//! ```no_run
//! # async fn example() -> Result<(), moq_net::Error> {
//! # async fn attempt() -> Result<(), moq_net::Error> { Ok(()) }
//! let mut backoff = moq_net::retry::Backoff::default();
//! loop {
//!     match attempt().await {
//!         Ok(()) => return Ok(()),
//!         // Deterministic: the next attempt fails the same way, so surface it now.
//!         Err(err) if !err.is_retryable() => return Err(err),
//!         // Transient, but the budget is spent: stop rather than retry forever.
//!         Err(err) if !backoff.sleep().await => return Err(err),
//!         Err(_) => continue,
//!     }
//! }
//! # }
//! ```

use kio::time::{Duration, Instant};
use rand::RngExt;

/// Whether an OS-level failure is worth another attempt.
///
/// Configuration mistakes reach a caller as [`std::io::Error`] too: a path that doesn't exist, a
/// port another process holds, an address this host can't bind. Those repeat forever. What's left
/// (refused, unreachable, reset, timed out) is the network being the network.
pub fn io_retryable(err: &std::io::Error) -> bool {
	!matches!(
		err.kind(),
		std::io::ErrorKind::NotFound
			| std::io::ErrorKind::PermissionDenied
			| std::io::ErrorKind::AddrInUse
			| std::io::ErrorKind::AddrNotAvailable
			| std::io::ErrorKind::InvalidInput
			| std::io::ErrorKind::InvalidData
			| std::io::ErrorKind::Unsupported
	)
}

/// Whether an HTTP response status means "ask again later".
///
/// A response that arrived is the server's answer, and only this narrow set invites another attempt:
/// request timeout, rate limit, and the gateway/overload statuses. Every other status, `404` and
/// `403` included, is settled. A request that got *no* response is a transport failure and doesn't
/// come through here.
pub fn status_retryable(status: u16) -> bool {
	matches!(status, 408 | 429 | 502 | 503 | 504)
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

	/// How long to keep retrying before giving up, measured from the first delay after a
	/// [`reset`](Backoff::reset). [`Duration::ZERO`] retries forever, which only belongs in a
	/// supervisor whose job is to outlive an outage.
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

/// A capped exponential backoff with jitter and a give-up budget.
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

	/// When the budget runs out, or `None` while the sequence hasn't started (or never expires).
	deadline: Option<Instant>,
}

impl Backoff {
	/// A backoff following `config`, with a full budget.
	pub fn new(config: Config) -> Self {
		Self {
			window: config.initial,
			config,
			deadline: None,
		}
	}

	/// How long to wait before the next attempt, or `None` once the budget is spent.
	///
	/// For callers that do their own waiting (a blocking thread, a poll loop with other arms).
	/// Everything else wants [`sleep`](Self::sleep).
	pub fn delay(&mut self) -> Option<Duration> {
		// An unlimited budget never reads the clock, which is what lets a blocking thread with its
		// own [`std::time::Instant`] bookkeeping drive this too.
		if !self.config.timeout.is_zero() {
			let now = Instant::now();
			match self.deadline {
				// Started already: stop once the budget is gone.
				Some(deadline) if now >= deadline => return None,
				Some(_) => {}
				// The first delay of a sequence starts the clock. Deferred to here rather than to
				// `new`/`reset` so a loop that runs healthy for hours still gets its full budget
				// when it finally does fail.
				None => self.deadline = now.checked_add(self.config.timeout),
			}
		}

		let delay = self.jitter(self.window);
		self.window = self
			.window
			.saturating_mul(self.config.multiplier.max(1))
			.min(self.config.max);

		Some(delay)
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
		self.deadline = None;
	}

	/// Draw the actual delay from the top half of `window`, so peers that failed together spread out.
	fn jitter(&self, window: Duration) -> Duration {
		let half = window / 2;
		match half.is_zero() {
			true => window,
			false => half + Duration::from_nanos(rand::rng().random_range(0..half.as_nanos() as u64)),
		}
	}
}

impl Default for Backoff {
	fn default() -> Self {
		Self::new(Config::default())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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

	/// The budget is a wall-clock deadline over the whole sequence, not a per-attempt one.
	#[tokio::test(start_paused = true)]
	async fn gives_up_once_the_budget_is_spent() {
		let mut backoff = Backoff::new(Config {
			timeout: Duration::from_secs(10),
			..config()
		});

		let mut slept = Duration::ZERO;
		while let Some(delay) = backoff.delay() {
			slept += delay;
			tokio::time::sleep(delay).await;
			assert!(slept < Duration::from_secs(60), "budget never ran out");
		}

		assert!(slept >= Duration::from_secs(10), "gave up after only {slept:?}");
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

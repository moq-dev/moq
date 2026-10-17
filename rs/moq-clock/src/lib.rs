//! One clock for Media over QUIC.
//!
//! - [`Instant`] is a logical point on the monotonic clock, in nanoseconds from
//!   the clock's origin, with [`std::time::Instant`]'s methods. [`Wall`] is
//!   wall time from the same clock, kept a separate type so wall time never
//!   measures elapsed time.
//! - Caller-driven state machines take `now` and never read a clock. They arm
//!   [`Deadline`]s on a [`Deadlines`] queue, and tests step them with a
//!   [`Manual`] clock.
//! - Async code reads the ambient clock: [`now`], [`wall`], [`sleep`],
//!   [`sleep_until`], [`timeout`], [`interval`], and [`drive`] resolve to the
//!   current runtime's clock. That is tokio's (paused tokio included) on native,
//!   `performance.now()` and `setTimeout` in the browser, and the simulator inside
//!   `sim::run`.
//!
//! The `sim` feature adds the `sim` module, a deterministic executor whose clock
//! starts at [`Instant::ZERO`], and the `#[moq_clock::test]` attribute that runs an
//! `async fn` test on it. Enable it for dev-dependencies only.

mod deadline;
mod instant;
mod manual;
mod runtime;
#[cfg(feature = "sim")]
pub mod sim;
mod timer;
mod wall;

pub use deadline::{Deadline, Deadlines};
pub use instant::Instant;
pub use manual::Manual;
pub use timer::{Elapsed, Interval, MissedTickBehavior, Sleep, Timeout, drive, interval, sleep, sleep_until, timeout};
pub use wall::{OutOfRange, Wall};

/// Run an `async fn` test to completion on [`sim::run`].
#[cfg(feature = "sim")]
pub use moq_clock_macros::test;

// So `#[moq_clock::test]` resolves inside this crate's own tests.
#[cfg(test)]
extern crate self as moq_clock;

/// The current instant on the ambient clock.
pub fn now() -> Instant {
	#[cfg(feature = "sim")]
	if let Some(now) = sim::now() {
		return now;
	}
	runtime::now()
}

/// The current wall time on the ambient clock.
///
/// Paused tokio does not pause wall time; use the simulator to control both.
///
/// # Panics
///
/// When the host's wall clock reads before 1970.
pub fn wall() -> Wall {
	#[cfg(feature = "sim")]
	if let Some(wall) = sim::wall() {
		return wall;
	}
	runtime::wall()
}

/// The first monotonic and wall readings observed on the ambient clock.
///
/// Unlike [`wall`], the wall half does not change after a clock correction.
pub fn anchor() -> (Instant, Wall) {
	#[cfg(feature = "sim")]
	if let Some(anchor) = sim::anchor() {
		return anchor;
	}
	runtime::anchor()
}

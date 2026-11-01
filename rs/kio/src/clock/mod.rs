//! One clock: logical instants, wall time, deadlines, timers, and a deterministic simulator.
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
//!   current runtime's clock. That is tokio's (paused tokio included), the
//!   simulator inside `sim::run`, and a [`Deadlines`] queue on a thread that
//!   [entered](Deadlines::enter) it, which is how a runtime that reads the host
//!   clock itself (an io_uring worker) becomes a backend.
//!
//! The `runtime` feature arms timers on tokio's clock. Without it, reads fall back
//! to std, and waiting on the host clock panics.
//!
//! The `manual` feature adds `Manual::install`, which makes a [`Manual`] clock
//! the ambient clock of one tokio runtime. It moves only when told to, never on
//! idle, so a test harness outside the runtime decides when native time passes.
//!
//! The `sim` feature adds the `sim` module, a deterministic executor whose clock
//! starts at [`Instant::ZERO`], and the `#[kio::test]` attribute that runs an
//! `async fn` test on it. Enable it for dev-dependencies only.
//!
//! The `serde` feature adds `serde::UnixSeconds`, matching existing integer
//! Unix-second JSON encodings for wall times.

mod deadline;
#[cfg(feature = "manual")]
mod installed;
mod instant;
mod manual;
mod runtime;
#[cfg(feature = "serde")]
pub mod serde;
#[cfg(feature = "sim")]
pub mod sim;
mod timer;
mod wall;

pub use deadline::{Deadline, Deadlines, Entered};
#[cfg(feature = "manual")]
pub use installed::Installed;
pub use instant::Instant;
pub use manual::Manual;
pub use timer::{
	Elapsed, Interval, MissedTickBehavior, Sleep, Timeout, drive, interval, interval_at, sleep, sleep_until, timeout,
	timeout_at,
};
pub use wall::{OutOfRange, Wall};

/// The current instant on the ambient clock.
pub fn now() -> Instant {
	if let Some(now) = deadline::now() {
		return now;
	}
	#[cfg(feature = "sim")]
	if let Some(now) = sim::now() {
		return now;
	}
	#[cfg(feature = "manual")]
	if let Some(clock) = installed::current() {
		return clock.now();
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
	#[cfg(feature = "manual")]
	if let Some(clock) = deadline::installed() {
		return clock.wall();
	}
	#[cfg(feature = "sim")]
	if let Some(wall) = sim::wall() {
		return wall;
	}
	#[cfg(feature = "manual")]
	if let Some(clock) = installed::current() {
		return clock.wall();
	}
	runtime::wall()
}

/// The ambient clock's origin: an instant and its wall time, fixed at the first read.
///
/// It maps instants onto wall time and, unlike [`wall`], does not move after a clock
/// correction. `@moq/clock`'s `origin()` returns only the wall time of instant zero, in
/// milliseconds; here [`Wall`] is nanoseconds and comes paired with an instant, since a
/// runtime clock's instant zero predates 1970.
pub fn origin() -> (Instant, Wall) {
	#[cfg(feature = "manual")]
	if let Some(clock) = deadline::installed() {
		return clock.anchor();
	}
	#[cfg(feature = "sim")]
	if let Some(anchor) = sim::anchor() {
		return anchor;
	}
	#[cfg(feature = "manual")]
	if let Some(clock) = installed::current() {
		return clock.anchor();
	}
	runtime::anchor()
}

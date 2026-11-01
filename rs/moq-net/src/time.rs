//! Caller-supplied time for protocol and model drivers.
//!
//! Drivers take the current [`Instant`] on every poll and never read a clock.

pub use kio::clock::Instant;
pub(crate) use kio::clock::{Deadline, Deadlines};

use crate::Error;

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
#[cfg(feature = "async")]
pub async fn run<D: Driver>(driver: D) -> Error {
	let mut driver = Box::new(driver);
	kio::clock::drive(move |now, waiter| driver.poll(now, waiter)).await
}

/// A deadline queue attached to the deterministic test clock.
#[cfg(all(test, feature = "async"))]
pub(crate) fn sim() -> Deadlines {
	let deadlines = Deadlines::new(kio::clock::now());
	kio::clock::sim::attach(&deadlines);
	deadlines
}

//! The model's only reads of the local clock.
//!
//! Everything else takes time from its caller: drivers are polled with an instant,
//! and cache pools collect at the one they are handed. What reads the clock here is
//! the explicit [`Timestamp::now`](crate::Timestamp::now) convenience and the
//! [`anchor`] that maps an instant onto a timestamp, so a port swaps this module
//! for its platform's clock.

use std::sync::LazyLock;
use std::time::Duration;

use rand::RngExt;

/// The current instant on the model's clock.
pub(crate) fn now() -> crate::time::Instant {
	crate::time::Instant::now()
}

/// Where [`Timestamp`](crate::Timestamp)s count from: an instant, and the
/// timestamp it maps to.
///
/// A timestamp isn't a real clock; it only needs to be non-negative and roughly
/// monotonic with wall time. A random per-process jitter makes it read late, which
/// deters using it as a wall clock and catches implementations that assume
/// unrelated broadcasts are synchronized.
pub(crate) fn anchor() -> (crate::time::Instant, Duration) {
	static ANCHOR: LazyLock<(crate::time::Instant, Duration)> = LazyLock::new(|| {
		let jitter = Duration::from_millis(rand::rng().random_range(1..69_420));
		(now(), offset(jitter))
	});
	*ANCHOR
}

/// Wall time since 2020-01-01T00:00:00Z, less the jitter.
///
/// Counting from 50 years after the Unix epoch keeps the value ~1.5e12 ms smaller,
/// trimming a byte or two off the first frame's varint. Saturates to zero on a wall
/// clock set before 2020 (an unsynced clock), since only a non-negative start matters.
#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
fn offset(jitter: Duration) -> Duration {
	const EPOCH: Duration = Duration::from_secs(1_577_836_800);
	let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
	since.unwrap_or_default().saturating_sub(EPOCH).saturating_sub(jitter)
}

/// The browser build has no wall clock, so timestamps count up from the jitter.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
fn offset(jitter: Duration) -> Duration {
	jitter
}

//! The model's only reads of the local clock.
//!
//! Everything else takes time from its caller: drivers are polled with an instant,
//! and cache pools collect at the one they are handed. What reads the clock here is
//! the explicit [`Timestamp::now`](crate::Timestamp::now) convenience and the
//! [`anchor`] that maps an instant onto a timestamp, so a port swaps this module
//! for its platform's clock.
//!
//! Tests share a frozen thread-local clock so advancing one test never affects
//! another. Advancing it also dates pending cache activity; expiration remains a
//! separate operation, driven by writes or an explicit cleanup call.

use std::sync::LazyLock;
use std::time::Duration;

use rand::RngExt;

/// The current instant on the model's clock.
#[cfg(not(test))]
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

/// The current instant on the model's clock: frozen at test-thread start until
/// [`advance`] moves it.
#[cfg(test)]
pub(crate) fn now() -> crate::time::Instant {
	BASE.with(|base| *base) + OFFSET.with(std::cell::Cell::get)
}

/// Move the model's clock forward. Test-only; production time moves itself.
#[cfg(test)]
pub(crate) fn advance(duration: std::time::Duration) {
	poll_pools();
	OFFSET.with(|offset| {
		offset.set(
			offset
				.get()
				.checked_add(duration)
				.expect("advance overflows the test clock"),
		);
	});
	poll_pools();
}

#[cfg(test)]
thread_local! {
	static BASE: crate::time::Instant = crate::time::Instant::now();
	static OFFSET: std::cell::Cell<std::time::Duration> = const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::time::Duration;

	#[test]
	fn frozen_until_advanced() {
		let a = super::now();
		std::thread::sleep(Duration::from_millis(5));
		assert_eq!(a, super::now(), "the test clock moved on its own");

		super::advance(Duration::from_secs(3));
		assert_eq!(super::now(), a + Duration::from_secs(3));
	}

	#[test]
	fn advances_are_isolated_between_threads() {
		let before = super::now();
		std::thread::spawn(|| {
			let before = super::now();
			super::advance(Duration::from_secs(7));
			assert_eq!(super::now(), before + Duration::from_secs(7));
		})
		.join()
		.unwrap();

		assert_eq!(super::now(), before, "another test thread advanced this clock");
	}
}

#[cfg(test)]
thread_local! {
	static POOLS: std::cell::RefCell<Vec<crate::cache::PoolWeak>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn register(pool: &crate::cache::Pool) {
	pool.advance_test(now());
	POOLS.with(|pools| pools.borrow_mut().push(pool.downgrade()));
}

#[cfg(test)]
fn poll_pools() {
	let pools: Vec<_> = POOLS.with(|pools| {
		let mut pools = pools.borrow_mut();
		pools.retain(|pool| pool.upgrade().is_some());
		pools.iter().filter_map(|pool| pool.upgrade()).collect()
	});
	for pool in pools {
		pool.advance_test(now());
	}
}

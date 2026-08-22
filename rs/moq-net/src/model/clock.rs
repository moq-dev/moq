//! The model's clock: the real monotonic clock in production, paused and
//! manually advanced under `cfg(test)`.
//!
//! Model time is passive measurement against the model's own stamps (arrival
//! ordering vs a latency budget, cache access ticks, datagram age). Nothing
//! here arms a wakeup, so the model needs no runtime handle, and instants
//! minted here never cross into runtime-armed deadlines (durations may). The
//! test clock exists purely so timing tests are deterministic: it starts
//! frozen and only [`advance`] moves it, minting real `Instant`s as base plus
//! offset. (Crates like `mock_instant` do this by substituting the `Instant`
//! type; keeping std's type is the point, so this stays hand-rolled.)
//!
//! The paused state is process-global, which is safe because tests run under
//! nextest, one process per test. Under plain `cargo test` (shared process)
//! concurrent tests would race the offset; don't do that.

/// The current instant on the model's clock.
#[cfg(not(test))]
pub(crate) fn now() -> crate::runtime::Instant {
	crate::runtime::Instant::now()
}

/// The current instant on the model's clock: frozen at process start until
/// [`advance`] moves it.
#[cfg(test)]
pub(crate) fn now() -> crate::runtime::Instant {
	*base() + std::time::Duration::from_nanos(OFFSET_NANOS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Move the model's clock forward. Test-only; production time moves itself.
#[cfg(test)]
pub(crate) fn advance(duration: std::time::Duration) {
	let nanos = u64::try_from(duration.as_nanos()).expect("advance overflows the test clock");
	OFFSET_NANOS.fetch_add(nanos, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
static OFFSET_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn base() -> &'static crate::runtime::Instant {
	static BASE: std::sync::OnceLock<crate::runtime::Instant> = std::sync::OnceLock::new();
	BASE.get_or_init(crate::runtime::Instant::now)
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
}

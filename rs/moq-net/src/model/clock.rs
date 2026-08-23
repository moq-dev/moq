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
//! The paused state is thread-local so the standard test harness can run tests
//! concurrently in one process without one test aging another's model.

/// The current instant on the model's clock.
#[cfg(not(test))]
pub(crate) fn now() -> crate::runtime::Instant {
	crate::runtime::Instant::now()
}

/// The current instant on the model's clock: frozen at test-thread start until
/// [`advance`] moves it.
#[cfg(test)]
pub(crate) fn now() -> crate::runtime::Instant {
	BASE.with(|base| *base) + OFFSET.with(std::cell::Cell::get)
}

/// Move the model's clock forward. Test-only; production time moves itself.
#[cfg(test)]
pub(crate) fn advance(duration: std::time::Duration) {
	OFFSET.with(|offset| {
		offset.set(
			offset
				.get()
				.checked_add(duration)
				.expect("advance overflows the test clock"),
		);
	});
}

#[cfg(test)]
thread_local! {
	static BASE: crate::runtime::Instant = crate::runtime::Instant::now();
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

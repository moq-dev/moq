//! Clock access for the explicit `Timestamp::now` convenience API and model tests.
//!
//! Production cache maintenance uses caller-supplied instants at the GC boundary. Tests share a frozen thread-local clock so advancing one test never
//! affects another. Advancing it also dates pending cache activity; expiration
//! remains a separate operation, driven by writes or an explicit cleanup call.

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

#![cfg(loom)]

// Exercise the private primitive with Loom's atomics without exporting test-only API.
use moq_net::Timestamp;
mod time {
	pub use moq_net::time::Instant;
}
#[allow(dead_code)]
#[path = "../src/model/expiry.rs"]
mod expiry;

mod loom {
	use super::expiry::Wakes;
	use ::loom::sync::Arc;
	use ::loom::sync::atomic::{AtomicBool, Ordering};
	use ::loom::{future::block_on, thread};
	use std::task::Poll;

	fn landing_race(held: bool) {
		::loom::model(move || {
			let shared = Arc::new((Wakes::default(), AtomicBool::new(false)));
			let writer = shared.clone();
			let publisher = thread::spawn(move || {
				writer.1.store(true, Ordering::Relaxed);
				writer.0.landed(None, 1, 0);
			});
			block_on(kio::wait(|waiter| {
				if held {
					shared.0.watch_held(0, waiter);
				} else {
					shared.0.watch_landing(0, waiter);
				}
				if shared.1.load(Ordering::Relaxed) {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			}));
			publisher.join().unwrap();
		});
	}

	#[test]
	fn a_landing_wakes_a_registered_reader() {
		landing_race(false);
	}

	#[test]
	fn a_landing_wakes_a_registered_held_reader() {
		landing_race(true);
	}

	#[test]
	fn concurrent_landings_keep_the_hint_armed() {
		::loom::model(|| {
			let shared = Arc::new((Wakes::default(), AtomicBool::new(false)));
			let earlier = shared.clone();
			let first = thread::spawn(move || earlier.0.landed(None, 1, 0));
			let later = shared.clone();
			let second = thread::spawn(move || {
				later.1.store(true, Ordering::Relaxed);
				later.0.landed(Some(1), 2, 0);
			});
			block_on(kio::wait(|waiter| {
				shared.0.watch_landing(1, waiter);
				if shared.1.load(Ordering::Relaxed) {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			}));
			first.join().unwrap();
			second.join().unwrap();
		});
	}
}

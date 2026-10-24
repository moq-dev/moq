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
	use ::loom::sync::{Arc, RwLock};
	use ::loom::{future::block_on, thread};
	use std::task::Poll;

	fn insertion_race(held: bool) {
		::loom::model(move || {
			let shared = Arc::new((Wakes::default(), RwLock::new(false)));
			let writer = shared.clone();
			let publisher = thread::spawn(move || {
				let mut track = writer.1.write().unwrap();
				*track = true;
				if writer.0.has_landing_waiters() {
					writer.0.landed(None, 1, 0);
				}
			});
			block_on(kio::wait(|waiter| {
				let track = shared.1.read().unwrap();
				if held {
					shared.0.watch_held(0, waiter);
				} else {
					shared.0.watch_landing(0, waiter);
				}
				if *track { Poll::Ready(()) } else { Poll::Pending }
			}));
			publisher.join().unwrap();
		});
	}

	#[test]
	fn insertion_observes_a_registered_reader_or_the_reader_observes_it() {
		insertion_race(false);
	}

	#[test]
	fn insertion_observes_a_registered_held_reader_or_the_reader_observes_it() {
		insertion_race(true);
	}
}

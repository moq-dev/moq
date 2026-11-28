//! Admission guard cost across serving threads and already admitted connections.
//!
//! Run with `cargo bench -p moq-net --features async --bench admission`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::{
	fmt,
	sync::Barrier,
	time::{Duration, Instant},
};

use moq_net::server::{Admission, Admitted};
use swole::{Iterations, bench};

/// Threads entering at once and guards already held, named `workers=<workers>/<held>`.
#[derive(Clone, Copy)]
struct Case {
	workers: usize,
	held: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "workers={}/{}", self.workers, self.held)
	}
}

fn cases() -> Vec<Case> {
	[1, 4, 16]
		.into_iter()
		.flat_map(|workers| [0, 64, 4096].map(|held| Case { workers, held }))
		.collect()
}

struct Admitting {
	workers: usize,
	admission: Admission,
	_held: Vec<Option<Admitted>>,
}

fn admitting(case: Case) -> Admitting {
	let admission = Admission::default();
	let _held = (0..case.held).map(|_| admission.enter()).collect();
	Admitting {
		workers: case.workers,
		admission,
		_held,
	}
}

/// Times from the barrier's release to the last worker's finish, leaving thread spawn out.
#[bench(args = cases(), setup = admitting, items = |case: Case| case.workers)]
fn server_admission(input: &Admitting, iterations: Iterations) -> Duration {
	let barrier = Barrier::new(input.workers + 1);
	std::thread::scope(|scope| {
		let barrier = &barrier;
		let handles: Vec<_> = (0..input.workers)
			.map(|_| {
				let admission = &input.admission;
				scope.spawn(move || {
					barrier.wait();
					for _ in iterations {
						drop(std::hint::black_box(admission.enter()));
					}
				})
			})
			.collect();
		let start = Instant::now();
		barrier.wait();
		for handle in handles {
			handle.join().unwrap();
		}
		start.elapsed()
	})
}

fn main() {
	swole::main();
}

//! Admission guard cost across serving threads and already admitted connections.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::{sync::Barrier, time::Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::server::Admission;

fn admission(c: &mut Criterion) {
	let mut group = c.benchmark_group("server_admission");
	for workers in [1, 4, 16] {
		for held in [0, 64, 4096] {
			let admission = Admission::default();
			let _held: Vec<_> = (0..held).map(|_| admission.enter()).collect();
			group.throughput(Throughput::Elements(workers as u64));
			group.bench_function(BenchmarkId::new(format!("workers={workers}"), held), |b| {
				b.iter_custom(|iterations| {
					let barrier = Barrier::new(workers + 1);
					std::thread::scope(|scope| {
						let barrier = &barrier;
						let handles: Vec<_> = (0..workers)
							.map(|_| {
								let admission = &admission;
								scope.spawn(move || {
									barrier.wait();
									for _ in 0..iterations {
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
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, admission);
criterion_main!(benches);

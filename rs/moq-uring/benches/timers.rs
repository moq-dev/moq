//! Timer churn on a worker's queue: a QUIC driver re-arms its timer every
//! loop, so the cost of a re-arm, swept over how many other timers are armed,
//! shows whether the queue's cost grows with the table.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
fn benchmark(c: &mut criterion::Criterion) {
	use std::time::Duration;

	use criterion::BenchmarkId;
	use moq_uring::{Config, Error, Worker};

	let worker = match Worker::new(Config::default()) {
		Ok(worker) => worker,
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping timer benchmark: {reason}");
			return;
		}
		Err(err) => panic!("worker setup failed: {err}"),
	};
	let handle = worker.handle();

	let mut group = c.benchmark_group("timers");
	for armed in [1, 64, 1024, 16384] {
		// The rest of the table, spread across a minute so the re-armed timer
		// lands among them.
		let now = kio::clock::now();
		let _others: Vec<_> = (0..armed)
			.map(|i| {
				let mut timer = handle.timer();
				timer.set(Some(now + Duration::from_secs(60) * i / armed));
				timer
			})
			.collect();

		// Two instants a re-arm alternates between, so every set moves it.
		let mut timer = handle.timer();
		let at = [now + Duration::from_secs(30), now + Duration::from_secs(31)];
		let mut flip = 0;
		group.bench_function(BenchmarkId::new("rearm", armed), |b| {
			b.iter(|| {
				flip ^= 1;
				timer.set(Some(at[flip]));
			});
		});
	}
	group.finish();
}

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);

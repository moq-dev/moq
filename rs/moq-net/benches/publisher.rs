//! The lite publisher's coverage check before SUBSCRIBE_START.
//!
//! `publisher_covered_before_start` holds a bounded lite-07 subscription to `RANGES` ranges
//! whose source gave up `skipped` of them and has yet to resolve a start, then wakes it with
//! nothing new to serve. Each wake ends in the check of whether every range was given up.
//! The range count stays fixed, so a cost that grows with the ranges already given up shows
//! up as a slope over `skipped`. `adjacent` gives up the groups of one range instead, each
//! with a different error from its neighbor, so the record holds one entry per group.
//!
//! `publisher_drop` gives up `drops` ranges of a subscription to `ranges` ranges, plus one
//! left open, timing each drop and every wake it causes. Each drop starts the range it gives
//! up, so a drop costing every range shows as a slope over `ranges`, and one costing every
//! drop before it as a slope over `drops`.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench publisher`.

use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::CoverageBench;

/// Ranges the source gave up before the wake.
const SKIPPED: [u64; 4] = [1, 32, 1024, 8192];

/// Ranges subscribed: one more than the most given up, so one stays open.
const RANGES: u64 = 8193;

fn bench_covered_before_start(c: &mut Criterion) {
	let mut group = c.benchmark_group("publisher_covered_before_start");
	group.throughput(Throughput::Elements(1));
	for skipped in SKIPPED {
		// Criterion calls the closure once per sample, so the fixture is built on the first call and kept.
		let mut run = None;
		group.bench_function(BenchmarkId::new("wake", skipped), |b| {
			let run = run.get_or_insert_with(|| CoverageBench::new(RANGES, skipped));
			b.iter(|| run.wake());
		});
		// The same record within one range, each group its own entry.
		let mut run = None;
		group.bench_function(BenchmarkId::new("adjacent", skipped), |b| {
			let run = run.get_or_insert_with(|| CoverageBench::adjacent(skipped));
			b.iter(|| run.wake());
		});
	}
	group.finish();
}

/// Ranges given up per measurement, and ranges subscribed besides the open one.
const SWEEP: [u64; 4] = [1, 32, 1024, 8192];

fn bench_drop(c: &mut Criterion) {
	let mut group = c.benchmark_group("publisher_drop");
	// The largest cell gives up 8192 ranges per sample.
	group.sample_size(10);
	group.measurement_time(Duration::from_secs(2));
	for ranges in SWEEP {
		for drops in SWEEP.into_iter().filter(|drops| *drops <= ranges) {
			group.throughput(Throughput::Elements(drops));
			group.bench_function(
				BenchmarkId::new(format!("ranges={ranges}"), format!("drops={drops}")),
				|b| {
					b.iter_batched(
						|| CoverageBench::spread(ranges + 1, drops),
						|mut run| {
							run.give_up(drops);
							run
						},
						BatchSize::PerIteration,
					)
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, bench_covered_before_start, bench_drop);
criterion_main!(benches);

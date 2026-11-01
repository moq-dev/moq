//! The lite publisher's coverage check before SUBSCRIBE_START.
//!
//! `publisher_covered_before_start` holds a bounded lite-07 subscription to `RANGES` ranges
//! whose source gave up `skipped` of them and has yet to resolve a start, then wakes it with
//! nothing new to serve. Each wake ends in the check of whether every range was given up.
//! The range count stays fixed, so a cost that grows with the ranges already given up shows
//! up as a slope over `skipped`.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench publisher`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::CoverageBench;

/// Ranges the source gave up before the wake.
const SKIPPED: [u64; 4] = [1, 32, 1024, 8192];

/// Ranges subscribed: one more than the most given up, so one stays open.
const RANGES: u64 = 8193;

fn bench_covered_before_start(c: &mut Criterion) {
	let mut group = c.benchmark_group("publisher_covered_before_start");
	group.throughput(Throughput::Elements(1));
	for skipped in SKIPPED {
		group.bench_function(BenchmarkId::new("wake", skipped), |b| {
			let mut run = CoverageBench::new(RANGES, skipped);
			b.iter(|| run.wake());
		});
	}
	group.finish();
}

criterion_group!(benches, bench_covered_before_start);
criterion_main!(benches);

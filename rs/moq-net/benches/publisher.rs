//! The lite publisher's coverage check before SUBSCRIBE_START.
//!
//! `publisher_covered_before_start` holds a bounded lite-07 subscription to `RANGES` ranges
//! whose source gave up `skipped` of them and has yet to resolve a start, then wakes it with
//! nothing new to serve. Each wake ends in the check of whether every range was given up.
//! The range count stays fixed, so a cost that grows with the ranges already given up shows
//! up as a slope over `skipped`. `adjacent` gives up the groups of one range instead, each
//! with a different error from its neighbor, so the record holds one entry per group.
//!
//! `publisher_paused_frames` holds `paused` unfinished paused groups and writes `frames`
//! frames to them per wake, one group each in turn, before the run checks its paused groups
//! for one the budget or the cache gave up. A wake re-polls only the groups written to, so
//! the cost per frame stays flat over `paused`.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench publisher`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::time::{Duration, Instant};

use moq_net::fuzz::{CoverageBench, PausedBench};

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

/// Paused groups the run holds.
const PAUSED: [u64; 4] = [1, 32, 1024, 8192];

/// Frames written to paused groups per wake.
const FRAMES: [usize; 3] = [1, 16, 256];

/// Wakes per fixture: each frame stays in its group, so the fixture is rebuilt, untimed,
/// before it holds too many.
const WAKES: u64 = 1024;

fn bench_paused_frames(c: &mut Criterion) {
	let mut group = c.benchmark_group("publisher_paused_frames");
	for paused in PAUSED {
		for frames in FRAMES {
			group.throughput(Throughput::Elements(frames as u64));
			group.bench_function(BenchmarkId::new(format!("paused={paused}"), frames), |b| {
				b.iter_custom(|iters| {
					let mut total = Duration::ZERO;
					let mut left = iters;
					while left > 0 {
						let mut run = PausedBench::new(paused);
						let wakes = left.min(WAKES);
						let start = Instant::now();
						for _ in 0..wakes {
							run.wake(frames);
						}
						total += start.elapsed();
						left -= wakes;
					}
					total
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench_covered_before_start, bench_paused_frames);
criterion_main!(benches);

//! Cost of canonical cache sizing and final frame-storage cleanup.
//! Each case retains real frame owners; cleanup includes their weak-record removals,
//! without reporting deferred log retirement as completed physical storage cleanup.
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz::CanonicalStorage;
use std::{hint::black_box, time::Duration};

fn canonical(c: &mut Criterion) {
	let mut group = c.benchmark_group("canonical_storage");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	for frames in [1, 64, 1024, 8192] {
		for views in [1, 8] {
			let id = format!("{frames}x{views}");
			let stored = CanonicalStorage::new(frames, views);
			group.bench_function(BenchmarkId::new("size", &id), |b| b.iter(|| black_box(stored.size())));
			drop(stored);
			group.bench_function(BenchmarkId::new("cleanup", &id), |b| {
				b.iter_batched(|| CanonicalStorage::new(frames, views), drop, BatchSize::PerIteration)
			});
		}
	}
	group.finish();
}
criterion_group!(benches, canonical);
criterion_main!(benches);

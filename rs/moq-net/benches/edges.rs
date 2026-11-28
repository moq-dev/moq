//! A frame write on a session at its live-edge budget, which reclaims on every write.
//!
//! `reclaim` holds `quiet` edges that moved first, each with a frame nobody read, and writes
//! frames to one more edge whose reader takes each as it lands, so the frames to shed are
//! that edge's alone. A reclaim that looks at every edge shows a slope over the quiet edges;
//! the frame size sets how many bytes each write pushes past the budget.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench edges`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::LiveEdges;
use std::time::Duration;

/// Edges holding a frame nobody read, ranked to pay before the written one.
const QUIET: [usize; 3] = [1, 32, 1024];

/// Bytes per frame written.
const FRAME: [usize; 2] = [1024, 64 * 1024];

fn reclaim(c: &mut Criterion) {
	let mut group = c.benchmark_group("reclaim");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	group.throughput(Throughput::Elements(1));
	for quiet in QUIET {
		for frame in FRAME {
			let mut edges = LiveEdges::new(quiet, frame);
			group.bench_function(BenchmarkId::new(format!("{frame}B"), quiet), |b| {
				b.iter(|| edges.write())
			});
		}
	}
	group.finish();
}

criterion_group!(benches, reclaim);
criterion_main!(benches);

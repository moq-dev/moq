//! What one change to an origin's own claims costs its lite-07 route node.
//!
//! The node answers announce streams, each with its own interest, and each is sent
//! only the claims that changed among those its interest meets. Swept over the claims
//! the origin holds and the streams it answers: a cost that grows with the claims
//! rather than the streams the change reaches shows up as a slope. Sending a change
//! costs each stream it reaches its own batch, so the streams axis is expected to
//! grow; the claims axis is not.
//!
//! - watched: every stream watches the root, so each is sent the change.
//! - unwatched: every stream watches a prefix of its own, which the change misses.
//!
//! The route layer is crate-private, so this reaches it through the hidden `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench routes`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz;

/// `(claims, streams)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 1_000), (10_000, 1_000)];

fn bench(c: &mut Criterion, name: &str, interest: fn(usize) -> String, sent: usize) {
	let mut group = c.benchmark_group(format!("routes/{name}"));
	for (claims, streams) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{streams}s"));
		// Criterion calls the closure once per sample, so the fixture is built on the first call and kept.
		let mut fixture = None;
		group.bench_function(id, |b| {
			let node = fixture.get_or_insert_with(|| {
				let interests: Vec<String> = (0..streams).map(interest).collect();
				let interests: Vec<&str> = interests.iter().map(String::as_str).collect();
				fuzz::RouteNode::new(claims, &interests)
			});
			b.iter(|| {
				node.own("live/extra", true);
				assert_eq!(node.drain(), sent * streams);
				node.own("live/extra", false);
				assert_eq!(node.drain(), sent * streams);
			});
		});
	}
	group.finish();
}

fn bench_watched(c: &mut Criterion) {
	// Each stream reads the start, then the withdrawal, as a batch.
	bench(c, "watched", |_| String::new(), 1);
}

fn bench_unwatched(c: &mut Criterion) {
	bench(c, "unwatched", |i| format!("other/{i}"), 0);
}

criterion_group!(benches, bench_watched, bench_unwatched);
criterion_main!(benches);

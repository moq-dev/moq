//! What one change costs a lite-07 route node, at the origin and at a relay hearing it.
//!
//! - watched, unwatched: one change to the origin's own claims, swept over the claims
//!   the origin holds and the streams it answers. Sending a change costs each stream
//!   it reaches its own batch, so the streams axis is expected to grow; the claims
//!   axis is not. In watched every stream watches the root, so each is sent the
//!   change; in unwatched each watches a prefix of its own, which the change misses.
//! - received: a relay hears one batch from the origin and forwards it on, swept over
//!   the claims the origin holds and the claims the batch changes. The changes axis is
//!   expected to grow; the claims axis is not.
//! - lagging: a relay hears the origin on two streams, the second some one-claim
//!   batches behind the first, and each hears its next batch. Swept over the claims
//!   the origin holds and how far the second lags: neither axis is expected to grow.
//! - confirmed: a relay holds back every claim from the streams it answers until it
//!   confirms serving one, and one claim restarts and is confirmed. Swept over the
//!   claims held and the streams answered: the streams axis is expected to grow, since
//!   each is sent the claim; the claims axis is not.
//!
//! The route layer is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench routes`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz;

/// `(claims, streams)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 1_000), (10_000, 1_000)];

/// `(claims, changes)` shapes for a relay hearing a batch.
const RECEIVED: [(usize, usize); 4] = [(100, 1), (10_000, 1), (100, 100), (10_000, 100)];

/// `(claims, streams)` shapes for a relay holding back claims.
const HELD: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 100), (10_000, 100)];

/// `(claims, lag)` shapes for a relay hearing a stream that lags another.
const LAGGING: [(usize, usize); 4] = [(100, 1), (10_000, 1), (100, 100), (10_000, 10_000)];

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

fn bench_received(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/received");
	for (claims, changes) in RECEIVED {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{changes}d"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let relay = fixture.get_or_insert_with(|| fuzz::RouteRelay::heard(claims));
			// One serving change per claim changed, and one forwarded batch.
			b.iter(|| assert_eq!(relay.change(changes), changes + 1));
		});
	}
	group.finish();
}

fn bench_lagging(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/lagging");
	for (claims, lag) in LAGGING {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{lag}l"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let relay = fixture.get_or_insert_with(|| fuzz::RouteRelay::lagging(claims, lag));
			// The first stream's change is served through both streams and forwarded;
			// the second's older one changes nothing.
			b.iter(|| assert_eq!(relay.step(), 3));
		});
	}
	group.finish();
}

fn bench_confirmed(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/confirmed");
	for (claims, streams) in HELD {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{streams}s"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let relay = fixture.get_or_insert_with(|| fuzz::RouteRelay::held(claims, streams));
			// The restart's serving change, then one batch per stream answered.
			b.iter(|| assert_eq!(relay.restart(), 1 + streams));
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_watched,
	bench_unwatched,
	bench_received,
	bench_lagging,
	bench_confirmed
);
criterion_main!(benches);

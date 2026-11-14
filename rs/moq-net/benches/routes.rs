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
//!   confirms serving one, all of them at one instance and epoch. One claim restarts at
//!   another epoch, then back at the shared one, each confirmed. Swept over the claims
//!   held and the streams answered: the streams axis is expected to grow, since each is
//!   sent the claim; the claims axis is not.
//! - teardown: a node following one origin on a stream per prefix, as a viewer session
//!   holding many announce prefixes does, closes them all at once. Swept over the
//!   prefixes followed and the claims each holds: both axes are expected to grow, no
//!   faster than the claims held.
//! - refollow: the same node closes one prefix's stream and opens it again. The claims
//!   axis is expected to grow, since the stream hears its prefix's claims again; the
//!   prefixes axis no faster than the prefixes held.
//! - path/watched, path/unwatched, path/received: the same fan-out grids on the path rule.
//! - path/follow, path/teardown, path/refollow: the prefix-by-claim grid on the path
//!   router, including initial following and timer work with a virtual clock.
//! - path/drain: a relay hears a node it reaches over many sessions send GOAWAY, with
//!   a route per session sent to each stream it answers. Swept over the leaving
//!   node's sessions and the streams: both axes are expected to grow, since each
//!   stream is re-sent each route ranked draining, and no faster than their product.
//! - path/drained: after the drain, an RTT change on a session with no routes. Swept
//!   over the same grid: the streams axis is expected to grow, since each stream's
//!   DRAIN notice is kept; the sessions axis is not.
//!
//! The route layer is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench routes`.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz;

/// `(claims, streams)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 1_000), (10_000, 1_000)];

/// `(claims, changes)` shapes for a relay hearing a batch.
const RECEIVED: [(usize, usize); 4] = [(100, 1), (10_000, 1), (100, 100), (10_000, 100)];

/// `(claims, streams)` shapes for a relay holding back claims.
const HELD: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 100), (10_000, 100)];

/// `(prefixes, claims)` shapes for a node following one origin per prefix.
const FOLLOWED: [(usize, usize); 4] = [(16, 1), (1_024, 1), (16, 16), (1_024, 16)];

/// `(sessions, streams)` shapes for a relay hearing a node it reaches over many
/// sessions drain.
const DRAINING: [(usize, usize); 4] = [(4, 4), (128, 4), (4, 128), (128, 128)];

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
			// Each change's serving change, then one batch per stream answered.
			b.iter(|| assert_eq!(relay.restart(), 2 * (1 + streams)));
		});
	}
	group.finish();
}

fn bench_teardown(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/teardown");
	for (prefixes, claims) in FOLLOWED {
		let id = BenchmarkId::from_parameter(format!("{prefixes}p_{claims}c"));
		group.bench_function(id, |b| {
			b.iter_batched(
				|| fuzz::RouteFollower::new(prefixes, claims),
				fuzz::RouteFollower::teardown,
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

fn bench_refollow(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/refollow");
	for (prefixes, claims) in FOLLOWED {
		let id = BenchmarkId::from_parameter(format!("{prefixes}p_{claims}c"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let follower = fixture.get_or_insert_with(|| fuzz::RouteFollower::new(prefixes, claims));
			// The reopened stream serves its prefix's claims into the origin again.
			b.iter(|| assert_eq!(follower.refollow(), claims));
		});
	}
	group.finish();
}

fn bench_path_teardown(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/teardown");
	for (prefixes, claims) in FOLLOWED {
		let id = BenchmarkId::from_parameter(format!("{prefixes}p_{claims}c"));
		group.bench_function(id, |b| {
			b.iter_batched(
				|| fuzz::PathRouteFollower::new(prefixes, claims),
				fuzz::PathRouteFollower::teardown,
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

fn bench_path_follow(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/follow");
	for (prefixes, claims) in FOLLOWED {
		let id = BenchmarkId::from_parameter(format!("{prefixes}p_{claims}c"));
		group.bench_function(id, |b| {
			b.iter_batched(
				|| (),
				|()| fuzz::PathRouteFollower::new(prefixes, claims),
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

fn bench_path(c: &mut Criterion, name: &str, interest: fn(usize) -> String, sent: usize) {
	let mut group = c.benchmark_group(format!("routes/path/{name}"));
	for (claims, streams) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{streams}s"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let node = fixture.get_or_insert_with(|| {
				let interests: Vec<String> = (0..streams).map(interest).collect();
				let interests: Vec<&str> = interests.iter().map(String::as_str).collect();
				fuzz::PathRouteNode::new(claims, &interests)
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

fn bench_path_watched(c: &mut Criterion) {
	bench_path(c, "watched", |_| String::new(), 1);
}

fn bench_path_unwatched(c: &mut Criterion) {
	bench_path(c, "unwatched", |i| format!("other/{i}"), 0);
}

fn bench_path_received(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/received");
	for (claims, changes) in RECEIVED {
		let id = BenchmarkId::from_parameter(format!("{claims}c_{changes}d"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let relay = fixture.get_or_insert_with(|| fuzz::PathRouteRelay::new(claims));
			b.iter(|| assert_eq!(relay.change(changes), (1, changes)));
		});
	}
	group.finish();
}

fn bench_path_refollow(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/refollow");
	for (prefixes, claims) in FOLLOWED {
		let id = BenchmarkId::from_parameter(format!("{prefixes}p_{claims}c"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let follower = fixture.get_or_insert_with(|| fuzz::PathRouteFollower::new(prefixes, claims));
			b.iter(|| follower.refollow());
		});
	}
	group.finish();
}

fn bench_path_drain(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/drain");
	for (sessions, streams) in DRAINING {
		let id = BenchmarkId::from_parameter(format!("{sessions}n_{streams}s"));
		group.bench_function(id, |b| {
			b.iter_batched(
				|| fuzz::PathRouteDrain::new(sessions, streams),
				|mut node| node.drain(),
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

fn bench_path_drained(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/path/drained");
	for (sessions, streams) in DRAINING {
		let id = BenchmarkId::from_parameter(format!("{sessions}n_{streams}s"));
		let mut fixture = None;
		group.bench_function(id, |b| {
			let node = fixture.get_or_insert_with(|| {
				let mut node = fuzz::PathRouteDrain::new(sessions, streams);
				node.drain();
				node
			});
			b.iter(|| node.idle());
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
	bench_confirmed,
	bench_teardown,
	bench_refollow,
	bench_path_follow,
	bench_path_watched,
	bench_path_unwatched,
	bench_path_received,
	bench_path_teardown,
	bench_path_refollow,
	bench_path_drain,
	bench_path_drained
);
criterion_main!(benches);

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
//! - refollow: the same node stops asking for one prefix and asks for it again. The claims
//!   axis is expected to grow, since the stream hears its prefix's claims again; the
//!   prefixes axis no faster than the prefixes held.
//!
//! - drain, repath, follow, reopen, swap, remark: a relay hearing every forwarding node
//!   behind it from its upstream neighbours, telling one downstream neighbour every route
//!   and one watcher every claim, and serving what it hears into a real origin as a
//!   session does. Swept over the forwarding nodes whose routes the event changes and the
//!   broadcasts behind each. Each prints what the event cost once per shape: routes and
//!   claims sent downstream, claims told the watcher, and writes into the origin.
//! - drain: the cheapest upstream sends GOAWAY, so every route moves to the second.
//! - repath: the only upstream moves every route onto another path at the same cost.
//!   Nothing anyone is told or served changes, so only the routes axis may grow.
//! - follow: the first upstream connects to a relay holding nothing and offers every
//!   route with its claims.
//! - reopen/spare: the cheapest upstream closes its session and opens another, twice in
//!   a row, as a relay restarting every link does, while a second upstream offers every
//!   route dearer. Each route's cost changes four times, and each change rewrites every
//!   claim behind it into the origin and tells the watcher each again.
//! - reopen/sole: the same with no second upstream, so each route ends and returns:
//!   the claims stay shown, unavailable, and come back sent whole.
//! - swap: the watcher swaps the whole tree for the first forwarding node's prefix and
//!   back. Each edit tells again the claims both prefixes cover.
//! - remark: the relay's own claim moves between its two identities, then back, as an
//!   endpoint's does when it reconnects over a marked link. Swept over the claims the relay
//!   holds and the downstream neighbours, which each hear a revision under both.
//!
//! The route layer is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench routes`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings leave the unmeasured half of an event out of the sample"
)]

use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz::{self, RouteMesh, RouteShape, RouteTally};

/// `(claims, streams)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 1_000), (10_000, 1_000)];

/// `(claims, changes)` shapes for a relay hearing a batch.
const RECEIVED: [(usize, usize); 4] = [(100, 1), (10_000, 1), (100, 100), (10_000, 100)];

/// `(claims, streams)` shapes for a relay holding back claims.
const HELD: [(usize, usize); 4] = [(10, 10), (10_000, 10), (10, 100), (10_000, 100)];

/// `(prefixes, claims)` shapes for a node following one origin per prefix.
const FOLLOWED: [(usize, usize); 4] = [(16, 1), (1_024, 1), (16, 16), (1_024, 16)];

/// `(claims, lag)` shapes for a relay hearing a stream that lags another.
const LAGGING: [(usize, usize); 4] = [(100, 1), (10_000, 1), (100, 100), (10_000, 10_000)];

/// `(relays, broadcasts)` shapes for a relay between routing neighbours: the forwarding
/// nodes whose routes one event changes, and the broadcasts behind each.
const ROUTED: [(usize, usize); 6] = [(1, 16), (1, 4_096), (16, 16), (16, 4_096), (64, 16), (64, 1_024)];

/// `(broadcasts, downstream)` shapes for the relay's own claim moving identity.
const REMARKED: [(usize, usize); 4] = [(16, 1), (4_096, 1), (16, 16), (4_096, 16)];

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
			// Each claim changed is served into the origin and forwarded on its own.
			b.iter(|| assert_eq!(relay.change(changes), 2 * changes));
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
			// the second's older one queues nothing, though its copy now holds the claim,
			// so the relay works out that claim's ways again.
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
			// Both edits go to the peer, and the prefix's claims leave the origin and return.
			b.iter(|| assert_eq!(follower.refollow(), 2 + 2 * claims));
		});
	}
	group.finish();
}

/// A relay shaped `(relays, broadcasts)` with `upstream` upstream neighbours.
fn mesh(relays: usize, broadcasts: usize, upstream: usize) -> RouteShape {
	RouteShape {
		relays,
		broadcasts,
		upstream,
		downstream: 1,
		watchers: 1,
	}
}

fn label(relays: usize, broadcasts: usize) -> String {
	format!("{relays}r_{broadcasts}b")
}

/// Print what one event cost, once per shape.
fn report(name: &str, id: &str, tally: RouteTally) {
	let RouteTally {
		routes,
		claims,
		told,
		writes,
	} = tally;
	eprintln!("routes/{name}/{id}: routes={routes} claims={claims} told={told} writes={writes}");
}

/// Time `event` on a mesh built once per shape, and `restore` after it unmeasured.
fn bench_event(
	c: &mut Criterion,
	name: &str,
	upstream: usize,
	event: fn(&mut RouteMesh) -> RouteTally,
	restore: fn(&mut RouteMesh),
) {
	let mut group = c.benchmark_group(format!("routes/{name}"));
	group.sample_size(10);
	for (relays, broadcasts) in ROUTED {
		let id = label(relays, broadcasts);
		let mut fixture = None;
		group.bench_function(BenchmarkId::from_parameter(&id), |b| {
			let mesh = fixture.get_or_insert_with(|| {
				let mut mesh = RouteMesh::new(mesh(relays, broadcasts, upstream));
				// Once to settle, then once to report.
				event(&mut mesh);
				restore(&mut mesh);
				report(name, &id, event(&mut mesh));
				restore(&mut mesh);
				mesh
			});
			b.iter_custom(|iters| {
				let mut spent = Duration::ZERO;
				for _ in 0..iters {
					let start = Instant::now();
					std::hint::black_box(event(mesh));
					spent += start.elapsed();
					restore(mesh);
				}
				spent
			});
		});
	}
	group.finish();
}

fn bench_drain(c: &mut Criterion) {
	bench_event(c, "drain", 2, |mesh| mesh.goaway(0), |mesh| _ = mesh.reopen(0));
}

fn bench_repath(c: &mut Criterion) {
	bench_event(c, "repath", 1, |mesh| mesh.repath(0), |_| {});
}

/// Close and open the first upstream twice.
fn twice(mesh: &mut RouteMesh) -> RouteTally {
	let mut tally = mesh.reopen(0);
	tally += mesh.reopen(0);
	tally
}

fn bench_reopen_spare(c: &mut Criterion) {
	bench_event(c, "reopen/spare", 2, twice, |_| {});
}

fn bench_reopen_sole(c: &mut Criterion) {
	bench_event(c, "reopen/sole", 1, twice, |_| {});
}

fn bench_swap(c: &mut Criterion) {
	bench_event(
		c,
		"swap",
		1,
		|mesh| {
			let mut tally = mesh.swap(true);
			tally += mesh.swap(false);
			tally
		},
		|_| {},
	);
}

fn bench_follow(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/follow");
	group.sample_size(10);
	for (relays, broadcasts) in ROUTED {
		let id = label(relays, broadcasts);
		let mut reported = false;
		group.bench_function(BenchmarkId::from_parameter(&id), |b| {
			if !std::mem::replace(&mut reported, true) {
				report("follow", &id, RouteMesh::new(mesh(relays, broadcasts, 0)).open(0));
			}
			b.iter_batched_ref(
				|| RouteMesh::new(mesh(relays, broadcasts, 0)),
				|mesh| mesh.open(0),
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

fn bench_remark(c: &mut Criterion) {
	let mut group = c.benchmark_group("routes/remark");
	for (broadcasts, downstream) in REMARKED {
		let id = format!("{broadcasts}b_{downstream}d");
		let mut fixture = None;
		group.bench_function(BenchmarkId::from_parameter(&id), |b| {
			let mesh = fixture.get_or_insert_with(|| {
				let shape = RouteShape {
					downstream,
					..mesh(0, 0, 0)
				};
				let mut mesh = RouteMesh::new(shape);
				mesh.own(broadcasts);
				mesh.remark();
				mesh.remark();
				let mut tally = mesh.remark();
				tally += mesh.remark();
				report("remark", &id, tally);
				mesh
			});
			b.iter(|| {
				mesh.remark();
				mesh.remark()
			});
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
	bench_drain,
	bench_repath,
	bench_follow,
	bench_reopen_spare,
	bench_reopen_sole,
	bench_swap,
	bench_remark,
);
criterion_main!(benches);

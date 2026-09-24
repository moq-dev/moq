//! Fan-out benchmarks for the origin: its route table, the announce cursors
//! watching it, request resolution, and the handoff between local sources.
//!
//! Every shape is swept over both axes, publishers (routes) and subscribers
//! (cursors), so a cost that grows with the size of the table rather than with
//! the tree around the touched path shows up as a slope. The table is a trie
//! keyed by path segment: an announcement visits only the cursors on the walk
//! down to its prefix and beneath it, and each recomputes its best route from
//! the entries at that prefix alone.
//!
//! Run with `cargo bench -p moq-net --bench origin`.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Hop, Hops, Pattern, Patterns, Timestamp, announce, broadcast, kio, origin};

/// `(publishers, subscribers)` shapes for the fan-out benchmarks.
const SHAPES: [(usize, usize); 3] = [(100, 10), (1_000, 100), (1_000, 1_000)];

/// `(duplicates, subscribers)` shapes for one *contended* prefix: how many
/// routes cover the same path, against how many cursors watch it. The first row
/// is a live fleet's mesh width; the rest sweep past it so the slope is visible.
const CONTENDED: [(usize, usize); 4] = [(30, 30), (60, 60), (240, 60), (240, 240)];

/// An origin with `publishers` broadcasts under `room/` and `subscribers`
/// cursors watching everything. The handles are held: dropping a broadcast
/// retracts its route, and dropping a cursor unregisters it.
struct Fanout {
	producer: origin::Producer,
	consumer: origin::Consumer,
	driver: origin::Driver,
	_publishers: Vec<broadcast::Producer>,
	subscribers: Vec<announce::Consumer>,
}

fn fanout(publishers: usize, subscribers: usize) -> Fanout {
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();
	let publishers = (0..publishers)
		.map(|i| producer.publish(format!("room/{i}"), origin::Route::default()).unwrap())
		.collect();
	let mut subscribers: Vec<announce::Consumer> = (0..subscribers).map(|_| consumer.announced()).collect();
	// Drain the replay so each iteration measures only what it adds.
	for cursor in &mut subscribers {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Fanout {
		producer,
		consumer,
		driver,
		_publishers: publishers,
		subscribers,
	}
}

/// One publish delivered to every subscriber, then its retraction: the
/// announcement fans out to `subscribers` cursors regardless of `publishers`.
fn bench_announce(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce");
	for (publishers, subscribers) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{publishers}p_{subscribers}s"));
		group.bench_function(id, |b| {
			let mut fleet = fanout(publishers, subscribers);
			b.iter(|| {
				let handle = fleet
					.producer
					.publish("room/incoming", origin::Route::default())
					.unwrap();
				for cursor in &mut fleet.subscribers {
					cursor.next().now_or_never().flatten().expect("announce delivered");
				}
				drop(handle);
				for cursor in &mut fleet.subscribers {
					cursor.next().now_or_never().flatten().expect("retract delivered");
				}
			});
		});
	}
	group.finish();
}

/// A relay's own stats fan-out: `.stats/<project>/node/<node>` for every project
/// on every node, watched by one cursor per peer, each scoped to one project.
/// Cursors differ in scope, so none can be collapsed, and an announcement under
/// one project must not touch the peers watching another.
fn bench_announce_fleet(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce_fleet");
	// (projects, nodes, peers). The middle row is a live fleet's shape.
	for (projects, nodes, peers) in [(1, 8, 4), (4, 30, 30), (8, 30, 60)] {
		let id = BenchmarkId::from_parameter(format!("{projects}p_{nodes}n_{peers}c"));
		group.bench_function(id, |b| {
			let (producer, _driver) = origin::Producer::new(origin::Config::default());
			let consumer = producer.consume();
			let _routes: Vec<_> = (0..projects)
				.flat_map(|project| (0..nodes).map(move |node| (project, node)))
				.map(|(project, node)| {
					producer
						.publish(format!(".stats/p{project}/node/edge{node}"), origin::Route::default())
						.unwrap()
				})
				.collect();
			let _cursors: Vec<_> = (0..peers)
				.map(|peer| {
					let patterns: Patterns = [Pattern::subtree(&format!(".stats/p{}", peer % projects)).unwrap()]
						.into_iter()
						.collect();
					consumer.scope("", &patterns).unwrap().announced()
				})
				.collect();
			b.iter(|| {
				let handle = producer
					.publish(".stats/p0/node/incoming", origin::Route::default())
					.unwrap();
				drop(handle);
			});
		});
	}
	group.finish();
}

/// One route flapping at a prefix that `duplicates` others already cover.
///
/// `announce_fleet` gives every path a single announcer, which is the shape a
/// publisher produces. A mesh produces the other one: the same path arrives
/// once per peer it can travel through, so a prefix carries one entry per peer.
/// The trie narrows a change to the touched prefix, but picking the winner
/// there still visits every entry at it, once per watching cursor, so both are
/// swept.
///
/// The arriving route is priced below every incumbent so it takes the prefix
/// outright: each cursor is told twice per iteration, once for the new winner
/// and once for the incumbent taking the prefix back when it retracts. Equal
/// costs would instead tie-break on a hash of the hop chain, which decides the
/// winner but is not what a reconnecting peer does.
fn bench_announce_duplicate(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce_duplicate");
	for (duplicates, subscribers) in CONTENDED {
		let id = BenchmarkId::from_parameter(format!("{duplicates}d_{subscribers}s"));
		group.bench_function(id, |b| {
			let (producer, _driver) = origin::Producer::new(origin::Config::default());
			let consumer = producer.consume();
			// Every peer announces the one path, each under its own hop chain so
			// the entries are distinct routes rather than one re-priced in place.
			let _routes: Vec<_> = (1..=duplicates)
				.map(|peer| producer.dynamic(PATH, peer_route(peer as u64, INCUMBENT_COST)).unwrap())
				.collect();
			let mut cursors: Vec<announce::Consumer> = (0..subscribers)
				.map(|_| consumer.clone().with_hidden(true).announced())
				.collect();
			for cursor in &mut cursors {
				while cursor.next().now_or_never().flatten().is_some() {}
			}

			b.iter(|| {
				let handle = producer
					.dynamic(PATH, peer_route(duplicates as u64 + 1, INCUMBENT_COST - 1))
					.unwrap();
				for cursor in &mut cursors {
					cursor.next().now_or_never().flatten().expect("announce delivered");
				}
				drop(handle);
				for cursor in &mut cursors {
					cursor.next().now_or_never().flatten().expect("incumbent restored");
				}
			});
		});
	}
	group.finish();
}

/// The contended path: one node's stats feed, which every peer in the mesh
/// carries a route to.
const PATH: &str = ".stats/p0/node/edge0";

/// What every incumbent route costs, leaving room for a cheaper challenger.
const INCUMBENT_COST: u64 = 2;

/// A route as `peer` would have announced it: one hop, at `cost`.
fn peer_route(peer: u64, cost: u64) -> origin::Route {
	let mut hops = Hops::new();
	hops.push(Hop::new(peer).expect("peer id")).expect("hop chain");
	origin::Route::default().with_hops(hops).with_cost(cost)
}

/// An announcement of an unrelated prefix with `fronts` remote fronts parked on
/// their upstream request. Each front watches only the routes covering its own
/// path, so none of them wakes; the driver poll after each change runs whatever
/// did. Sweeps fronts, so a per-front wake shows up as a slope.
fn bench_announce_fronts(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce_fronts");
	for fronts in [100, 1_000, 10_000] {
		group.bench_function(BenchmarkId::from_parameter(format!("{fronts}f")), |b| {
			let (producer, mut driver) = origin::Producer::new(origin::Config::default());
			let consumer = producer.consume();
			// Served but never answered: every request under it parks a front.
			let _served = producer.dynamic("room", origin::Route::default()).unwrap();
			let _requests: Vec<_> = (0..fronts)
				.map(|i| consumer.request_broadcast(format!("room/{i}")))
				.collect();
			let waiter = kio::Waiter::noop();
			// Run each front once so it parks on its upstream request.
			driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			b.iter(|| {
				let handle = producer.publish("other/incoming", origin::Route::default()).unwrap();
				driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
				drop(handle);
				driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			});
		});
	}
	group.finish();
}

/// One serve sweep over the routes attached to a single announce stream.
///
/// `lite::subscriber`'s serve loop registers *one* waiter, the announce stream
/// machine's, on *every* attached route's request queue, then re-sweeps all of
/// them whenever it wakes. So the sweep fans in: a request arriving on any one
/// route, or one more announce landing during convergence, re-polls every other
/// route's queue, each taking its lock and re-registering the waiter. An idle
/// sweep that serves nothing still costs one lock per attached route.
///
/// A mesh makes `routes` large: a peer announcing `.stats/<project>/node/<node>`
/// for every project on every node attaches one route per path to one stream.
/// This measures a single sweep, so the convergence cost of a reconnecting peer
/// (one sweep per announce, over a table growing to `routes`) reads off it as
/// the sum.
fn bench_serve_idle(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/serve_idle");
	for routes in [30, 300, 3_000] {
		group.throughput(Throughput::Elements(routes as u64));
		group.bench_function(BenchmarkId::from_parameter(format!("{routes}r")), |b| {
			let (producer, _driver) = origin::Producer::new(origin::Config::default());
			// One route per announced path, exactly as a session lands a peer's.
			let dynamics: Vec<_> = (0..routes)
				.map(|i| {
					producer
						.dynamic(format!(".stats/p{}/node/edge{i}", i % 8), origin::Route::default())
						.unwrap()
				})
				.collect();
			b.iter(|| {
				// A fresh waiter per sweep. A live registration keeps the waiter's
				// `Weak` in each route's list until it drops, so a waiter reused
				// across sweeps would stack one per route per iteration. Production
				// retires the parked waiter the same way: `Park::hold` drops a
				// still-registered waiter before the next poll registers again.
				let waiter = kio::Waiter::noop();
				// Nothing is queued, so every poll parks again: the idle sweep.
				for dynamic in &dynamics {
					assert!(dynamic.poll_requested_broadcast(&waiter).is_pending());
				}
			});
		});
	}
	group.finish();
}

/// A new subscriber registering against `publishers` routes and draining the
/// replay: the one operation whose cost legitimately scales with what it watches.
fn bench_subscribe(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/subscribe");
	for (publishers, subscribers) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{publishers}p_{subscribers}s"));
		group.bench_function(id, |b| {
			let fleet = fanout(publishers, subscribers);
			b.iter(|| {
				let mut cursor = fleet.consumer.announced();
				let mut replayed = 0;
				while cursor.next().now_or_never().flatten().is_some() {
					replayed += 1;
				}
				assert_eq!(replayed, publishers);
			});
		});
	}
	group.finish();
}

/// Resolving one broadcast among `publishers`: a local hit walks the table to
/// the exact path and joins the front serving it, and a miss under a broadcast
/// published above it walks the table to prove nothing serves it. Neither may
/// depend on `publishers`.
fn bench_request(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/request");
	// A request costs nothing per subscriber, so this sweeps the publisher counts
	// in `SHAPES` rather than its shapes, whose last two share one.
	for publishers in [100, 1_000] {
		let mut fleet = fanout(publishers, 0);
		// A broadcast above the misses covers them without serving them.
		let _covering = fleet.producer.publish("room", origin::Route::default()).unwrap();
		let waiter = kio::Waiter::noop();
		group.bench_function(BenchmarkId::new("local", publishers), |b| {
			b.iter(|| {
				let pending = fleet.consumer.request_broadcast("room/0");
				// The front's driver resolves the first request; later ones join it.
				fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
				pending
					.now_or_never()
					.expect("resolves once driven")
					.expect("local broadcast");
			});
		});
		group.bench_function(BenchmarkId::new("unroutable", publishers), |b| {
			b.iter(|| {
				let result = fleet
					.consumer
					.request_broadcast("room/missing")
					.now_or_never()
					.expect("fails synchronously");
				assert!(matches!(result, Err(moq_net::Error::Unroutable)));
			});
		});
	}
	group.finish();
}

/// Publisher handoff at one path: a subscriber is reading from one local
/// source when a second announces at the same path and takes over (newest
/// wins). Measured from the standby's attach to the subscriber receiving its
/// first group, with `publishers` unrelated broadcasts in the table.
fn bench_handoff(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/handoff");
	group.measurement_time(Duration::from_secs(5));
	for publishers in [1, 1_000] {
		group.bench_function(BenchmarkId::from_parameter(format!("{publishers}p")), |b| {
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			let (producer, driver) = origin::Producer::new(origin::Config::default());
			// The driver runs whenever the runtime is entered below.
			runtime.spawn(moq_net::time::run(driver));
			let consumer = producer.consume();
			let _others: Vec<_> = (0..publishers)
				.map(|i| producer.publish(format!("room/{i}"), origin::Route::default()).unwrap())
				.collect();

			b.iter_custom(|iterations| {
				runtime.block_on(async {
					let mut total = Duration::ZERO;
					for _ in 0..iterations {
						let incumbent = producer.publish("room/live", origin::Route::default()).unwrap();
						let track = incumbent.create_track("video", None).unwrap();
						let mut first = track.append_group().unwrap();
						first.write_frame(Timestamp::ZERO, b"one".as_ref()).unwrap();
						first.finish().unwrap();

						let resolved = consumer.request_broadcast("room/live").await.unwrap();
						let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
						subscription.recv_group().await.unwrap().expect("first group");

						let started = std::time::Instant::now();
						let standby = producer.create_broadcast("room/live").unwrap();
						let track = standby.create_track("video", None).unwrap();
						let mut second = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
						second.write_frame(Timestamp::ZERO, b"two".as_ref()).unwrap();
						second.finish().unwrap();
						// Announcing is what makes the standby a route the front can take.
						standby.announce(origin::Route::default()).unwrap();
						subscription.recv_group().await.unwrap().expect("standby group");
						total += started.elapsed();

						drop(subscription);
						incumbent.finish();
						standby.finish();
						// Wait for the front to close so the next iteration starts a fresh one.
						resolved.closed().await;
					}
					total
				})
			});
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_announce,
	bench_announce_fleet,
	bench_announce_duplicate,
	bench_announce_fronts,
	bench_serve_idle,
	bench_subscribe,
	bench_request,
	bench_handoff
);
criterion_main!(benches);

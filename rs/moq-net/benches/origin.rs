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

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Pattern, Patterns, Timestamp, announce, broadcast, kio, origin};

/// `(publishers, subscribers)` shapes for the fan-out benchmarks.
const SHAPES: [(usize, usize); 3] = [(100, 10), (1_000, 100), (1_000, 1_000)];

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
/// source when a second attaches at the same path and takes over (newest
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
						let incumbent = producer.create_broadcast("room/live").unwrap();
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
	bench_announce_fronts,
	bench_subscribe,
	bench_request,
	bench_handoff
);
criterion_main!(benches);

//! Fan-out benchmarks for the origin: its route table, the announce cursors
//! watching it, request resolution, the handoff between local sources, and the
//! delivery through a front.
//!
//! Every shape is swept over both axes, publishers (routes) and subscribers
//! (cursors), so a cost that grows with the size of the table rather than with
//! the tree around the touched path shows up as a slope. The table is a trie
//! keyed by path segment: an announcement visits only the cursors on the walk
//! down to its prefix and beneath it. Competing routes at that prefix are
//! ranked once per change, then each cursor selects its first visible entry.
//!
//! `origin/narrow*` put a session in front of the origin, since a narrowing is
//! enforced by the session serving it: every subscription holds a gate on the
//! session's grant, and a narrowing wakes each one. The baseline is a session
//! that never narrows.
//!
//! A withdrawal wave is swept over the broadcasts it withdraws and the cursors
//! polling through it, and a route churning beneath an unchanged winner over the
//! table and its cursors.
//!
//! An equal-cost pool is swept the same way, over its members and the paths it
//! already serves, and over those paths and the cursors watching the pool.
//!
//! One-frame groups through a front with a single route are swept over readers and the
//! groups each holds open, which is what a reader records per group it hands out.
//!
//! A route swap on one front is swept over its tracks and the copies each track
//! still holds from earlier routes, and a front retiring over the fronts around it.
//! Readers draining a swapped track are swept over their count, the groups every
//! route delivered, and the routes each reader still holds a copy of.
//!
//! A claim's answer taking over the claim is swept over the paths the claim already
//! answered and the cursors watching it, and a takeover in place over the fronts
//! resolved through what it takes over.
//!
//! Peer claims land through the hidden `fuzz` module, as a session lands them.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench origin`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

#[path = "../tests/integration/support/mod.rs"]
mod support;

use std::task::Poll;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Epoch, Hop, Hops, Pattern, Patterns, Timestamp, announce, auth, broadcast, kio, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// `(publishers, subscribers)` shapes for the fan-out benchmarks.
const SHAPES: [(usize, usize); 3] = [(100, 10), (1_000, 100), (1_000, 1_000)];

/// `(duplicates, subscribers)` shapes for one *contended* prefix: how many
/// routes cover the same path, against how many cursors watch it. Includes
/// sparse fanout, a live fleet's mesh width, and larger contended tables.
const CONTENDED: [(usize, usize); 6] = [(1, 1), (30, 1), (30, 30), (60, 60), (240, 60), (240, 240)];

/// An origin with `publishers` broadcasts under `room/` and `subscribers`
/// cursors watching everything. The handles are held: dropping a broadcast
/// retracts its route, and dropping a cursor unregisters it.
struct Fanout {
	producer: origin::Producer,
	consumer: origin::Consumer,
	driver: origin::Driver,
	_publishers: Vec<origin::Publication>,
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

/// Matched and overlap-only route churn, swept over table size and watching cursors,
/// for listing cursors and covering ones.
fn bench_announce_covering(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce_covering");
	for routes in [10, 1_000] {
		for readers in [1, 100] {
			for covering in [false, true] {
				group.bench_function(format!("{routes}routes_{readers}readers_covering{covering}"), |b| {
					let (producer, _driver) = origin::Producer::new(origin::Config::default());
					let _routes: Vec<_> = (0..routes)
						.map(|i| producer.requests(format!("room/{i}/mic.hang")).unwrap())
						.collect();
					let consumer = producer
						.consume()
						.scope("", &Patterns::from("**/*.chat".parse::<Pattern>().unwrap()))
						.unwrap();
					let consumer = if covering { consumer.covering() } else { consumer };
					let mut cursors: Vec<_> = (0..readers).map(|_| consumer.announced()).collect();
					for cursor in &mut cursors {
						while cursor.next().now_or_never().flatten().is_some() {}
					}
					b.iter(|| {
						for (name, delivered) in [("room/live.chat", true), ("room/mic.hang", covering)] {
							let route = producer.requests(name).unwrap();
							for cursor in &mut cursors {
								assert_eq!(cursor.next().now_or_never().flatten().is_some(), delivered);
							}
							drop(route);
							for cursor in &mut cursors {
								assert_eq!(cursor.next().now_or_never().flatten().is_some(), delivered);
							}
						}
					});
				});
			}
		}
	}
	group.finish();
}

/// Cancel unseen additions while every cursor stalls on its initial replay.
fn bench_pending_churn(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/pending_churn");
	for routes in [10, 1_000] {
		for readers in [1, 100] {
			group.bench_function(format!("{routes}routes_{readers}readers"), |b| {
				let (producer, _driver) = origin::Producer::new(origin::Config::default());
				let _routes: Vec<_> = (0..routes)
					.map(|i| producer.publish(format!("z/{i}"), origin::Route::default()).unwrap())
					.collect();
				let mut cursors: Vec<_> = (0..readers).map(|_| producer.consume().announced()).collect();
				let mut sequence = 0u64;
				b.iter(|| {
					sequence += 1;
					drop(
						producer
							.publish(format!("a/{sequence}"), origin::Route::default())
							.unwrap(),
					);
				});
				for cursor in &mut cursors {
					assert_eq!(std::iter::from_fn(|| cursor.try_next()).count(), routes);
				}
			});
		}
	}
	group.finish();
}

/// A peer carrying `publishers` broadcasts withdraws them all before the
/// driver's clock moves again, then `subscribers` cursors drain the retractions.
/// Each retraction quarantines its prefix until a fresh clock reading, so a cost
/// per earlier retraction shows up as a slope over publishers.
fn bench_withdraw(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/withdraw");
	for (publishers, subscribers) in [(100, 1), (1_000, 1), (10_000, 1), (1_000, 10)] {
		let id = BenchmarkId::from_parameter(format!("{publishers}p_{subscribers}s"));
		group.bench_function(id, |b| {
			b.iter_batched(
				|| {
					let (producer, mut driver) = origin::Producer::new(origin::Config::default());
					// Retractions read the driver's last clock reading, which stays put below.
					driver
						.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
						.unwrap();
					let routes: Vec<_> = (0..publishers)
						.map(|i| {
							moq_net::fuzz::requests_route(
								&producer,
								moq_net::Path::new(&format!("room/{i}")).unwrap(),
								peer_route(9, 1),
							)
							.unwrap()
						})
						.collect();
					let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().announced()).collect();
					for cursor in &mut cursors {
						while cursor.next().now_or_never().flatten().is_some() {}
					}
					(producer, driver, routes, cursors)
				},
				|(producer, driver, routes, mut cursors)| {
					drop(routes);
					for cursor in &mut cursors {
						for _ in 0..publishers {
							cursor.next().now_or_never().flatten().expect("retract delivered");
						}
					}
					(producer, driver, cursors)
				},
				BatchSize::LargeInput,
			);
		});
	}
	group.finish();
}

/// Two peers carry the same `broadcasts` of one publisher, and both withdraw them
/// a broadcast at a time, every cursor polled after each withdrawal as a session's
/// task would be. The first peer's withdrawals queue held updates; the second's, in
/// the opposite order, make each ready behind the held rest. A poll that walks what
/// it holds back shows up as a slope over broadcasts, multiplied by the cursors.
fn bench_withdraw_wave(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/withdraw_wave");
	group.sample_size(10);
	let epoch: Epoch = "01900000-0000-7000-8000-000000000001".parse().unwrap();
	// One epoch, so the winner moving between the peers is an update, which is held.
	let route = |chain: &[u64]| {
		let mut hops = Hops::new();
		for &hop in chain {
			hops.push(Hop::new(hop).unwrap()).unwrap();
		}
		origin::Route::default().with_epoch(epoch.clone()).with_hops(hops)
	};
	for broadcasts in [1_000, 10_000] {
		for subscribers in [1, 20, 100] {
			let id = BenchmarkId::from_parameter(format!("{broadcasts}b_{subscribers}s"));
			group.throughput(Throughput::Elements((2 * broadcasts * subscribers) as u64));
			group.bench_function(id, |b| {
				b.iter_batched(
					|| {
						let (producer, mut driver) = origin::Producer::new(origin::Config::default());
						// The holds read the driver's last clock reading, which stays put below.
						driver
							.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
							.unwrap();
						let near: Vec<_> = (0..broadcasts)
							.map(|i| {
								moq_net::fuzz::requests_route(
									&producer,
									moq_net::Path::new(&format!("room/{i}")).unwrap(),
									route(&[9, 2]),
								)
								.unwrap()
							})
							.collect();
						let far: Vec<_> = (0..broadcasts)
							.map(|i| {
								moq_net::fuzz::requests_route(
									&producer,
									moq_net::Path::new(&format!("room/{i}")).unwrap(),
									route(&[9, 3, 4]),
								)
								.unwrap()
							})
							.collect();
						let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().announced()).collect();
						for cursor in &mut cursors {
							while cursor.try_next().is_some() {}
						}
						(producer, driver, near, far, cursors)
					},
					|(producer, driver, near, far, mut cursors)| {
						for route in near {
							drop(route);
							for cursor in &mut cursors {
								assert!(cursor.try_next().is_none(), "the update is held");
							}
						}
						for route in far.into_iter().rev() {
							drop(route);
							for cursor in &mut cursors {
								cursor.try_next().expect("retract delivered");
							}
						}
						(producer, driver, cursors)
					},
					BatchSize::LargeInput,
				);
			});
		}
	}
	group.finish();
}

/// A route that never wins arriving at one broadcast's prefix and leaving again,
/// with `broadcasts` in the table and `subscribers` cursors watching. Each change
/// revisits every cursor presenting the prefix and finds its winner unchanged,
/// so a cost per cursor shows up as a slope over subscribers, and one per route
/// in the table as a slope over broadcasts.
fn bench_route_churn(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/route_churn");
	for broadcasts in [1_000, 10_000] {
		for subscribers in [1, 20, 100] {
			let id = BenchmarkId::from_parameter(format!("{broadcasts}b_{subscribers}s"));
			group.bench_function(id, |b| {
				let (producer, _driver) = origin::Producer::new(origin::Config::default());
				let _routes: Vec<_> = (0..broadcasts)
					.map(|i| {
						moq_net::fuzz::requests_route(
							&producer,
							moq_net::Path::new(&format!("room/{i}")).unwrap(),
							peer_route(1, 1),
						)
						.unwrap()
					})
					.collect();
				let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().announced()).collect();
				for cursor in &mut cursors {
					while cursor.try_next().is_some() {}
				}
				b.iter(|| {
					drop(
						moq_net::fuzz::requests_route(
							&producer,
							moq_net::Path::from_static("room/0"),
							peer_route(2, 5),
						)
						.unwrap(),
					)
				});
				for cursor in &mut cursors {
					assert!(cursor.try_next().is_none(), "the winner is unchanged");
				}
			});
		}
	}
	group.finish();
}

/// `bench_announce` read through mounts: `publishers` routes under the fleet-wide
/// `.svc/p0`, watched by `subscribers` project sessions that each mount it at
/// their own `<project>/.svc`. Every mount aliases the one target, the worst
/// case, so each announcement reaches every mounted cursor. Compare against
/// `origin/announce` for what a mount adds per cursor.
fn bench_announce_mounted(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/announce_mounted");
	for (publishers, subscribers) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{publishers}p_{subscribers}s"));
		group.bench_function(id, |b| {
			let (producer, _driver) = origin::Producer::new(origin::Config::default());
			let _publishers: Vec<_> = (0..publishers)
				.map(|i| {
					producer
						.publish(format!(".svc/p0/{i}"), origin::Route::default())
						.unwrap()
				})
				.collect();
			let mut cursors: Vec<announce::Consumer> = (0..subscribers)
				.map(|project| {
					producer
						.mount(format!("p{project}/.svc"), ".svc/p0")
						.unwrap()
						.scope(format!("p{project}"), &Patterns::from(Pattern::all()))
						.unwrap()
						.consume()
						.with_hidden(true)
						.announced()
				})
				.collect();
			for cursor in &mut cursors {
				while cursor.next().now_or_never().flatten().is_some() {}
			}

			b.iter(|| {
				let handle = producer.publish(".svc/p0/incoming", origin::Route::default()).unwrap();
				for cursor in &mut cursors {
					cursor.next().now_or_never().flatten().expect("announce delivered");
				}
				drop(handle);
				for cursor in &mut cursors {
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
/// there shares the ranking across its watching cursors, so both are swept.
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
			// No hold, so an iteration times the ranking rather than the origin batching it.
			let mut config = origin::Config::default();
			config.update_hold = Duration::ZERO;
			let (producer, _driver) = origin::Producer::new(config);
			let consumer = producer.consume();
			// Every peer announces the one path, each under its own hop chain so
			// the entries are distinct routes rather than one re-priced in place.
			let _routes: Vec<_> = (1..=duplicates)
				.map(|peer| {
					moq_net::fuzz::requests_route(
						&producer,
						moq_net::Path::new(PATH).unwrap(),
						peer_route(peer as u64, INCUMBENT_COST),
					)
					.unwrap()
				})
				.collect();
			let mut cursors: Vec<announce::Consumer> = (0..subscribers)
				.map(|_| consumer.clone().with_hidden(true).announced())
				.collect();
			for cursor in &mut cursors {
				while cursor.next().now_or_never().flatten().is_some() {}
			}

			b.iter(|| {
				let handle = moq_net::fuzz::requests_route(
					&producer,
					moq_net::Path::new(PATH).unwrap(),
					peer_route(duplicates as u64 + 1, INCUMBENT_COST - 1),
				)
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

/// Re-price a live mesh route with cursors watching different scopes.
/// The challenger alternates between winning and losing, so selection must
/// honor each cursor's scope rather than reuse one global winner.
fn bench_reprice_duplicate(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/reprice_duplicate");
	for incumbent_cost in [INCUMBENT_COST, origin::Cost::MAX.value() - 1] {
		for (duplicates, subscribers) in CONTENDED {
			let id = BenchmarkId::from_parameter(format!("{duplicates}d_{subscribers}s_cost{incumbent_cost}"));
			group.bench_function(id, |b| {
				// No hold, so an iteration times the ranking rather than the origin batching it.
				let mut config = origin::Config::default();
				config.update_hold = Duration::ZERO;
				let (producer, _driver) = origin::Producer::new(config);
				let _routes: Vec<_> = (1..=duplicates)
					.map(|peer| {
						let scoped = producer
							.scope(
								"",
								&Patterns::from(Pattern::subtree(&format!("{PATH}/p{}", (peer - 1) % 2)).unwrap()),
							)
							.unwrap();
						moq_net::fuzz::requests_route(
							&scoped,
							moq_net::Path::new(PATH).unwrap(),
							peer_route(peer as u64, incumbent_cost),
						)
						.unwrap()
					})
					.collect();
				let challenger = duplicates as u64 + 1;
				let route = moq_net::fuzz::requests_route(
					&producer,
					moq_net::Path::new(PATH).unwrap(),
					peer_route(challenger, incumbent_cost + 1),
				)
				.unwrap();
				let mut cursors: Vec<announce::Consumer> = (0..subscribers)
					.map(|peer| {
						producer
							.consume()
							.scope(
								"",
								&Patterns::from(Pattern::subtree(&format!("{PATH}/p{}", peer % 2)).unwrap()),
							)
							.unwrap()
							.with_hidden(true)
							.announced()
					})
					.collect();
				for cursor in &mut cursors {
					while cursor.next().now_or_never().flatten().is_some() {}
				}
				b.iter(|| {
					for cost in [incumbent_cost - 1, incumbent_cost + 1] {
						moq_net::fuzz::update_route(&route, peer_route(challenger, cost)).unwrap();
						for cursor in &mut cursors {
							// The winner switches between epochless routes, which is another instance.
							let event = cursor.next().now_or_never().flatten().expect("winner changed");
							let moq_net::announce::Event::Restart(update) = event else {
								panic!("expected a restart: got {event:?}");
							};
							assert_eq!(update.route.cost, moq_net::origin::Cost::new(cost.min(incumbent_cost)));
						}
					}
				});
			});
		}
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
			let _served = producer.requests("room").unwrap();
			let _requests: Vec<_> = (0..fronts)
				.map(|i| consumer.request(format!("room/{i}"), None))
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

/// Answer the claim's request for `path`, dropping any other it queued.
fn answer(claim: &origin::Requests, path: &str) -> origin::Publication {
	loop {
		let request = claim
			.requested_broadcast()
			.now_or_never()
			.expect("the request reached the claim")
			.unwrap();
		if request.path().as_str() == path {
			return request.prepare(Epoch::mint()).unwrap().publish().unwrap();
		}
	}
}

/// A claim's answer delivered to every cursor as a takeover of the claim, then retracted.
/// Swept over the paths the claim already answered and the cursors watching it, so a
/// continuity check that walks the table rather than the claim's prefix shows up as a
/// slope.
fn bench_takeover(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/takeover");
	for (publishers, subscribers) in SHAPES {
		group.bench_function(
			BenchmarkId::from_parameter(format!("{publishers}p_{subscribers}s")),
			|b| {
				let (producer, mut driver) = origin::Producer::new(origin::Config::default());
				let consumer = producer.consume();
				let claim = producer.requests("room").unwrap();
				let waiter = kio::Waiter::noop();
				let mut ask = |path: &str| {
					let request = consumer.request(path, None);
					driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
					(request, answer(&claim, path))
				};
				let _answered: Vec<_> = (0..publishers).map(|i| ask(&format!("room/{i}"))).collect();
				let mut cursors: Vec<_> = (0..subscribers).map(|_| consumer.announced()).collect();
				for cursor in &mut cursors {
					while cursor.next().now_or_never().flatten().is_some() {}
				}
				let mut n = 0;
				b.iter(|| {
					n += 1;
					let (request, output) = ask(&format!("room/incoming{n}"));
					for cursor in &mut cursors {
						let event = cursor.next().now_or_never().flatten();
						assert!(matches!(event, Some(announce::Event::Takeover { .. })), "{event:?}");
					}
					drop(request);
					drop(output);
					for cursor in &mut cursors {
						cursor.next().now_or_never().flatten().expect("retract delivered");
					}
				});
			},
		);
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
				.map(|i| producer.requests(format!(".stats/p{}/node/edge{i}", i % 8)).unwrap())
				.collect();
			b.iter(|| {
				// A fresh waiter per sweep, so every poll pays a real registration: a
				// reused one would find itself still parked on each route and skip it.
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
				while let Some(announce::Event::Start(_)) = cursor.next().now_or_never().flatten() {
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
	for publishers in [100, 1_000] {
		for requesters in [1, 8, 32] {
			let mut fleet = fanout(publishers, 0);
			let _covering = fleet.producer.publish("room", origin::Route::default()).unwrap();
			let waiter = kio::Waiter::noop();
			let shape = format!("{publishers}p_{requesters}r");
			group.throughput(Throughput::Elements(requesters));
			group.bench_function(BenchmarkId::new("local", &shape), |b| {
				b.iter(|| {
					let pending: Vec<_> = (0..requesters)
						.map(|_| fleet.consumer.request("room/0", None))
						.collect();
					fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
					for pending in pending {
						pending
							.now_or_never()
							.expect("resolves once driven")
							.expect("local broadcast");
					}
				});
			});
			group.bench_function(BenchmarkId::new("unroutable", &shape), |b| {
				b.iter(|| {
					for _ in 0..requesters {
						let result = fleet
							.consumer
							.request("room/missing", None)
							.now_or_never()
							.expect("fails synchronously");
						assert!(matches!(result, Err(moq_net::Error::Unroutable)));
					}
				});
			});
		}
	}
	group.finish();
}

/// A front minted, resolved, and retired once its holder leaves, among `fronts`
/// others held open. A retiring front leaves the table it shares with them, so a
/// cost that grows with the table rather than with the one path shows up as a slope.
fn bench_retire(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/retire");
	for fronts in [100, 1_000, 10_000] {
		group.bench_function(BenchmarkId::from_parameter(format!("{fronts}f")), |b| {
			let mut fleet = fanout(fronts + 1, 0);
			let waiter = kio::Waiter::noop();
			let held: Vec<_> = (0..fronts)
				.map(|i| fleet.consumer.request(format!("room/{i}"), None))
				.collect();
			fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			let _held: Vec<_> = held
				.into_iter()
				.map(|pending| pending.now_or_never().expect("resolved once driven").expect("local"))
				.collect();
			let path = format!("room/{fronts}");
			b.iter(|| {
				let pending = fleet.consumer.request(&path, None);
				fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
				drop(pending.now_or_never().expect("resolved once driven").expect("local"));
				// Nothing holds it and a local source keeps no track, so it retires here.
				fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			});
		});
	}
	group.finish();
}

/// `(members, paths)` shapes for an equal-cost pool: how many advertisers claim
/// one prefix, against how many paths beneath it are already being served.
const POOL: [usize; 3] = [4, 32, 256];
const POOL_PATHS: [usize; 3] = [100, 1_000, 10_000];

/// Announce cursors watching the pool, against [`POOL_PATHS`].
const POOL_CURSORS: [usize; 3] = [1, 100, 1_000];

/// Hop ids for pool members, clear of the origin's own.
const POOL_HOP: u64 = 1_000;

/// An origin where `members` equal-cost advertisers claim `pool`, already
/// serving `paths` requested paths beneath it, spread across the pool by the
/// hash on each path, and watched by `cursors` announce cursors. Every handle
/// is held so the fronts stay up.
struct Pool {
	producer: origin::Producer,
	driver: origin::Driver,
	_members: Vec<origin::Requests>,
	_producers: Vec<broadcast::Producer>,
	_consumers: Vec<broadcast::Consumer>,
	cursors: Vec<announce::Consumer>,
}

impl Pool {
	/// Discard what the cursors were delivered, so each iteration measures only what it adds.
	fn drain(&mut self) {
		for cursor in &mut self.cursors {
			while cursor.next().now_or_never().flatten().is_some() {}
		}
	}
}

fn pool(members: usize, paths: usize, cursors: usize) -> Pool {
	let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	let consumer = producer.consume();
	let cursors = (0..cursors).map(|_| consumer.announced()).collect();
	let members: Vec<_> = (0..members)
		.map(|i| {
			moq_net::fuzz::requests_route(
				&producer,
				moq_net::Path::from_static("pool"),
				pool_route(POOL_HOP + i as u64),
			)
			.unwrap()
		})
		.collect();
	let waiter = kio::Waiter::noop();

	let requests: Vec<_> = (0..paths)
		.map(|i| consumer.request(format!("pool/job-{i}"), None))
		.collect();
	driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	let mut producers = Vec::with_capacity(paths);
	for member in &members {
		while let Poll::Ready(request) = member.poll_requested_broadcast(&waiter) {
			let broadcast = broadcast::Info::new().produce();
			moq_net::fuzz::accept(request.unwrap(), &broadcast);
			producers.push(broadcast);
		}
	}
	assert_eq!(producers.len(), paths, "every path reached a member");
	driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	let consumers = requests
		.into_iter()
		.map(|request| request.now_or_never().expect("resolved once driven").expect("served"))
		.collect();

	let mut pool = Pool {
		producer,
		driver,
		_members: members,
		_producers: producers,
		_consumers: consumers,
		cursors,
	};
	pool.drain();
	pool
}

/// A pool member's claim: one hop, at the same cost as every other member.
fn pool_route(hop: u64) -> origin::Route {
	peer_route(hop, 3)
}

/// Re-requesting an epoch's stale fronts, swept over the cache around them and the
/// number restarted. Setup, route updates, and teardown stay outside the measurement.
fn bench_restart_fronts(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/restart_fronts");
	for fronts in [100, 1_000, 10_000] {
		for restarting in [1, 10, 100] {
			group.throughput(Throughput::Elements(restarting));
			group.bench_function(format!("{fronts}f_{restarting}r"), |b| {
				b.iter_batched_ref(
					|| {
						let pool = pool(1, fronts, 0);
						moq_net::fuzz::update_route(&pool._members[0], pool_route(POOL_HOP).with_epoch(Epoch::mint()))
							.unwrap();
						let paths: Vec<_> = (0..restarting).map(|i| format!("pool/job-{i}")).collect();
						(pool, paths)
					},
					|(pool, paths)| {
						let consumer = pool.producer.consume();
						paths
							.iter()
							.map(|path| consumer.request(path, None))
							.collect::<Vec<_>>()
					},
					BatchSize::PerIteration,
				);
			});
		}
	}
	group.finish();
}

/// A pool member taken over in place and retired, under `fronts` fronts resolved
/// through it, against the same member restarting without a takeover. Each change
/// pokes every front beneath the prefix, and the takeover carries each one's verdict
/// forward under the table lock, so the takeover's cost over the restart is that pass.
/// The fronts' tasks never run: this is the install alone.
fn bench_takeover_fronts(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/takeover_fronts");
	for fronts in [100, 1_000, 10_000] {
		for takeover in [false, true] {
			let kind = if takeover { "takeover" } else { "restart" };
			group.bench_function(format!("{fronts}f_{kind}"), |b| {
				let mut pool = pool(1, fronts, 0);
				let mut last = pool._members.pop().expect("one member");
				let mut hop = POOL_HOP + 1;
				b.iter(|| {
					hop += 1;
					let next = match takeover {
						true => moq_net::fuzz::takeover_route(
							&pool.producer,
							moq_net::Path::from_static("pool"),
							pool_route(hop),
							&last,
						),
						false => moq_net::fuzz::requests_route(
							&pool.producer,
							moq_net::Path::from_static("pool"),
							pool_route(hop),
						),
					};
					drop(std::mem::replace(&mut last, next.unwrap()));
				});
			});
		}
	}
	group.finish();
}

/// A member joining and leaving a pool that already serves `paths` paths.
///
/// Every front under the prefix watches the routes covering it, so each one
/// re-selects on both changes, and each selection scans the pool: the cost is
/// paths times members by design, which this sweep makes visible. The fronts the
/// newcomer outranks request through it, and fall back when it leaves before
/// answering.
fn bench_pool_churn(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/pool_churn");
	// The widest shapes take a noticeable fraction of a second per iteration.
	group.sample_size(10);
	for members in POOL {
		for paths in POOL_PATHS {
			let id = BenchmarkId::from_parameter(format!("{members}m_{paths}p"));
			group.bench_function(id, |b| {
				let mut pool = pool(members, paths, 0);
				let waiter = kio::Waiter::noop();
				let hop = POOL_HOP + members as u64;
				b.iter(|| {
					let joined = moq_net::fuzz::requests_route(
						&pool.producer,
						moq_net::Path::from_static("pool"),
						pool_route(hop),
					)
					.unwrap();
					pool.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
					drop(joined);
					pool.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
				});
			});
		}
	}
	group.finish();
}

/// [`bench_pool_churn`] at 32 members, watched by `cursors` announce cursors.
///
/// A front whose path the newcomer takes checks whether every cursor on the
/// prefix already restarted for it before renewing the prefix, so the moved
/// fronts times the cursors is the slope to watch.
fn bench_pool_cursors(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/pool_cursors");
	group.sample_size(10);
	let members = 32;
	for paths in POOL_PATHS {
		for cursors in POOL_CURSORS {
			let id = BenchmarkId::from_parameter(format!("{paths}p_{cursors}c"));
			group.bench_function(id, |b| {
				let mut pool = pool(members, paths, cursors);
				let waiter = kio::Waiter::noop();
				let hop = POOL_HOP + members as u64;
				b.iter(|| {
					let joined = moq_net::fuzz::requests_route(
						&pool.producer,
						moq_net::Path::from_static("pool"),
						pool_route(hop),
					)
					.unwrap();
					pool.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
					drop(joined);
					pool.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
					pool.drain();
				});
			});
		}
	}
	group.finish();
}

/// Publisher handoff at one path: a subscriber is reading from one local
/// source when a second announces at the same path and takes over (newest
/// wins). An open subscription stays on the old instance, so the subscriber
/// re-requests the broadcast. Measured from the standby's attach to the
/// re-request receiving its first group, with `publishers` unrelated
/// broadcasts in the table.
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
						let track = incumbent.create_track("video", timed()).unwrap();
						let mut first = track.append_group().unwrap();
						first.write_frame(Timestamp::ZERO, b"one".as_ref()).unwrap();
						first.finish().unwrap();

						let resolved = consumer.request("room/live", None).await.unwrap();
						let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
						subscription.recv_group().await.unwrap().expect("first group");

						let started = std::time::Instant::now();
						let standby = producer.create_broadcast("room/live").unwrap();
						let track = standby.create_track("video", timed()).unwrap();
						let mut second = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
						second.write_frame(Timestamp::ZERO, b"two".as_ref()).unwrap();
						second.finish().unwrap();
						// Announcing is what makes the standby a route the front can take.
						standby.announce(origin::Route::default()).unwrap();
						let replaced = consumer.request("room/live", None).await.unwrap();
						let mut replacement = replaced.track("video").unwrap().subscribe(None).await.unwrap();
						let group = replacement.recv_group().await.unwrap().expect("standby group");
						total += started.elapsed();
						// Rejoining the incumbent would replay its cached group 0 instead.
						assert_eq!(group.sequence, 1, "standby group");

						drop(subscription);
						drop(replacement);
						incumbent.close();
						standby.close();
						// Wait for the fronts to close so the next iteration starts fresh ones.
						resolved.closed().await;
						replaced.closed().await;
					}
					total
				})
			});
		});
	}
	group.finish();
}

/// `(routes, subscribers)` shapes for the narrowing benchmarks: [`SHAPES`] plus a
/// row that grows the routes alone, so each axis shows its own slope.
const NARROW_SHAPES: [(usize, usize); 4] = [(100, 10), (1_000, 10), (1_000, 100), (1_000, 1_000)];

/// A relay serving `routes` broadcasts to one client session subscribed to
/// `subscribers` of them. The first subscription is the one a narrowing to
/// `room/**` ends: its broadcast sits at `muted` instead.
struct Watching {
	pair: MockPair,
	tracks: Vec<track::Producer>,
	subscriptions: Vec<track::Subscriber>,
	_broadcasts: Vec<origin::Publication>,
	_origins: [origin::Producer; 2],
}

fn watched_path(index: usize) -> String {
	match index {
		0 => "muted".to_string(),
		index => format!("room/{index}"),
	}
}

async fn watching(routes: usize, subscribers: usize) -> Watching {
	let spawn = |hop| {
		let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
		support::harness::spawn(driver);
		producer
	};
	let relay = spawn(1);
	let received = spawn(2);

	let mut broadcasts = Vec::new();
	let mut tracks = Vec::new();
	for index in 0..routes {
		let broadcast = relay.publish(watched_path(index), origin::Route::default()).unwrap();
		tracks.push(broadcast.create_track("video", timed()).unwrap());
		broadcasts.push(broadcast);
	}

	let mut options = MockConnectOptions::new("moq-lite-06".parse().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(received.clone());
	let pair = connect_mock(options).await;

	let mut subscriptions = Vec::new();
	for index in 0..subscribers {
		let remote = received
			.consume()
			.follow(watched_path(index), None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let subscription = remote.track("video").unwrap().subscribe(None).await.unwrap();
		subscriptions.push(subscription);
	}
	// One round, so every subscription is live end to end before anything is timed.
	let mut watching = Watching {
		pair,
		tracks,
		subscriptions,
		_broadcasts: broadcasts,
		_origins: [relay, received],
	};
	watching.round().await;
	watching
}

impl Watching {
	/// Every subscribed track writes a group, and every subscription reads it.
	async fn round(&mut self) {
		for track in &self.tracks[..self.subscriptions.len()] {
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
			group.finish().unwrap();
		}
		for subscription in &mut self.subscriptions {
			subscription.recv_group().await.unwrap().expect("track ended");
		}
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

/// Everything under `room/`, which leaves out only the `muted` subscription.
fn room() -> auth::Grant {
	auth::Grant {
		subscribe: Pattern::subtree("room").unwrap().into(),
		..auth::Grant::all()
	}
}

/// Steady-state delivery through a session's gates: one round across every
/// subscription, on a session that never narrows and on one narrowed to a grant
/// that still covers them all. Neither may depend on `routes`.
fn bench_narrow_delivery(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/narrow_delivery");
	let rt = runtime();
	for (routes, subscribers) in NARROW_SHAPES {
		group.throughput(Throughput::Elements(subscribers as u64));
		for narrowed in [false, true] {
			let label = if narrowed { "narrowed" } else { "never" };
			let id = BenchmarkId::new(label, format!("{routes}r_{subscribers}s"));
			group.bench_function(id, |b| {
				let mut watching = rt.block_on(watching(routes, subscribers));
				if narrowed {
					// Covers every subscription but the muted one, which is not subscribed here.
					let mut grant = room();
					grant.subscribe.insert(Pattern::literal("muted").unwrap());
					watching.pair.server.auth().authorize(&grant);
				}
				b.iter(|| rt.block_on(watching.round()));
			});
		}
	}
	group.finish();
}

/// One narrowing on a live session: from `narrow` until the subscription it
/// excludes resets. It wakes every subscription's gate and re-checks every route
/// the session announced, so it may grow with the session's own `subscribers`
/// and `routes`, never with anything else on the origin.
fn bench_narrow(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/narrow");
	group.sample_size(10);
	group.sampling_mode(criterion::SamplingMode::Flat);
	let rt = runtime();
	for (routes, subscribers) in NARROW_SHAPES {
		let id = BenchmarkId::from_parameter(format!("{routes}r_{subscribers}s"));
		group.bench_function(id, |b| {
			b.iter_custom(|iters| {
				let mut elapsed = Duration::ZERO;
				for _ in 0..iters {
					let mut watching = rt.block_on(watching(routes, subscribers));
					let start = std::time::Instant::now();
					watching.pair.server.auth().authorize(&room());
					let ended = rt.block_on(watching.subscriptions[0].recv_group());
					elapsed += start.elapsed();
					assert!(ended.is_err(), "the muted subscription outlived the narrowing");
				}
				elapsed
			});
		});
	}
	group.finish();
}

/// `(tracks, readers per track)` shapes for [`bench_relay`].
const RELAY: [(usize, usize); 4] = [(1, 1), (1, 100), (100, 1), (10, 100)];

/// Frames per group in [`bench_relay`].
const RELAY_FRAMES: usize = 10;

/// Steady-state delivery through a front: one group of [`RELAY_FRAMES`] frames per
/// track, read in full by every reader of the broadcast the front serves. Readers
/// drain the serving route's shared group cache directly, so measure both the track
/// and reader axes.
fn bench_relay(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/relay");
	for (tracks, readers) in RELAY {
		group.throughput(Throughput::Elements((tracks * readers * RELAY_FRAMES) as u64));
		group.bench_function(BenchmarkId::from_parameter(format!("{tracks}t_{readers}r")), |b| {
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			let (producer, driver) = origin::Producer::new(origin::Config::default());
			runtime.spawn(moq_net::time::run(driver));
			let broadcast = producer.publish("room/live", origin::Route::default()).unwrap();
			let sources: Vec<_> = (0..tracks)
				.map(|i| broadcast.create_track(format!("{i}"), timed()).unwrap())
				.collect();
			let mut subscriptions = runtime.block_on(async {
				let resolved = producer.consume().request("room/live", None).await.unwrap();
				let mut subscriptions = Vec::new();
				for i in 0..tracks {
					let track = resolved.track(&format!("{i}")).unwrap();
					for _ in 0..readers {
						subscriptions.push(track.subscribe(None).await.unwrap());
					}
				}
				subscriptions
			});

			b.iter_custom(|iterations| {
				runtime.block_on(async {
					let started = std::time::Instant::now();
					for _ in 0..iterations {
						for source in &sources {
							let mut group = source.append_group().unwrap();
							for _ in 0..RELAY_FRAMES {
								group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
							}
							group.finish().unwrap();
						}
						for subscription in &mut subscriptions {
							let mut group = subscription.recv_group().await.unwrap().expect("group");
							for _ in 0..RELAY_FRAMES {
								group.read_frame().await.unwrap().expect("frame");
							}
						}
					}
					started.elapsed()
				})
			});
		});
	}
	group.finish();
}

/// `(readers, open)` shapes for [`bench_passthrough`]: readers of one track, and the
/// groups each holds open at once.
const PASSTHROUGH: [(usize, usize); 4] = [(1, 1), (1, 64), (100, 1), (100, 64)];

/// One-frame groups read through a front that only ever had one route, so each handed-out
/// group's per-reader record (kept for a later route to back it) is a visible share of the
/// cost. Each reader holds `open` groups at once, the live count that record tracks.
fn bench_passthrough(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/passthrough");
	for (readers, open) in PASSTHROUGH {
		group.throughput(Throughput::Elements((readers * open) as u64));
		group.bench_function(BenchmarkId::from_parameter(format!("{readers}r_{open}g")), |b| {
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			let (producer, driver) = origin::Producer::new(origin::Config::default());
			runtime.spawn(moq_net::time::run(driver));
			let broadcast = producer.publish("room/live", origin::Route::default()).unwrap();
			let source = broadcast.create_track("0", timed()).unwrap();
			let mut subscriptions = runtime.block_on(async {
				let resolved = producer.consume().request("room/live", None).await.unwrap();
				let track = resolved.track("0").unwrap();
				let mut subscriptions = Vec::with_capacity(readers);
				for _ in 0..readers {
					subscriptions.push(track.subscribe(None).await.unwrap());
				}
				subscriptions
			});
			let mut held: Vec<Vec<moq_net::group::Consumer>> = (0..readers).map(|_| Vec::with_capacity(open)).collect();

			b.iter_custom(|iterations| {
				runtime.block_on(async {
					let started = std::time::Instant::now();
					for _ in 0..iterations {
						// Received as each is written: a reader behind the live edge skips ahead.
						for _ in 0..open {
							let mut group = source.append_group().unwrap();
							group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
							group.finish().unwrap();
							for (subscription, held) in subscriptions.iter_mut().zip(&mut held) {
								held.push(subscription.recv_group().await.unwrap().expect("group"));
							}
						}
						for held in &mut held {
							for group in held.iter_mut() {
								group.read_frame().await.unwrap().expect("frame");
							}
							held.clear();
						}
					}
					started.elapsed()
				})
			});
		});
	}
	group.finish();
}

/// Re-poll stalled groups through a front, including their subscription budget.
/// Sweep tracks and readers separately to expose unrelated table scans.
fn bench_parked(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/parked");
	for tracks in [1, 100] {
		for readers in [1, 100] {
			group.throughput(Throughput::Elements((tracks * readers) as u64));
			group.bench_function(BenchmarkId::from_parameter(format!("{tracks}t_{readers}r")), |b| {
				let runtime = tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.unwrap();
				let (producer, driver) = origin::Producer::new(origin::Config::default());
				runtime.spawn(moq_net::time::run(driver));
				let broadcast = producer.publish("room/live", origin::Route::default()).unwrap();
				let sources: Vec<_> = (0..tracks)
					.map(|i| broadcast.create_track(format!("{i}"), None).unwrap())
					.collect();
				let _writers: Vec<_> = sources.iter().map(|source| source.append_group().unwrap()).collect();
				let (subscriptions, mut groups) = runtime.block_on(async {
					let resolved = producer.consume().request("room/live", None).await.unwrap();
					let mut subscriptions = Vec::new();
					let mut groups = Vec::new();
					for i in 0..tracks {
						let track = resolved.track(&format!("{i}")).unwrap();
						for _ in 0..readers {
							let mut sub = track.subscribe(None).await.unwrap();
							groups.push(sub.recv_group().await.unwrap().unwrap());
							subscriptions.push(sub);
						}
					}
					(subscriptions, groups)
				});
				let waiter = kio::Waiter::noop();
				b.iter(|| {
					for group in &mut groups {
						assert!(group.poll_read_frame(&waiter).is_pending());
					}
				});
				drop(subscriptions);
			});
		}
	}
	group.finish();
}

/// `(tracks, copies)` for [`bench_copy_walk`]: tracks one front is serving, and
/// route copies each of those tracks still holds when the route swaps.
///
/// The single-copy points from 32 to 128 tracks show the per-track slope alone.
const COPY_WALK: [(usize, usize); 8] = [(1, 1), (1, 32), (32, 1), (64, 1), (128, 1), (8, 32), (32, 8), (64, 16)];

const COPY_PATH: &str = "room/live";

/// How long a splice may take before the bench treats it as stuck.
const COPY_WAIT: Duration = Duration::from_secs(5);

/// One generation at [`COPY_PATH`], held so its copies stay subscribed.
struct Generation {
	broadcast: origin::Publication,
	tracks: Vec<track::Producer>,
}

impl Generation {
	/// Create a hidden generation and leave a finished group on every track, so a
	/// reader that does look has something to resume.
	fn build(producer: &origin::Producer, tracks: usize) -> Self {
		let broadcast = producer.create_broadcast(COPY_PATH).unwrap();
		let mut held = Vec::with_capacity(tracks);
		for i in 0..tracks {
			let track = broadcast.create_track(format!("{i}"), timed()).unwrap();
			let mut written = track.append_group().unwrap();
			written.write_frame(Timestamp::ZERO, b"f".as_ref()).unwrap();
			written.finish().unwrap();
			held.push(track);
		}
		Self {
			broadcast,
			tracks: held,
		}
	}

	/// Route the front to this generation without waiting.
	fn route(&self, epoch: &Epoch) {
		self.broadcast
			.announce(origin::Route::default().with_epoch(epoch.clone()))
			.unwrap();
	}

	/// Route the front to this generation and wait until every track is spliced.
	///
	/// The wait ends on demand, which flips when the front subscribes the new
	/// copy, before its splice. That bounds the splice only on a current-thread
	/// runtime, where every local query is ready within the front poll that
	/// splices it.
	async fn announce(&self, epoch: &Epoch) {
		self.route(epoch);
		for track in &self.tracks {
			kio::clock::timeout(COPY_WAIT, support::wanted(track.demand()))
				.await
				.expect("route swap did not splice")
				.expect("replacement track closed");
		}
	}
}

/// A front serving [`COPY_PATH`], with `copies` route copies on each of its tracks.
struct CopyWalk {
	epoch: Epoch,
	_producer: origin::Producer,
	_subscribers: Vec<track::Subscriber>,
	generations: Vec<Generation>,
	/// The generation the timed swap announces, built during setup.
	next: Option<Generation>,
}

/// Build a front with `copies` already spliced onto each track.
///
/// Subscribers are never polled. A poll drops a replaced copy once its groups
/// are delivered, and this measures the walk over the copies a swap still has.
/// The same is why every generation is kept: dropping one ends its copy.
async fn copy_walk(tracks: usize, copies: usize) -> CopyWalk {
	assert!(copies >= 1, "a front holds at least the serving copy");
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let epoch = Epoch::mint();
	let first = Generation::build(&producer, tracks);
	first.route(&epoch);
	let resolved = kio::clock::timeout(COPY_WAIT, producer.consume().request(COPY_PATH, None))
		.await
		.expect("front did not resolve")
		.expect("front refused");
	let mut subscribers = Vec::with_capacity(tracks);
	for i in 0..tracks {
		let track = resolved.track(&format!("{i}")).unwrap();
		let subscriber = kio::clock::timeout(COPY_WAIT, track.subscribe(None))
			.await
			.expect("track did not splice")
			.expect("subscribe failed");
		subscribers.push(subscriber);
	}
	let mut generations = Vec::with_capacity(copies + 1);
	generations.push(first);
	for _ in 1..copies {
		let generation = Generation::build(&producer, tracks);
		generation.announce(&epoch).await;
		generations.push(generation);
	}
	CopyWalk {
		next: Some(Generation::build(&producer, tracks)),
		epoch,
		_producer: producer,
		_subscribers: subscribers,
		generations,
	}
}

impl CopyWalk {
	/// Swap in the prebuilt newer route at the same epoch.
	///
	/// A local track's info is already known, so the front subscribes, walks
	/// every track, and each reader walks its copies in one driver turn.
	async fn swap(&mut self) {
		let generation = self.next.take().expect("one swap per setup");
		generation.announce(&self.epoch).await;
		self.generations.push(generation);
	}
}

/// A route swap on one front. Setup holds `copies` per track and builds the
/// next generation, untimed; the announce and splice are timed. Warm-up sizes
/// the iteration count from wall time, which includes that setup, so a large
/// front does not multiply it by a fast routine.
///
/// The runtime must stay current-thread; see [`Generation::announce`].
fn bench_copy_walk(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/copy_walk");
	group.sample_size(10);
	for (tracks, copies) in COPY_WALK {
		group.throughput(Throughput::Elements(tracks as u64));
		group.bench_function(BenchmarkId::from_parameter(format!("{tracks}t_{copies}c")), |b| {
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			b.iter_batched_ref(
				|| runtime.block_on(copy_walk(tracks, copies)),
				|rig| runtime.block_on(rig.swap()),
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

/// `(readers, retained, routes)` for [`bench_resume_overlap`]: readers of one track,
/// the groups every route holds, and the routes it swapped through.
const RESUME_OVERLAP: [(usize, usize, usize); 17] = [
	(1, 1, 1),
	(1, 64, 1),
	(32, 64, 1),
	(1, 1, 2),
	(1, 64, 2),
	(1, 1024, 2),
	(8, 1, 2),
	(8, 64, 2),
	(8, 1024, 2),
	(32, 64, 2),
	(256, 64, 2),
	(32, 1024, 2),
	(1, 64, 8),
	(1, 64, 32),
	(32, 64, 8),
	(32, 64, 32),
	(256, 64, 8),
];

/// A track that swapped through its routes while its readers' local cap held every
/// group back, so each reader keeps a copy per route and every copy holds the same
/// groups, each replacement in reverse arrival order.
struct ResumeOverlap {
	_producer: origin::Producer,
	_generations: Vec<(origin::Publication, track::Producer)>,
	readers: Vec<ResumeReader>,
}

impl ResumeOverlap {
	#[inline(never)]
	fn drain(&mut self, retained: usize) {
		let waiter = kio::Waiter::noop();
		for reader in &mut self.readers {
			let mut delivered = 0;
			while let Poll::Ready(Ok(Some(_))) = reader.poll_group(&waiter) {
				delivered += 1;
			}
			assert_eq!(delivered, retained, "every group once");
		}
	}
}

enum ResumeReader {
	Arrival(track::Subscriber),
	Sequence(track::Ordered),
}

impl ResumeReader {
	fn poll_group(&mut self, waiter: &kio::Waiter) -> Poll<moq_net::Result<Option<moq_net::group::Consumer>>> {
		match self {
			Self::Arrival(reader) => reader.poll_recv_group(waiter),
			Self::Sequence(reader) => reader.poll_next_group(waiter),
		}
	}
}

struct ResumeShape {
	readers: usize,
	retained: usize,
	routes: usize,
	waiting: bool,
}

async fn resume_overlap(shape: ResumeShape, ordered: bool) -> ResumeOverlap {
	let ResumeShape {
		readers,
		retained,
		routes,
		waiting,
	} = shape;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let epoch = Epoch::mint();
	let generation = |order: &mut dyn Iterator<Item = usize>| {
		let broadcast = producer.create_broadcast(COPY_PATH).unwrap();
		let track = broadcast.create_track("0", timed()).unwrap();
		for sequence in order {
			let mut group = track
				.create_group(moq_net::group::Info {
					sequence: sequence as u64,
				})
				.unwrap();
			group.write_frame(Timestamp::ZERO, b"f".as_ref()).unwrap();
			group.finish().unwrap();
		}
		broadcast
			.announce(origin::Route::default().with_epoch(epoch.clone()))
			.unwrap();
		(broadcast, track)
	};
	let mut generations = vec![generation(&mut (0..retained))];
	let resolved = kio::clock::timeout(COPY_WAIT, producer.consume().request(COPY_PATH, None))
		.await
		.expect("front did not resolve")
		.expect("front refused");
	let track = resolved.track("0").unwrap();
	// A budget covering every group, so none is written off as late.
	let subscription = track::Subscription::default().with_max_delay(Duration::from_secs(3600));
	let mut subscribers = Vec::with_capacity(readers);
	for _ in 0..readers {
		let subscriber = kio::clock::timeout(COPY_WAIT, track.subscribe(subscription.clone()))
			.await
			.expect("track did not splice")
			.expect("subscribe failed");
		// Held back until the drain, so every route's copy is kept for its unread groups.
		let subscriber = subscriber.with_range(..0);
		subscribers.push(subscriber);
	}
	let waiter = kio::Waiter::noop();
	for route in 1..routes + usize::from(waiting) {
		generations.push(if waiting && route == routes {
			// The serving copy has answered but not produced a group. It has not yet
			// bounded the overlapping copies, which must still skip each other's replay.
			generation(&mut std::iter::empty())
		} else {
			generation(&mut (0..retained).rev())
		});
		let (_, latest) = generations.last().unwrap();
		kio::clock::timeout(COPY_WAIT, support::wanted(latest.demand()))
			.await
			.expect("route swap did not splice")
			.expect("replacement track closed");
		// Each reader follows the swap, subscribing to the new copy.
		for subscriber in &mut subscribers {
			assert!(
				subscriber.poll_recv_group(&waiter).is_pending(),
				"the cap holds every group"
			);
		}
	}
	let readers = subscribers
		.into_iter()
		.map(|subscriber| subscriber.with_range(..))
		.map(|subscriber| match ordered {
			true => ResumeReader::Sequence(subscriber.ordered()),
			false => ResumeReader::Arrival(subscriber),
		})
		.collect();
	ResumeOverlap {
		_producer: producer,
		_generations: generations,
		readers,
	}
}

/// Readers draining a track whose routes all delivered every group: each group is
/// handed out once, and every other copy's duplicate is skipped against what the reader
/// handed out. The slopes over retained groups and routes are that skipping's cost.
fn bench_resume_overlap(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/resume_overlap");
	group.sample_size(10);
	for (order, ordered, waiting) in [
		("arrival", false, false),
		("sequence", true, false),
		("arrival_waiting", false, true),
		("sequence_waiting", true, true),
	] {
		for (readers, retained, routes) in RESUME_OVERLAP {
			if waiting && !(readers == 1 && retained == 64 && routes >= 2) {
				continue;
			}
			group.throughput(Throughput::Elements((readers * retained) as u64));
			group.bench_function(
				BenchmarkId::new(order, format!("{readers}r_{retained}g_{routes}c")),
				|b| {
					let runtime = tokio::runtime::Builder::new_current_thread()
						.enable_all()
						.build()
						.unwrap();
					b.iter_batched_ref(
						|| {
							runtime.block_on(resume_overlap(
								ResumeShape {
									readers,
									retained,
									routes,
									waiting,
								},
								ordered,
							))
						},
						|rig| rig.drain(retained),
						BatchSize::PerIteration,
					);
				},
			);
		}
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_announce,
	bench_announce_covering,
	bench_pending_churn,
	bench_withdraw,
	bench_withdraw_wave,
	bench_route_churn,
	bench_announce_mounted,
	bench_announce_fleet,
	bench_announce_duplicate,
	bench_reprice_duplicate,
	bench_announce_fronts,
	bench_takeover,
	bench_serve_idle,
	bench_subscribe,
	bench_request,
	bench_retire,
	bench_restart_fronts,
	bench_pool_churn,
	bench_takeover_fronts,
	bench_pool_cursors,
	bench_handoff,
	bench_narrow_delivery,
	bench_narrow,
	bench_relay,
	bench_passthrough,
	bench_parked,
	bench_copy_walk,
	bench_resume_overlap
);
criterion_main!(benches);

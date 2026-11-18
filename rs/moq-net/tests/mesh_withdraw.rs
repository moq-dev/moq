//! Broadcasts withdrawn from a meshed cluster retract everywhere, once.
//!
//! Every relay holds a route through each peer that re-advertised a broadcast.
//! When the publisher's relay withdraws it, those routes all derive from the one
//! withdrawn; a relay that selects them in turn re-advertises each stale path, and
//! the cluster walks all of them before it converges (path hunting).

mod support;

use std::{collections::HashMap, time::Duration};

use tokio::sync::mpsc;

use moq_net::{Hop, Hops, Version, announce, broadcast, origin};
use support::{
	harness::{MockPair, peer, peer_with_latency},
	production::{PRODUCTION, PRODUCTION_LATENCY_MS},
};

fn produce_origin(hop: u64) -> origin::Producer {
	produce_capped(hop, origin::HopLimit::default())
}

fn produce_capped(hop: u64, max_hops: origin::HopLimit) -> origin::Producer {
	let mut config = origin::Config::new(Hop::new(hop).unwrap());
	config.max_hops = max_hops;
	let (producer, driver) = origin::Producer::new(config);
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Every update per prefix until the watcher goes quiet. Time is paused, so the
/// timeout fires only once every task is idle.
async fn drain(
	watched: &mut mpsc::UnboundedReceiver<(String, announce::Kind)>,
) -> HashMap<String, Vec<announce::Kind>> {
	let mut updates = HashMap::<String, Vec<announce::Kind>>::new();
	while let Ok(Some((prefix, kind))) = tokio::time::timeout(Duration::from_secs(1), watched.recv()).await {
		updates.entry(prefix).or_default().push(kind);
	}
	updates
}

/// Watch `announced` from its own task, the way a session's announce writer does:
/// it runs when woken, between the relays' own tasks, rather than only once the
/// test task is polled again, which would coalesce every intermediate update.
fn watch(mut announced: announce::Consumer) -> mpsc::UnboundedReceiver<(String, announce::Kind)> {
	let (tx, rx) = mpsc::unbounded_channel();
	tokio::spawn(async move {
		while let Some(update) = announced.next().await {
			if tx.send((update.prefix.to_string(), update.kind)).is_err() {
				break;
			}
		}
	});
	rx
}

/// `n` relays meshed over `edges`, watched from the last one.
struct Mesh {
	nodes: Vec<origin::Producer>,
	_pairs: Vec<MockPair>,
	watched: mpsc::UnboundedReceiver<(String, announce::Kind)>,
}

impl Mesh {
	async fn new(version: &str, n: u64, edges: &[(usize, usize)]) -> Self {
		let version: Version = version.parse().unwrap();
		let nodes: Vec<_> = (1..=n).map(produce_origin).collect();
		let mut pairs = Vec::new();
		for &(a, b) in edges {
			pairs.push(peer(version, &nodes[a], &nodes[b]).await);
		}
		let watched = watch(nodes.last().unwrap().consume().announced());
		Self {
			nodes,
			_pairs: pairs,
			watched,
		}
	}

	/// Publish `count` broadcasts spread over every relay but the watcher, starting
	/// at relay `offset`, and require the watcher to see each announced.
	async fn publish(&mut self, count: usize, offset: usize) -> Vec<broadcast::Producer> {
		let relays = self.nodes.len() - 1;
		let broadcasts = (0..count)
			.map(|i| {
				let broadcast = self.nodes[(i + offset) % relays]
					.create_broadcast(format!("room/{i}"))
					.unwrap();
				broadcast.announce(Default::default()).unwrap();
				broadcast
			})
			.collect();
		let updates = drain(&mut self.watched).await;
		assert_eq!(updates.len(), count);
		for (prefix, kinds) in updates {
			assert_eq!(kinds[0], announce::Kind::Announced, "{prefix}: {kinds:?}");
			assert!(kinds.last().unwrap().is_active(), "{prefix}: {kinds:?}");
		}
		broadcasts
	}
}

fn full_mesh(n: usize) -> Vec<(usize, usize)> {
	(0..n).flat_map(|a| (a + 1..n).map(move |b| (a, b))).collect()
}

/// A ring with chords: most relays reach a publisher's relay only through others.
fn ring_with_chords(n: usize) -> Vec<(usize, usize)> {
	(0..n).flat_map(|a| [(a, (a + 1) % n), (a, (a + 3) % n)]).collect()
}

/// Every relay neighbors the publisher's, so each hears the withdrawal first-hand
/// and drops every path derived from it at once. Lite04 names the peer only in
/// the chain, later versions in the announce handshake too.
#[tokio::test(start_paused = true)]
async fn full_mesh_withdraw_retracts_once_lite04() {
	full_mesh_withdraw_retracts_once("moq-lite-04").await;
}

#[tokio::test(start_paused = true)]
async fn full_mesh_withdraw_retracts_once_lite06() {
	full_mesh_withdraw_retracts_once("moq-lite-06").await;
}

async fn full_mesh_withdraw_retracts_once(version: &str) {
	let mut mesh = Mesh::new(version, 8, &full_mesh(8)).await;
	let broadcasts = mesh.publish(100, 0).await;
	drop(broadcasts);
	let updates = drain(&mut mesh.watched).await;
	assert_eq!(updates.len(), 100);
	for (prefix, kinds) in updates {
		assert_eq!(kinds, [announce::Kind::Retracted], "{prefix}");
	}
	// The same relays publish again, clearing their own withdrawals.
	let _broadcasts = mesh.publish(100, 0).await;
}

/// A relay two hops from the publisher's hears only that its neighbor withdrew, not
/// why, so it can still pass through a stale path or two. Every broadcast must still
/// end retracted, and republishing from other relays must reach the watcher again:
/// no withdrawal outlives the peer announcing the path again.
#[tokio::test(start_paused = true)]
async fn partial_mesh_withdraw_then_republish() {
	let mut mesh = Mesh::new("moq-lite-06", 12, &ring_with_chords(12)).await;
	let broadcasts = mesh.publish(100, 0).await;
	drop(broadcasts);
	let updates = drain(&mut mesh.watched).await;
	assert_eq!(updates.len(), 100);
	for (prefix, kinds) in updates {
		assert!(!kinds.last().unwrap().is_active(), "{prefix}: {kinds:?}");
	}
	let _broadcasts = mesh.publish(100, 5).await;
}

/// One withdrawal on a partial mesh retracts everywhere once. A relay two hops
/// from the publisher's hears only that its neighbor withdrew, and must not
/// fall back to, and re-advertise, another path derived from the same
/// announcement. Holding route updates (`origin::Config::update_hold`) lets the
/// withdrawal remove those paths first; with link latency and no hold, each
/// withdrawal here costs tens of thousands of announcements.
#[tokio::test(start_paused = true)]
async fn partial_mesh_withdraw_retracts_once() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let nodes: Vec<_> = (1..=34).map(produce_origin).collect();
	let mut pairs = Vec::new();
	for (&(a, b), &ms) in PRODUCTION.iter().zip(PRODUCTION_LATENCY_MS) {
		pairs.push(peer_with_latency(version, &nodes[a], &nodes[b], Duration::from_millis(ms)).await);
	}
	let mut watched: Vec<_> = nodes.iter().map(|node| watch(node.consume().announced())).collect();

	for publisher in [5, 13, 22, 26, 32] {
		let broadcast = nodes[publisher].create_broadcast(format!("room/{publisher}")).unwrap();
		broadcast.announce(Default::default()).unwrap();
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = drain(watched).await;
			assert!(
				updates.values().all(|kinds| kinds.last().unwrap().is_active()),
				"relay {relay}: {updates:?}"
			);
		}

		drop(broadcast);
		let mut hunted = Vec::new();
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = drain(watched).await;
			let kinds = &updates[&format!("room/{publisher}")];
			if kinds != &[announce::Kind::Retracted] {
				hunted.push((relay, kinds.clone()));
			}
		}
		assert!(hunted.is_empty(), "publisher {publisher}: {hunted:?}");
	}
}

/// Eight relays with link latencies in ms, publishing from the last.
#[rustfmt::skip]
const STALE_PATHS: &[((usize, usize), u64)] = &[
	((0, 1), 35), ((1, 2), 90), ((0, 3), 129), ((2, 4), 140), ((1, 5), 137), ((4, 6), 148),
	((2, 7), 30), ((3, 5), 124), ((1, 4), 111), ((0, 5), 21), ((3, 4), 34),
];

/// A relay that already retracted a withdrawn broadcast must not announce it
/// again. Its neighbors, two or more hops from the publisher's relay, still hunt
/// through stale paths; one that falls back to a path not through this relay
/// offers it as a new route after the relay forgot the withdrawal.
#[tokio::test(start_paused = true)]
async fn retracted_relay_ignores_a_stale_path() {
	const EDGES: &[((usize, usize), u64)] = STALE_PATHS;
	let version: Version = "moq-lite-06".parse().unwrap();
	let nodes: Vec<_> = (1..=8).map(produce_origin).collect();
	let mut pairs = Vec::new();
	for &((a, b), ms) in EDGES {
		pairs.push(peer_with_latency(version, &nodes[a], &nodes[b], Duration::from_millis(ms)).await);
	}
	let mut watched: Vec<_> = nodes.iter().map(|node| watch(node.consume().announced())).collect();

	let broadcast = nodes[7].create_broadcast("room/7").unwrap();
	broadcast.announce(Default::default()).unwrap();
	for watched in watched.iter_mut() {
		drain(watched).await;
	}

	drop(broadcast);
	let mut revived = Vec::new();
	for (relay, watched) in watched.iter_mut().enumerate() {
		let kinds = drain(watched).await.remove("room/7").unwrap();
		assert_eq!(
			kinds.last(),
			Some(&announce::Kind::Retracted),
			"relay {relay}: {kinds:?}"
		);
		if kinds
			.windows(2)
			.any(|w| w == [announce::Kind::Retracted, announce::Kind::Announced])
		{
			revived.push((relay, kinds));
		}
	}
	assert!(revived.is_empty(), "{revived:?}");
}

/// The longest shortest path between two relays over `edges`.
fn diameter(edges: &[((usize, usize), u64)]) -> usize {
	let n = edges.iter().map(|&((a, b), _)| a.max(b) + 1).max().unwrap();
	(0..n)
		.map(|start| {
			let mut distance = vec![usize::MAX; n];
			distance[start] = 0;
			let mut queue = std::collections::VecDeque::from([start]);
			while let Some(a) = queue.pop_front() {
				for &((x, y), _) in edges {
					let b = if x == a {
						y
					} else if y == a {
						x
					} else {
						continue;
					};
					if distance[b] == usize::MAX {
						distance[b] = distance[a] + 1;
						queue.push_back(b);
					}
				}
			}
			*distance.iter().max().unwrap()
		})
		.max()
		.unwrap()
}

/// With room for the diameter and two extra hops, including the publisher's own,
/// the cap only cuts detours: every relay still learns the broadcast and ends
/// retracted once it is withdrawn.
#[tokio::test(start_paused = true)]
async fn capped_mesh_withdraw_retracts() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let cap = origin::HopLimit::new(diameter(STALE_PATHS) + 2).unwrap();
	let nodes: Vec<_> = (1..=8).map(|id| produce_capped(id, cap)).collect();
	let mut pairs = Vec::new();
	for &((a, b), ms) in STALE_PATHS {
		pairs.push(peer_with_latency(version, &nodes[a], &nodes[b], Duration::from_millis(ms)).await);
	}
	let mut watched: Vec<_> = nodes.iter().map(|node| watch(node.consume().announced())).collect();

	let broadcast = nodes[7].create_broadcast("room/7").unwrap();
	broadcast.announce(Default::default()).unwrap();
	for (relay, watched) in watched.iter_mut().enumerate() {
		let kinds = drain(watched).await.remove("room/7").unwrap();
		assert!(kinds.last().unwrap().is_active(), "relay {relay}: {kinds:?}");
	}

	drop(broadcast);
	for (relay, watched) in watched.iter_mut().enumerate() {
		let kinds = drain(watched).await.remove("room/7").unwrap();
		assert_eq!(
			kinds.last(),
			Some(&announce::Kind::Retracted),
			"relay {relay}: {kinds:?}"
		);
	}
}

/// Every version whose wire carries a hop path.
const HOP_PATH_VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-17",
	"moq-transport-18",
	"moq-transport-19",
	"moq-transport-20",
	"moq-transport-21",
	"moq-transport-22",
];

/// A route whose chain is `count` hops starting at `first`, before the
/// announcing relay's own, at `cost`.
fn capped_route(first: u64, count: u64, cost: u64) -> origin::Route {
	let hops = (first..first + count)
		.map(|id| Hop::new(id).unwrap())
		.collect::<Vec<_>>();
	origin::Route::default()
		.with_hops(Hops::try_from(hops).unwrap())
		.with_cost(origin::Cost::new(cost))
}

/// A permissive source sends an over-limit path to prove ingress refusal; an
/// accepted boundary path then proves egress counts the relay's own stamp.
#[tokio::test(start_paused = true)]
async fn hop_limit_bounds_receive_and_send() {
	for &name in HOP_PATH_VERSIONS {
		let version: Version = name.parse().unwrap();
		// Lite03 carries only placeholder hops, so the relay names the unknown
		// publisher with a stamp of its own, one more entry in the stored chain.
		let stamp = usize::from(name == "moq-lite-03");
		let source = produce_origin(1);
		let relay = produce_capped(2, origin::HopLimit::new(2 + stamp).unwrap());
		let sink = produce_origin(3);
		let _upstream = peer(version, &source, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut received = watch(relay.consume().announced());
		let mut sent = watch(sink.consume().announced());
		drain(&mut received).await;

		let route = |count: u64| capped_route(100, count, 1);
		let _long = source.dynamic("long", route(2)).unwrap();
		let _boundary = source.dynamic("boundary", route(1)).unwrap();
		let short = source.dynamic("short", route(0)).unwrap();
		let received = drain(&mut received).await;
		assert!(!received.contains_key("long"), "{name}: accepted over-limit path");
		assert!(received.contains_key("boundary"), "{name}: refused boundary path");
		assert!(received.contains_key("short"), "{name}: refused short path");
		let sent_initial = drain(&mut sent).await;
		assert_eq!(sent_initial.len(), 1, "{name}: {sent_initial:?}");
		assert!(sent_initial.contains_key("short"), "{name}: forwarded over-limit path");

		if matches!(name, "moq-lite-03" | "moq-lite-04") {
			// These wires have no route metadata update: withdraw, then replace it.
			drop(short);
			assert_eq!(
				drain(&mut sent).await.get("short").and_then(|kinds| kinds.last()),
				Some(&announce::Kind::Retracted),
				"{name}"
			);
			let _replacement = source.dynamic("short", route(1)).unwrap();
			assert!(
				drain(&mut sent).await.is_empty(),
				"{name}: forwarded over-limit replacement"
			);
			continue;
		}
		short.update(route(1)).unwrap();
		let ended = drain(&mut sent).await;
		assert_eq!(
			ended.get("short").and_then(|kinds| kinds.last()),
			Some(&announce::Kind::Retracted),
			"{name}: {ended:?}"
		);
	}
}

/// A cheaper route at the cap wins locally but cannot be forwarded, so the relay
/// advertises the pricier in-cap route instead of withdrawing the prefix, and
/// withdraws only once no in-cap route is left.
#[tokio::test(start_paused = true)]
async fn hop_limit_falls_back_to_a_forwardable_route() {
	// Only wires carrying a route cost can rank a longer route first.
	for &name in HOP_PATH_VERSIONS {
		if matches!(name, "moq-lite-03" | "moq-lite-04" | "moq-lite-05") {
			continue;
		}
		let version: Version = name.parse().unwrap();
		let near = produce_origin(1);
		let far = produce_origin(4);
		let relay = produce_capped(2, origin::HopLimit::new(3).unwrap());
		let sink = produce_origin(3);
		let _near = peer(version, &near, &relay).await;
		let _far = peer(version, &far, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut local = watch(relay.consume().announced());
		let mut sent = watch(sink.consume().announced());
		drain(&mut sent).await;

		// Three hops at the relay once the sender is appended: in the cap, but full.
		let _cheap = far.dynamic("room", capped_route(100, 2, 1)).unwrap();
		// Two hops at the relay, so it has room for the relay's own.
		let pricey = near.dynamic("room", capped_route(200, 1, 50)).unwrap();
		let started = drain(&mut sent).await;
		assert_eq!(
			started.get("room").and_then(|kinds| kinds.last()),
			Some(&announce::Kind::Announced),
			"{name}: withdrew a prefix an in-cap route reaches: {started:?}"
		);
		// The relay itself still selects the cheaper route at the cap.
		assert!(drain(&mut local).await.contains_key("room"), "{name}");
		let mut selected = relay.consume().announced();
		let update = selected.next().await.unwrap();
		assert_eq!(update.route.hops.len(), 3, "{name}: {:?}", update.route);

		drop(pricey);
		let ended = drain(&mut sent).await;
		assert_eq!(
			ended.get("room").and_then(|kinds| kinds.last()),
			Some(&announce::Kind::Retracted),
			"{name}: forwarded a route at the cap: {ended:?}"
		);
	}
}

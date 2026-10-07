//! Broadcasts withdrawn from a meshed cluster retract everywhere, once.
//!
//! Every relay holds a route through each peer that re-advertised a broadcast.
//! When the publisher's relay withdraws it, those routes all derive from the one
//! withdrawn; a relay that selects them in turn re-advertises each stale path, and
//! the cluster walks all of them before it converges (path hunting).

mod support;

use std::{collections::HashMap, time::Duration};

use futures::{StreamExt, channel::mpsc};

use moq_net::{Hop, Version, announce, broadcast, origin};
use support::harness::{MockPair, peer, peer_with_latency};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The kind of an [`announce::Event`] for a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Start,
	Update,
	End,
}

/// Every update per prefix until the watcher goes quiet. Time is paused, so the
/// timeout fires only once every task is idle.
async fn drain(watched: &mut mpsc::UnboundedReceiver<(String, Kind)>) -> HashMap<String, Vec<Kind>> {
	let mut updates = HashMap::<String, Vec<Kind>>::new();
	while let Ok(Some((prefix, kind))) = moq_net_sim::timeout(Duration::from_secs(1), watched.next()).await {
		updates.entry(prefix).or_default().push(kind);
	}
	updates
}

/// Watch `announced` from its own task, the way a session's announce writer does:
/// it runs when woken, between the relays' own tasks, rather than only once the
/// test task is polled again, which would coalesce every intermediate update.
fn watch(mut announced: announce::Consumer) -> mpsc::UnboundedReceiver<(String, Kind)> {
	let (tx, rx) = mpsc::unbounded();
	moq_net_sim::spawn(async move {
		while let Some(event) = announced.next().await {
			let (kind, announce) = match event {
				announce::Event::Start(announce) => (Kind::Start, announce),
				announce::Event::Update(announce) => (Kind::Update, announce),
				announce::Event::End(announce) => (Kind::End, announce),
				announce::Event::Live => continue,
			};
			if tx.unbounded_send((announce.prefix.to_string(), kind)).is_err() {
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
	watched: mpsc::UnboundedReceiver<(String, Kind)>,
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
			assert_eq!(kinds[0], Kind::Start, "{prefix}: {kinds:?}");
			assert_ne!(kinds.last(), Some(&Kind::End), "{prefix}: {kinds:?}");
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
#[moq_net_sim::test]
async fn full_mesh_withdraw_retracts_once_lite04() {
	full_mesh_withdraw_retracts_once("moq-lite-04").await;
}

#[moq_net_sim::test]
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
		assert_eq!(kinds, [Kind::End], "{prefix}");
	}
	// The same relays publish again, clearing their own withdrawals.
	let _broadcasts = mesh.publish(100, 0).await;
}

/// A relay two hops from the publisher's hears only that its neighbor withdrew, not
/// why, so it can still pass through a stale path or two. Every broadcast must still
/// end retracted, and republishing from other relays must reach the watcher again:
/// no withdrawal outlives the peer announcing the path again.
#[moq_net_sim::test]
async fn partial_mesh_withdraw_then_republish() {
	let mut mesh = Mesh::new("moq-lite-06", 12, &ring_with_chords(12)).await;
	let broadcasts = mesh.publish(100, 0).await;
	drop(broadcasts);
	let updates = drain(&mut mesh.watched).await;
	assert_eq!(updates.len(), 100);
	for (prefix, kinds) in updates {
		assert_eq!(kinds.last(), Some(&Kind::End), "{prefix}: {kinds:?}");
	}
	let _broadcasts = mesh.publish(100, 5).await;
}

/// A 34-relay partial mesh: moq.pro's production graph on 2026-09-30, where
/// relays in neighboring PoPs peer and most reach a publisher's relay through
/// others.
#[rustfmt::skip]
const PRODUCTION: &[(usize, usize)] = &[
	(0, 20), (0, 22), (0, 29), (1, 3), (1, 12), (1, 14), (1, 21), (2, 20), (2, 26), (2, 32), (3, 8),
	(3, 11), (3, 15), (3, 16), (3, 17), (3, 18), (3, 21), (3, 22), (3, 23), (3, 24), (3, 29), (3, 30),
	(3, 31), (4, 7), (4, 20), (4, 22), (4, 25), (4, 29), (5, 20), (5, 27), (5, 33), (6, 23), (6, 24),
	(6, 30), (6, 31), (7, 22), (7, 25), (7, 29), (8, 15), (8, 17), (8, 21), (8, 23), (8, 30), (9, 19),
	(9, 22), (9, 23), (9, 28), (9, 29), (9, 30), (10, 19), (10, 20), (10, 22), (10, 23), (10, 26),
	(10, 27), (10, 28), (10, 29), (10, 30), (10, 32), (10, 33), (11, 15), (11, 16), (11, 21),
	(12, 14), (12, 27), (12, 33), (13, 14), (13, 27), (13, 33), (14, 26), (14, 27), (14, 32),
	(14, 33), (15, 21), (16, 18), (16, 21), (17, 21), (17, 24), (17, 31), (18, 21), (18, 22),
	(18, 23), (18, 24), (18, 29), (18, 30), (18, 31), (19, 22), (19, 23), (19, 26), (19, 28),
	(19, 29), (19, 30), (19, 32), (20, 22), (20, 26), (20, 27), (20, 29), (20, 32), (20, 33),
	(21, 22), (21, 23), (21, 24), (21, 29), (21, 30), (21, 31), (22, 23), (22, 24), (22, 25),
	(22, 26), (22, 27), (22, 28), (22, 29), (22, 30), (22, 31), (22, 32), (22, 33), (23, 24),
	(23, 28), (23, 29), (23, 30), (23, 31), (24, 29), (24, 30), (24, 31), (25, 29), (26, 27),
	(26, 28), (26, 29), (26, 32), (26, 33), (27, 29), (27, 32), (27, 33), (28, 29), (28, 30),
	(28, 32), (29, 30), (29, 31), (29, 32), (29, 33), (30, 31), (32, 33),
];

/// One-way latency in ms for each [`PRODUCTION`] link, from the great-circle
/// distance between its PoPs at 200 km/ms over a path 1.5 times as long, plus 1 ms.
#[rustfmt::skip]
const PRODUCTION_LATENCY_MS: &[u64] = &[
	10, 7, 7, 50, 30, 51, 50, 9, 13, 13, 5, 4, 6, 8, 3, 4, 1, 50, 6, 4, 50, 6, 4, 50, 63, 58, 50, 58,
	16, 5, 5, 10, 10, 10, 10, 12, 1, 12, 6, 3, 5, 8, 8, 5, 4, 43, 5, 4, 43, 6, 11, 8, 49, 22, 23, 6,
	8, 49, 22, 23, 3, 5, 4, 41, 103, 103, 60, 91, 91, 59, 64, 59, 64, 6, 9, 8, 3, 6, 6, 4, 48, 4, 3,
	48, 4, 3, 5, 44, 26, 1, 5, 44, 26, 15, 21, 18, 15, 21, 18, 50, 6, 4, 50, 6, 4, 45, 47, 12, 29, 30,
	5, 1, 45, 47, 29, 30, 2, 44, 45, 1, 2, 47, 2, 1, 12, 9, 26, 29, 1, 9, 30, 9, 1, 5, 44, 26, 45, 47,
	29, 30, 2, 9,
];

/// One withdrawal on a partial mesh retracts everywhere once. A relay two hops
/// from the publisher's hears only that its neighbor withdrew, and must not
/// fall back to, and re-advertise, another path derived from the same
/// announcement. Holding route updates (`origin::Config::update_hold`) lets the
/// withdrawal remove those paths first; with link latency and no hold, each
/// withdrawal here costs tens of thousands of announcements.
#[moq_net_sim::test]
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
				updates.values().all(|kinds| kinds.last() != Some(&Kind::End)),
				"relay {relay}: {updates:?}"
			);
		}

		drop(broadcast);
		let mut hunted = Vec::new();
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = drain(watched).await;
			let kinds = &updates[&format!("room/{publisher}")];
			if kinds != &[Kind::End] {
				hunted.push((relay, kinds.clone()));
			}
		}
		assert!(hunted.is_empty(), "publisher {publisher}: {hunted:?}");
	}
}

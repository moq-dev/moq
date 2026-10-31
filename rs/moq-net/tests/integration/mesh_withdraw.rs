#![cfg(feature = "async")]

//! Broadcasts withdrawn from a meshed cluster retract everywhere, once.
//!
//! Every relay holds a route through each peer that re-advertised a broadcast.
//! When the publisher's relay withdraws it, those routes all derive from the one
//! withdrawn; a relay that selects them in turn re-advertises each stale path, and
//! the cluster walks all of them before it converges (path hunting).

use crate::support;

use std::{collections::HashMap, time::Duration};

use futures::channel::mpsc;

use moq_net::{Hop, Version, announce, cache, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer, peer_with_latency};

fn produce_origin(hop: u64) -> origin::Producer {
	produce(hop, origin::Config::default().update_hold)
}

/// An origin holding route updates for `hold`. Its cache never expires idle
/// groups: nothing here carries media, and the expiry sweep would keep a timer
/// armed forever, so [`moq_clock::sim::idle`] could never see the mesh settle.
fn produce(hop: u64, hold: Duration) -> origin::Producer {
	let mut config = origin::Config::new(Hop::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	config.update_hold = hold;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// The kind of an [`announce::Event`] for a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Start,
	Update,
	Restart,
	Takeover,
	End,
}

/// Every update per prefix the watcher has seen. Await [`moq_clock::sim::idle`]
/// first, so every link, hold and quarantine in the mesh has settled.
fn drain(watched: &mut mpsc::UnboundedReceiver<(String, Kind)>) -> HashMap<String, Vec<Kind>> {
	let mut updates = HashMap::<String, Vec<Kind>>::new();
	while let Ok((prefix, kind)) = watched.try_recv() {
		updates.entry(prefix).or_default().push(kind);
	}
	updates
}

/// Watch `announced` from its own task, the way a session's announce writer does:
/// it runs when woken, between the relays' own tasks, rather than only once the
/// test task is polled again, which would coalesce every intermediate update.
fn watch(mut announced: announce::Consumer) -> mpsc::UnboundedReceiver<(String, Kind)> {
	let (tx, rx) = mpsc::unbounded();
	moq_clock::sim::spawn(async move {
		while let Some(event) = announced.next().await {
			let (kind, announce) = match event {
				announce::Event::Start(announce) => (Kind::Start, announce),
				announce::Event::Update(announce) => (Kind::Update, announce),
				announce::Event::Restart(announce) => (Kind::Restart, announce),
				announce::Event::Takeover { announce, .. } => (Kind::Takeover, announce),
				announce::Event::End(announce) => (Kind::End, announce),
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
	async fn publish(&mut self, count: usize, offset: usize) -> Vec<moq_net::origin::Publication> {
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
		moq_clock::sim::idle().await;
		let updates = drain(&mut self.watched);
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
#[moq_clock::test]
async fn full_mesh_withdraw_retracts_once_lite04() {
	full_mesh_withdraw_retracts_once("moq-lite-04").await;
}

#[moq_clock::test]
async fn full_mesh_withdraw_retracts_once_lite06() {
	full_mesh_withdraw_retracts_once("moq-lite-06").await;
}

async fn full_mesh_withdraw_retracts_once(version: &str) {
	let mut mesh = Mesh::new(version, 8, &full_mesh(8)).await;
	let broadcasts = mesh.publish(100, 0).await;
	drop(broadcasts);
	moq_clock::sim::idle().await;
	let updates = drain(&mut mesh.watched);
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
#[moq_clock::test]
async fn partial_mesh_withdraw_then_republish() {
	let mut mesh = Mesh::new("moq-lite-06", 12, &ring_with_chords(12)).await;
	let broadcasts = mesh.publish(100, 0).await;
	drop(broadcasts);
	moq_clock::sim::idle().await;
	let updates = drain(&mut mesh.watched);
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
#[moq_clock::test]
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
		moq_clock::sim::idle().await;
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = drain(watched);
			assert!(
				updates.values().all(|kinds| kinds.last() != Some(&Kind::End)),
				"relay {relay}: {updates:?}"
			);
		}

		drop(broadcast);
		moq_clock::sim::idle().await;
		let mut hunted = Vec::new();
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = drain(watched);
			let kinds = &updates[&format!("room/{publisher}")];
			if kinds != &[Kind::End] {
				hunted.push((relay, kinds.clone()));
			}
		}
		assert!(hunted.is_empty(), "publisher {publisher}: {hunted:?}");
	}
}

/// A link between two relays, by index, and its one-way latency.
type Link = ((usize, usize), Duration);

const fn ms(millis: u64) -> Duration {
	Duration::from_millis(millis)
}

/// Eight relays where, without the retraction quarantine, relays 1 and 5
/// retract a broadcast published on relay 7 and then announce it again.
#[rustfmt::skip]
const STALE_PATHS: &[Link] = &[
	((0, 1), ms(35)), ((1, 2), ms(90)), ((0, 3), ms(129)), ((2, 4), ms(140)), ((1, 5), ms(137)), ((4, 6), ms(148)),
	((2, 7), ms(30)), ((3, 5), ms(124)), ((1, 4), ms(111)), ((0, 5), ms(21)), ((3, 4), ms(34)),
];

/// A relay that already retracted a withdrawn broadcast must not announce it
/// again. Its neighbors, two or more hops from the publisher's relay, still hunt
/// through stale paths; one that falls back to a path not through this relay
/// offers it as a new route after the relay forgot the withdrawal.
#[moq_clock::test]
async fn retracted_relay_ignores_a_stale_path() {
	let version: Version = "moq-lite-06".parse().unwrap();
	let nodes: Vec<_> = (1..=8).map(produce_origin).collect();
	let mut pairs = Vec::new();
	for &((a, b), latency) in STALE_PATHS {
		pairs.push(peer_with_latency(version, &nodes[a], &nodes[b], latency).await);
	}
	let mut watched: Vec<_> = nodes.iter().map(|node| watch(node.consume().announced())).collect();

	let broadcast = nodes[7].create_broadcast("room/7").unwrap();
	broadcast.announce(Default::default()).unwrap();
	moq_clock::sim::idle().await;
	for watched in watched.iter_mut() {
		drain(watched);
	}

	drop(broadcast);
	moq_clock::sim::idle().await;
	let mut revived = Vec::new();
	for (relay, watched) in watched.iter_mut().enumerate() {
		let kinds = drain(watched).remove("room/7").unwrap();
		assert_eq!(kinds.last(), Some(&Kind::End), "relay {relay}: {kinds:?}");
		if kinds.windows(2).any(|w| w == [Kind::End, Kind::Start]) {
			revived.push((relay, kinds));
		}
	}
	assert!(revived.is_empty(), "{revived:?}");
}

/// Relays peered over `edges`, each
/// holding route updates for `hold`, with a client publishing on relay
/// `publisher` and a client watching on relay `watcher`. Every relay is watched
/// too.
///
/// The watching client holds nothing, so it reports every toggle its relay
/// sends, as a browser's announce stream does. The broadcast has no epoch, so a
/// relay moving to another path restarts it, which lite-06 sends as a
/// retraction then an announcement.
struct Cluster {
	_pairs: Vec<MockPair>,
	publisher: origin::Producer,
	client: mpsc::UnboundedReceiver<(String, Kind)>,
	relays: Vec<mpsc::UnboundedReceiver<(String, Kind)>>,
}

impl Cluster {
	async fn new(version: &str, edges: &[Link], mut config: origin::Config, publisher: usize, watcher: usize) -> Self {
		let version: Version = version.parse().unwrap();
		let n = edges.iter().map(|&((a, b), _)| a.max(b) + 1).max().unwrap();
		config.pool = cache::Pool::unbounded();
		let relays: Vec<_> = (1..=n as u64)
			.map(|id| {
				let mut config = config.clone();
				config.hop = Hop::new(id).unwrap();
				let (origin, driver) = origin::Producer::new(config);
				support::harness::spawn(driver);
				origin
			})
			.collect();
		let mut pairs = Vec::new();
		for &((a, b), latency) in edges {
			pairs.push(peer_with_latency(version, &relays[a], &relays[b], latency).await);
		}

		let publishing = produce(1000, Duration::ZERO);
		let mut options = MockConnectOptions::new(version);
		options.client_publish = Some(publishing.consume());
		options.server_subscribe = Some(relays[publisher].clone());
		pairs.push(connect_mock(options).await);

		let watching = produce(1001, Duration::ZERO);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(relays[watcher].consume());
		options.client_subscribe = Some(watching.clone());
		pairs.push(connect_mock(options).await);

		let mut cluster = Self {
			_pairs: pairs,
			publisher: publishing,
			client: watch(watching.consume().announced()),
			relays: relays.iter().map(|relay| watch(relay.consume().announced())).collect(),
		};
		// Let every session's announce interest land before anything is published.
		moq_clock::sim::idle().await;
		assert!(drain(&mut cluster.client).is_empty());
		cluster
	}

	/// Publish `path` from the client and require everyone to see it. Returns
	/// what the watching client saw.
	async fn publish(&mut self, path: &str) -> (moq_net::origin::Publication, Vec<Kind>) {
		let broadcast = self.publisher.create_broadcast(path).unwrap();
		broadcast.announce(Default::default()).unwrap();
		moq_clock::sim::idle().await;
		let client = drain(&mut self.client).remove(path).unwrap();
		assert_ne!(client.last(), Some(&Kind::End), "client: {client:?}");
		for (relay, watched) in self.relays.iter_mut().enumerate() {
			let kinds = drain(watched).remove(path).unwrap();
			assert_ne!(kinds.last(), Some(&Kind::End), "relay {relay}: {kinds:?}");
		}
		(broadcast, client)
	}

	/// Close `broadcast` cleanly: what the watching client, then each relay, saw.
	async fn close(&mut self, path: &str, broadcast: moq_net::origin::Publication) -> (Vec<Kind>, Vec<Vec<Kind>>) {
		drop(broadcast);
		moq_clock::sim::idle().await;
		let client = drain(&mut self.client).remove(path).unwrap();
		let mut relays = Vec::new();
		for watched in self.relays.iter_mut() {
			relays.push(drain(watched).remove(path).unwrap());
		}
		(client, relays)
	}
}

/// #4349: a publisher on one relay cleanly closes a broadcast, and a client
/// watching from another relay sees one retraction and no re-announce. On the
/// release before the retraction quarantine (#5376), this client saw it
/// retracted, announced, and retracted again. Relay 0 still restarts onto a
/// stale path after the hold; see [`residual_slow_ring_reannounces`].
#[moq_clock::test]
async fn cross_node_close_retracts_once_lite06() {
	cross_node_close_retracts_once("moq-lite-06").await;
}

#[moq_clock::test]
async fn cross_node_close_retracts_once_lite07() {
	cross_node_close_retracts_once("moq-lite-07-wip").await;
}

async fn cross_node_close_retracts_once(version: &str) {
	let mut cluster = Cluster::new(version, STALE_PATHS, origin::Config::default(), 7, 1).await;
	for round in 0..3 {
		let (broadcast, client) = cluster.publish("room/a").await;
		assert_eq!(client, [Kind::Start], "round {round}");
		let (client, relays) = cluster.close("room/a", broadcast).await;
		assert_eq!(client, [Kind::End], "round {round}");
		for (relay, kinds) in relays.iter().enumerate() {
			assert_eq!(
				kinds.last(),
				Some(&Kind::End),
				"round {round}, relay {relay}: {kinds:?}"
			);
			assert!(
				!kinds.windows(2).any(|w| w == [Kind::End, Kind::Start]),
				"round {round}, relay {relay}: {kinds:?}"
			);
		}
	}
}

/// The positive control for [`cross_node_close_retracts_once`]: without the
/// update hold and the quarantine, the client sees the broadcast come back
/// before it ends.
#[moq_clock::test]
async fn cross_node_close_without_hold_reannounces() {
	let mut cluster = Cluster::new(
		"moq-lite-06",
		STALE_PATHS,
		{
			let mut config = origin::Config::default();
			config.update_hold = Duration::ZERO;
			config
		},
		7,
		1,
	)
	.await;
	let (broadcast, _) = cluster.publish("room/a").await;
	let (client, _) = cluster.close("room/a", broadcast).await;
	assert_eq!(client, [Kind::End, Kind::Start, Kind::End]);
}

// Residual counterexamples on `main`, with the hold and the retraction
// quarantine. Each pins what the cluster does today, so a change either way
// fails here. Routes and announces (quest/m1/cluster-routing/routes.md) must
// bring each to a single retraction everywhere.

/// A ring of six relays with links under 150 ms. Relay 3 hears the withdrawal
/// from relay 2 at 84 ms, but relay 5's equal-cost copy, the long way round,
/// stays live until 446 ms. That outlasts the hold, so relay 3 restarts onto
/// the stale copy and its client sees the broadcast come back.
#[moq_clock::test]
async fn residual_slow_ring_reannounces() {
	#[rustfmt::skip]
	const EDGES: &[Link] = &[
		((0, 1), ms(141)), ((0, 2), ms(55)), ((2, 3), ms(29)), ((1, 4), ms(135)), ((4, 5), ms(148)), ((3, 5), ms(22)),
	];
	use Kind::*;
	let mut cluster = Cluster::new("moq-lite-06", EDGES, origin::Config::default(), 0, 3).await;
	let (broadcast, _) = cluster.publish("room/a").await;
	let (client, relays) = cluster.close("room/a", broadcast).await;
	assert_eq!(client, [End, Start, End]);
	let relays: Vec<&[Kind]> = relays.iter().map(Vec::as_slice).collect();
	assert_eq!(relays, [&[End][..], &[End], &[End], &[Restart, End], &[End], &[End]]);
	assert_capped_withdrawal(EDGES, 0, 3).await;
}

/// Nine relays, reduced from a seeded random mesh with [`random_mesh`]'s
/// links. Relays 2 and 8 retract the broadcast and then announce it again: a
/// revival the quarantine misses.
#[moq_clock::test]
async fn residual_mesh_revives() {
	#[rustfmt::skip]
	const EDGES: &[Link] = &[
		((3, 4), ms(10)), ((3, 5), ms(12)), ((1, 6), ms(149)), ((0, 7), ms(73)), ((6, 8), ms(20)), ((1, 4), ms(26)),
		((0, 6), ms(121)), ((3, 7), ms(81)), ((0, 2), ms(96)), ((2, 8), ms(93)), ((5, 8), ms(89)),
	];
	use Kind::*;
	let mut cluster = Cluster::new("moq-lite-06", EDGES, origin::Config::default(), 4, 8).await;
	let (broadcast, _) = cluster.publish("room/a").await;
	let (client, relays) = cluster.close("room/a", broadcast).await;
	assert_eq!(client, [End, Start, End, Start, End]);
	let relays: Vec<&[Kind]> = relays.iter().map(Vec::as_slice).collect();
	assert_eq!(
		relays,
		[
			&[Restart, End][..],
			&[End],
			&[End, Start, End],
			&[End],
			&[End],
			&[Restart, End],
			&[End],
			&[Restart, End],
			&[Restart, End, Start, End],
		]
	);
	assert_capped_withdrawal(EDGES, 4, 8).await;
}

async fn assert_capped_withdrawal(edges: &[Link], publisher: usize, watcher: usize) {
	let mut config = origin::Config::default();
	config.max_hops = origin::HopLimit::new(diameter(edges) + 2).unwrap();
	let mut cluster = Cluster::new("moq-lite-06", edges, config, publisher, watcher).await;
	let (broadcast, _) = cluster.publish("room/a").await;
	let (client, relays) = cluster.close("room/a", broadcast).await;
	assert_eq!(client.last(), Some(&Kind::End));
	for kinds in relays {
		assert_eq!(kinds.last(), Some(&Kind::End));
	}
}

/// splitmix64, so every platform builds the same meshes.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^ (z >> 31)
	}

	/// A value in `lo..=hi`.
	fn range(&mut self, lo: u64, hi: u64) -> u64 {
		lo + self.next() % (hi - lo + 1)
	}
}

/// A connected mesh of 8 to 17 relays (a random spanning tree plus up to as
/// many extra links), each link 1 to 150 ms, and the relay publishing on it.
fn random_mesh(seed: u64) -> (Vec<Link>, usize) {
	let mut rng = Rng(seed);
	let n = rng.range(8, 17) as usize;
	let mut edges = Vec::new();
	let mut linked = std::collections::HashSet::new();
	for b in 1..n {
		let a = rng.range(0, b as u64 - 1) as usize;
		linked.insert((a, b));
		edges.push(((a, b), ms(rng.range(1, 150))));
	}
	for _ in 0..rng.range(0, n as u64) {
		let a = rng.range(0, n as u64 - 1) as usize;
		let b = rng.range(0, n as u64 - 1) as usize;
		let (a, b) = (a.min(b), a.max(b));
		if a != b && linked.insert((a, b)) {
			edges.push(((a, b), ms(rng.range(1, 150))));
		}
	}
	(edges, rng.range(0, n as u64 - 1) as usize)
}

/// One clean close on each of 200 seeded random meshes: how many relays
/// announce the broadcast again before their last retraction (by restarting
/// onto a stale path, or by reviving it after retracting), how many of those
/// revive, and how many updates they send beyond the one retraction. Pinned at
/// today's counts; Routes and announces brings all three to zero.
#[moq_clock::test]
async fn residual_random_meshes() {
	assert_eq!(withdrawal_counts(false).await, (179, 3, 192));
}

/// The same topologies and terminal states with room for the diameter and two
/// extra hops, including the publishing client outside the relay graph.
#[moq_clock::test]
async fn capped_random_meshes_hunt_less() {
	let capped = withdrawal_counts(true).await;
	assert_eq!(capped, (151, 1, 154));
}

fn diameter(edges: &[Link]) -> usize {
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

async fn withdrawal_counts(capped: bool) -> (usize, usize, usize) {
	let (mut hunted, mut revived, mut extra) = (0, 0, 0);
	for seed in 0..200 {
		let (edges, publisher) = random_mesh(seed);
		let mut config = origin::Config::default();
		if capped {
			config.max_hops = origin::HopLimit::new(diameter(&edges) + 2).unwrap();
		}
		let mut cluster = Cluster::new("moq-lite-06", &edges, config, publisher, 0).await;
		let (broadcast, _) = cluster.publish("room/a").await;
		let (_, relays) = cluster.close("room/a", broadcast).await;
		for kinds in relays {
			assert_eq!(kinds.last(), Some(&Kind::End), "seed {seed}: {kinds:?}");
			hunted += usize::from(kinds.len() > 1);
			revived += usize::from(kinds.windows(2).any(|w| w == [Kind::End, Kind::Start]));
			extra += kinds.len() - 1;
		}
	}
	(hunted, revived, extra)
}

/// A permissive source sends an over-limit path to prove ingress refusal; an
/// accepted boundary path then proves egress counts the relay's own stamp.
#[moq_clock::test]
async fn hop_limit_bounds_receive_and_send() {
	for name in [
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		let version = name.parse().unwrap();
		let source = produce_origin(1);
		let mut config = origin::Config::new(Hop::new(2).unwrap());
		config.max_hops = origin::HopLimit::new(2).unwrap();
		config.pool = cache::Pool::unbounded();
		let (relay, driver) = origin::Producer::new(config);
		support::harness::spawn(driver);
		let sink = produce_origin(3);
		let _upstream = peer(version, &source, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut received = watch(relay.consume().announced());
		let mut sent = watch(sink.consume().announced());
		moq_clock::sim::idle().await;
		let route = |count: u64| {
			origin::Route::default().with_hops(
				moq_net::Hops::try_from((100..100 + count).map(|id| Hop::new(id).unwrap()).collect::<Vec<_>>())
					.unwrap(),
			)
		};
		let _long = source.publish("long", route(2)).unwrap();
		let _boundary = source.publish("boundary", route(1)).unwrap();
		let short = source.publish("short", route(0)).unwrap();
		moq_clock::sim::idle().await;
		let received = drain(&mut received);
		assert!(!received.contains_key("long"), "{name}: accepted over-limit path");
		assert!(received.contains_key("boundary"), "{name}: refused boundary path");
		assert!(received.contains_key("short"), "{name}: refused short path");
		let sent_initial = drain(&mut sent);
		assert_eq!(sent_initial.len(), 1, "{name}: {sent_initial:?}");
		assert!(sent_initial.contains_key("short"), "{name}: forwarded over-limit path");

		if matches!(name, "moq-lite-03" | "moq-lite-04") {
			// These wires have no route metadata update: withdraw, then replace it.
			drop(short);
			moq_clock::sim::idle().await;
			assert_eq!(
				drain(&mut sent).get("short").and_then(|kinds| kinds.last()),
				Some(&Kind::End),
				"{name}"
			);
			let _replacement = source.publish("short", route(1)).unwrap();
			moq_clock::sim::idle().await;
			assert!(drain(&mut sent).is_empty(), "{name}: forwarded over-limit replacement");
			continue;
		}
		short.announce(route(1)).unwrap();
		moq_clock::sim::idle().await;
		let ended = drain(&mut sent);
		assert_eq!(
			ended.get("short").and_then(|kinds| kinds.last()),
			Some(&Kind::End),
			"{name}: {ended:?}"
		);
	}
}

/// A cheaper route at the cap wins locally but cannot be forwarded, so the relay
/// advertises the pricier in-cap route instead of withdrawing the prefix, and
/// withdraws only once no in-cap route is left.
#[moq_clock::test]
async fn hop_limit_falls_back_to_a_forwardable_route() {
	// Only wires carrying a route cost can rank a longer route first.
	for name in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		let version = name.parse().unwrap();
		let near = produce_origin(1);
		let far = produce_origin(4);
		let mut config = origin::Config::new(Hop::new(2).unwrap());
		config.max_hops = origin::HopLimit::new(3).unwrap();
		config.pool = cache::Pool::unbounded();
		let (relay, driver) = origin::Producer::new(config);
		support::harness::spawn(driver);
		let sink = produce_origin(3);
		let _near = peer(version, &near, &relay).await;
		let _far = peer(version, &far, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut sent = watch(sink.consume().announced());
		moq_clock::sim::idle().await;

		let route = |first: u64, count: u64, cost: u64| {
			origin::Route::default()
				.with_hops(
					moq_net::Hops::try_from(
						(first..first + count)
							.map(|id| Hop::new(id).unwrap())
							.collect::<Vec<_>>(),
					)
					.unwrap(),
				)
				.with_cost(origin::Cost::new(cost))
		};
		// Three hops at the relay once the sender is appended: in the cap, but full.
		let _cheap = far.publish("room", route(100, 2, 1)).unwrap();
		// Two hops at the relay, so it has room for the relay's own.
		let pricey = near.publish("room", route(200, 1, 50)).unwrap();
		moq_clock::sim::idle().await;
		let started = drain(&mut sent);
		assert_eq!(
			started.get("room").and_then(|kinds| kinds.last()),
			Some(&Kind::Start),
			"{name}: withdrew a prefix an in-cap route reaches: {started:?}"
		);

		drop(pricey);
		moq_clock::sim::idle().await;
		let ended = drain(&mut sent);
		assert_eq!(
			ended.get("room").and_then(|kinds| kinds.last()),
			Some(&Kind::End),
			"{name}: forwarded a route at the cap: {ended:?}"
		);
	}
}

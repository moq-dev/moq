//! Broadcasts withdrawn from a meshed cluster retract everywhere, once.
//!
//! Every relay holds a route through each peer that re-advertised a broadcast.
//! When the publisher's relay withdraws it, those routes all derive from the one
//! withdrawn; a relay that selects them in turn re-advertises each stale path, and
//! the cluster walks all of them before it converges (path hunting).

mod support;

use std::{collections::HashMap, time::Duration};

use tokio::sync::mpsc;

use moq_net::{Hop, Version, announce, broadcast, origin};
use support::harness::{MockPair, peer};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
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

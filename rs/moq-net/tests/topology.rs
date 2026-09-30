//! Cluster sessions flood per-link liveness, so every relay learns the relay
//! graph once and computes its shortest path to every other relay.
//!
//! Time is paused: each wait advances past the hold-down only once every task
//! is idle, so the graph each relay settles on is deterministic.

mod support;

use std::time::Duration;

use moq_net::{Client, Hop, Server, Session, Version, origin, topology};
use support::{
	harness::{now, run},
	mock::create_mock_session_pair,
};

fn version(name: &str) -> Version {
	name.parse().expect("known version")
}

/// One relay: its origin, named by the same id its topology reports under.
#[derive(Clone)]
struct Relay {
	origin: origin::Producer,
	topology: topology::Database,
}

impl Relay {
	fn new(id: u64, incarnation: u64) -> Self {
		let hop = Hop::new(id).unwrap();
		let (origin, driver) = origin::Producer::new(origin::Config::new(hop));
		tokio::spawn(run(driver));
		Self {
			origin,
			topology: topology::Database::new(hop, incarnation),
		}
	}

	/// This relay's distance to `node` as (cost, hops, next hop).
	fn route(&self, node: u64) -> Option<(u64, u64, u64)> {
		let graph = self.topology.graph();
		let node = graph.get(Hop::new(node).unwrap())?;
		let distance = node.distance?;
		Some((distance.cost, distance.hops, node.next?.id()))
	}
}

/// Dial `server` from `client` the way the relay cluster does.
async fn link(client: &Relay, server: &Relay, cost: u64) -> (Session, Session) {
	let (client_transport, server_transport) = create_mock_session_pair(Some(version("moq-lite-07-wip").alpn()));
	let dial = Client::new()
		.with_versions(version("moq-lite-07-wip").into())
		.with_origin(client.origin.clone().peer())
		.with_cost(cost)
		.with_topology(client.topology.clone());
	let accept = Server::new()
		.with_versions(version("moq-lite-07-wip").into())
		.with_origin(server.origin.clone().peer())
		.with_topology(server.topology.clone());
	let dialed = async {
		let (session, driver) = dial.connect(now(), client_transport).await.expect("dial");
		tokio::spawn(run(driver));
		session
	};
	let accepted = async {
		let (session, driver) = accept.accept(now(), server_transport).await.expect("accept");
		tokio::spawn(run(driver));
		session
	};
	tokio::join!(dialed, accepted)
}

/// Let every hold-down expire and every message land.
async fn settle() {
	tokio::time::sleep(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn a_line_learns_every_link() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let c = Relay::new(3, 0);
	let _ab = link(&a, &b, 2).await;
	let _bc = link(&b, &c, 3).await;
	settle().await;

	assert_eq!(a.route(2), Some((2, 1, 2)));
	assert_eq!(a.route(3), Some((5, 2, 2)));
	assert_eq!(c.route(1), Some((5, 2, 2)));
	assert_eq!(b.route(1), Some((2, 1, 1)));
	assert_eq!(b.route(3), Some((3, 1, 3)));
}

/// Reports that loop around a ring are dropped as stale, and the cheaper way
/// around wins over the shorter one.
#[tokio::test(start_paused = true)]
async fn a_ring_routes_by_cost() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let c = Relay::new(3, 0);
	let d = Relay::new(4, 0);
	let _ab = link(&a, &b, 1).await;
	let _bc = link(&b, &c, 1).await;
	let _cd = link(&c, &d, 1).await;
	let _da = link(&d, &a, 5).await;
	settle().await;

	assert_eq!(a.route(4), Some((3, 3, 2)), "three cheap links beat one dear one");
	assert_eq!(d.route(1), Some((3, 3, 3)));
	assert_eq!(a.route(3), Some((2, 2, 2)));
	for relay in [&a, &b, &c, &d] {
		let graph = relay.topology.graph();
		assert_eq!(graph.nodes.len(), 4);
		assert!(graph.nodes.iter().all(|node| node.incarnation == 0));
	}
}

/// A link that drops is withdrawn everywhere, and restored when it returns.
#[tokio::test(start_paused = true)]
async fn a_flapping_link_is_withdrawn_and_restored() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let c = Relay::new(3, 0);
	let _ab = link(&a, &b, 1).await;
	let bc = link(&b, &c, 1).await;
	settle().await;
	assert_eq!(a.route(3), Some((2, 2, 2)));

	bc.0.abort(moq_net::Error::Cancel);
	drop(bc);
	settle().await;
	assert_eq!(a.route(3), None, "c is unreachable once b-c drops");

	let _bc = link(&b, &c, 1).await;
	settle().await;
	assert_eq!(a.route(3), Some((2, 2, 2)));
	assert_eq!(c.route(1), Some((2, 2, 2)));
}

/// A relay that restarts under the same id reports a new incarnation, which
/// replaces everything its last run reported instead of looking older.
#[tokio::test(start_paused = true)]
async fn a_restarted_relay_supersedes_its_last_run() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let c = Relay::new(3, 100);
	let d = Relay::new(4, 0);
	let _ab = link(&a, &b, 1).await;
	let bc = link(&b, &c, 1).await;
	let cd = link(&c, &d, 1).await;
	settle().await;
	assert_eq!(a.route(4), Some((3, 3, 2)));

	// c dies. Its links drop without it saying so.
	bc.0.abort(moq_net::Error::Cancel);
	cd.0.abort(moq_net::Error::Cancel);
	drop((bc, cd));
	drop(c);
	settle().await;
	assert_eq!(a.route(3), None);

	// It comes back with only its link to b. Its last run's link to d must not
	// linger just because d still reports it... which d does not, having seen it drop.
	let c = Relay::new(3, 200);
	let _bc = link(&b, &c, 1).await;
	settle().await;
	let graph = a.topology.graph();
	let node = graph.get(Hop::new(3).unwrap()).expect("c is known");
	assert_eq!(node.incarnation, 200);
	assert_eq!(node.links.len(), 1, "only the new run's link to b");
	assert_eq!(a.route(3), Some((2, 2, 2)));
	assert_eq!(a.route(4), None);
}

/// A relay joining a settled cluster learns it all from its first digest.
#[tokio::test(start_paused = true)]
async fn a_late_joiner_learns_the_cluster_from_the_digest() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let c = Relay::new(3, 0);
	let _ab = link(&a, &b, 1).await;
	let _bc = link(&b, &c, 4).await;
	settle().await;

	let d = Relay::new(4, 0);
	let _da = link(&d, &a, 1).await;
	settle().await;
	assert_eq!(d.route(3), Some((6, 3, 1)));
	assert_eq!(c.route(4), Some((6, 3, 2)));
}

/// A session without a database on both ends is no link: the session still
/// works, and neither graph gains the peer.
#[tokio::test(start_paused = true)]
async fn one_sided_topology_is_not_a_link() {
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let (client_transport, server_transport) = create_mock_session_pair(Some(version("moq-lite-07-wip").alpn()));
	let dial = Client::new()
		.with_versions(version("moq-lite-07-wip").into())
		.with_origin(a.origin.clone().peer())
		.with_topology(a.topology.clone());
	let accept = Server::new()
		.with_versions(version("moq-lite-07-wip").into())
		.with_origin(b.origin.clone().peer());
	let dialed = async {
		let (session, driver) = dial.connect(now(), client_transport).await.expect("dial");
		tokio::spawn(run(driver));
		session
	};
	let accepted = async {
		let (session, driver) = accept.accept(now(), server_transport).await.expect("accept");
		tokio::spawn(run(driver));
		session
	};
	let (client, _server) = tokio::join!(dialed, accepted);
	settle().await;

	assert_eq!(a.topology.graph().nodes.len(), 1, "only a itself");
	assert!(
		tokio::time::timeout(Duration::from_secs(1), client.closed())
			.await
			.is_err(),
		"the session carries on"
	);
}

/// An older version carries no Topology stream, so the session runs without one.
#[tokio::test(start_paused = true)]
async fn lite_06_sessions_take_no_part() {
	let old = version("moq-lite-06");
	let a = Relay::new(1, 0);
	let b = Relay::new(2, 0);
	let (client_transport, server_transport) = create_mock_session_pair(Some(old.alpn()));
	let dial = Client::new()
		.with_versions(old.into())
		.with_origin(a.origin.clone().peer())
		.with_topology(a.topology.clone());
	let accept = Server::new()
		.with_versions(old.into())
		.with_origin(b.origin.clone().peer())
		.with_topology(b.topology.clone());
	let dialed = async {
		let (session, driver) = dial.connect(now(), client_transport).await.expect("dial");
		tokio::spawn(run(driver));
		session
	};
	let accepted = async {
		let (session, driver) = accept.accept(now(), server_transport).await.expect("accept");
		tokio::spawn(run(driver));
		session
	};
	let _sessions = tokio::join!(dialed, accepted);
	settle().await;

	assert_eq!(a.topology.graph().nodes.len(), 1);
	assert_eq!(b.topology.graph().nodes.len(), 1);
}

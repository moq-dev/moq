//! A draining relay's cluster peers stay on the session it drains instead of
//! redialing it, and reconnect once its replacement is up.
//!
//! Three relays on kio's simulator: B drains while A and C peer with it. Each peer
//! dials B through a [`Net`], which counts the sessions B accepts, and
//! [`kio::clock::sim::idle`] proves no further dial is on its way.

use std::time::Duration;

use moq_relay::{cluster, shutdown};

use crate::memory::{self, Net};

/// How long B drains before it force-closes what is left.
const DRAIN: Duration = Duration::from_secs(30);

/// The capped backoff a peer reconnects within: long enough that only a peer that
/// stopped redialing reaches it, and simulated time skips straight there.
const RECONNECT: Duration = Duration::from_secs(120);

/// Bound `fut` by `limit` of simulated time, panicking with `step` so a hang names the
/// stage that failed.
async fn within<T>(step: &str, limit: Duration, fut: impl std::future::Future<Output = T>) -> T {
	kio::clock::timeout(limit, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// Resolve once `origin` has an active route for `name`, as a peer's reconnect would
/// bring at the latest.
async fn learns(step: &str, origin: &moq_net::origin::Producer, name: &str) {
	within(
		step,
		RECONNECT,
		origin.consume().follow(name, None).unwrap().available(),
	)
	.await
	.expect("resolves");
}

/// A relay serving as `b`, publishing `b`, that drains over [`DRAIN`] once `trigger`
/// fires. `exited` resolves once its drain ends and the host is down, as
/// [`moq_relay::Relay::run`] returns and drops its listener.
struct Relay {
	cluster: cluster::Cluster,
	trigger: shutdown::Trigger,
	exited: kio::clock::sim::JoinHandle<()>,
	_route: moq_net::origin::Route,
}

impl Relay {
	fn up(net: &Net) -> Self {
		let cluster = memory::spawn_cluster(net.options(&[]));
		let route = cluster
			.origin
			.route("b", moq_net::origin::RouteOptions::default())
			.expect("create broadcast");
		let (trigger, observer) = shutdown::Observer::new(DRAIN);
		let _accepts = net.relay("b", &cluster, &memory::public(&["**"], &[], &[]), &observer);
		let net = net.clone();
		let exited = kio::clock::sim::spawn(async move {
			observer.drained().await;
			net.down("b");
		});
		Self {
			cluster,
			trigger,
			exited,
			_route: route,
		}
	}
}

/// A peer relay publishing `name` and dialing B.
async fn peer(net: &Net, name: &str) -> (cluster::Cluster, moq_net::origin::Route) {
	let cluster = memory::start(memory::spawn_cluster(net.options(&["b"])), Default::default()).await;
	let route = cluster
		.origin
		.route(name, moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	(cluster, route)
}

#[test]
fn draining_relay_keeps_its_peers_until_it_restarts() {
	memory::run(draining_relay_keeps_its_peers_until_it_restarts_inner);
}

async fn draining_relay_keeps_its_peers_until_it_restarts_inner() {
	let net = Net::new();
	let b = Relay::up(&net);
	let (a, _a_route) = peer(&net, "a").await;
	let (c, _c_route) = peer(&net, "c").await;

	// The mesh converges: each side learns the other's broadcasts, over one session each.
	learns("A learns b", &a.origin, "b").await;
	learns("C learns b", &c.origin, "b").await;
	learns("B learns a", &b.cluster.origin, "a").await;
	learns("B learns c", &b.cluster.origin, "c").await;
	kio::clock::sim::idle().await;
	assert_eq!(net.accepted("b"), 2);

	// B drains for its whole window: its peers hold their sessions to the end.
	b.trigger.start();
	within("B exits after its drain", DRAIN * 2, b.exited)
		.await
		.expect("relay task");
	assert_eq!(net.accepted("b"), 2, "a peer redialed the draining relay");

	// Nothing answers while B is down. Then its replacement comes up.
	kio::clock::sleep(Duration::from_secs(5)).await;
	let b2 = Relay::up(&net);

	// Each peer reconnects within the capped backoff, exactly once: once the mesh
	// settles, no redial is left pending.
	learns("B' learns a", &b2.cluster.origin, "a").await;
	learns("B' learns c", &b2.cluster.origin, "c").await;
	kio::clock::sim::idle().await;
	assert_eq!(net.accepted("b"), 4, "a peer dialed the replacement other than once");
}

//! Two relays sharing one `cluster.connect` list dial each other, so they hold two
//! sessions. Losing either one, the newer included, never black-holes a relay that
//! reaches them only through one of the two.
//!
//! Three lite-07 relays on kio's simulator: A and B list each other, C lists only A,
//! and B publishes. One direction's dials are then refused for good, so its session
//! stays closed while the other carries the routes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_net::origin::RouteKind;

use moq_relay::sim::Net;
use moq_relay::{cluster, shutdown};

use crate::memory;

/// Long enough that only a hang reaches it; simulated time skips straight there.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Well past the reader's debounce, after which a claim with no route shows offline.
const SETTLE: Duration = Duration::from_secs(5);

/// A lite-07 relay named `name`, dialing each of `peers` at its link cost.
async fn clustered(net: &Net, name: &str, peers: &[(&str, u64)]) -> cluster::Cluster {
	let mut connect = moq_tokio::connect::Config::default();
	connect.version = vec!["moq-lite-07-wip".parse().unwrap()];
	let mut options = net.options(name, &[]);
	options.config.connect = (peers.iter())
		.map(|(peer, cost)| cluster::Peer::new(format!("{}?cost={cost}", net.url(peer))))
		.collect();
	let cluster = memory::start(memory::spawn_cluster(options), connect).await;
	drop(net.relay(name, &cluster, &peers_auth(name), &shutdown::Observer::disabled()));
	cluster
}

/// Admits every session as a peer, as a deployment's auth grants its relays: only a
/// peer exchanges routes.
fn peers_auth(name: &str) -> moq_relay::auth::Auth {
	let (auth, mut admissions) = moq_relay::auth::Auth::embedded(name);
	drop(kio::clock::sim::spawn(async move {
		let mut leases = Vec::new();
		while let Some(admission) = admissions.next().await {
			let mut grant = moq_auth::Grant::new(moq_auth::Prefix::all(), moq_auth::Prefix::all());
			grant.link = Some(moq_auth::Link::Peer);
			let (lease, consumer) = moq_auth::lease::Producer::new(grant);
			admission.grant(consumer);
			leases.push(lease);
		}
	}));
	auth
}

/// Each way round, the cheaper of the two sessions carries A's route to B, and the
/// session closing is A's own dial or B's: one of the four closes the newer session at
/// A while its route crosses it.
#[test]
fn losing_either_of_two_sessions_keeps_the_route() {
	memory::run(|| losing_one_session_keeps_the_route(1, 5, ("a", "b")));
	memory::run(|| losing_one_session_keeps_the_route(1, 5, ("b", "a")));
	memory::run(|| losing_one_session_keeps_the_route(5, 1, ("a", "b")));
	memory::run(|| losing_one_session_keeps_the_route(5, 1, ("b", "a")));
}

/// A dials B at `a_cost` and B dials A at `b_cost`; then the dialer `cut.0` loses its
/// session to `cut.1` for good.
async fn losing_one_session_keeps_the_route(a_cost: u64, b_cost: u64, cut: (&'static str, &'static str)) {
	let net = Net::new();
	let a = clustered(&net, "a", &[("b", a_cost)]).await;
	let b = clustered(&net, "b", &[("a", b_cost)]).await;
	let c = clustered(&net, "c", &[("a", 1)]).await;
	let _route = b
		.origin
		.route("live", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let mut follow = c.origin.consume().follow("live", None).unwrap();
	kio::clock::timeout(TIMEOUT, follow.available())
		.await
		.expect("C learns B's broadcast")
		.expect("resolves");
	kio::clock::sleep(SETTLE).await;
	assert_eq!(
		net.accepted("a") + net.accepted("b"),
		3,
		"A and B dialed each other, C dialed A"
	);
	let shown = record(c.origin.consume().routes());

	let (from, to) = cut;
	net.dial_down(from, to);
	kio::clock::sleep(SETTLE).await;
	let lost: Vec<_> = (shown.lock().unwrap().iter())
		.filter(|(_, usable)| !usable)
		.cloned()
		.collect();
	assert!(lost.is_empty(), "{from}'s session to {to} closing lost {lost:?} at C");
	drop((a, b, c));
}

/// Each route change `routes` delivers, and whether it leaves the route usable: live and
/// available.
fn record(mut routes: moq_net::origin::Routes) -> Arc<Mutex<Vec<(String, bool)>>> {
	let shown = Arc::<Mutex<Vec<(String, bool)>>>::default();
	let recorded = shown.clone();
	drop(kio::clock::sim::spawn(async move {
		while let Some(event) = routes.next().await {
			let live = !matches!(event.kind, RouteKind::End | RouteKind::Offline);
			let usable = live && event.announce.route.available();
			let prefix = event.announce.prefix.as_str().to_string();
			recorded.lock().unwrap().push((prefix, usable));
		}
	}));
	shown
}

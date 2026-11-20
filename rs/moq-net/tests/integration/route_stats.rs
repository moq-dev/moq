#![cfg(feature = "async")]

//! A relay counts what its moq-lite-07 route layer does under each peer's label.
//!
//! The chain `D - C - R - A` carries `D`'s broadcasts to `A`. `R` counts its sessions
//! with `C` and `A` under their own labels. `D` drains and then leaves, so `C` tells `R`
//! with DRAIN and then DEAD, and `R` passes each on to `A`. Each changes `R`'s route to
//! `D`, which `R` charges to `C`'s label with every broadcast it rewrote.

use crate::{mesh_withdraw::produce_origin, support};

use std::time::Duration;

use moq_net::{Path, Version, goaway::Goaway, origin, stats};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const BROADCASTS: u64 = 3;

/// Peer `client` with `server` as moq-relay's cluster does, counting the server's side
/// under `stats`.
async fn peer(client: &origin::Producer, server: &origin::Producer, stats: stats::Session) -> MockPair {
	let client = client.clone().with_link(origin::Link::Peer);
	let server = server.clone().with_link(origin::Link::Peer);
	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.client_publish = Some(client.consume().with_hidden(true));
	options.client_subscribe = Some(client);
	options.server_publish = Some(server.consume().with_hidden(true));
	options.server_subscribe = Some(server);
	options.server_stats = stats;
	connect_mock(options).await
}

/// What `R` counted under `label`.
fn routes(registry: &stats::Registry, label: &str) -> stats::Routes {
	let mut report = stats::Report::default();
	registry.report(&mut report, moq_net::time::Instant::now());
	let entry = (report.sessions.iter())
		.find(|entry| entry.label.as_str() == label)
		.unwrap_or_else(|| panic!("no row for {label}"));
	entry.presence.routes
}

fn withdrawals(dead: u64, drain: u64) -> stats::Withdrawals {
	let mut withdrawals = stats::Withdrawals::default();
	withdrawals.dead = dead;
	withdrawals.drain = drain;
	withdrawals
}

async fn settle() {
	kio::clock::sleep(Duration::from_secs(1)).await;
}

#[kio::test]
async fn a_relay_counts_its_route_layer_per_peer_label() {
	let registry = stats::Registry::new(stats::Config::new());
	let tier = registry.tier(stats::Tier::default());
	let label = |name: &'static str| tier.session(stats::SessionInfo::new(Path::from_static(name)));
	let [d, c, r, a] = [1, 2, 3, 4].map(produce_origin);
	let d_c = peer(&d, &c, Default::default()).await;
	let _c_r = peer(&c, &r, label("c")).await;
	let _a_r = peer(&a, &r, label("a")).await;
	let _broadcasts: Vec<_> = (0..BROADCASTS)
		.map(|i| {
			let broadcast = d.route(format!("room/{i}"), origin::RouteOptions::hidden()).unwrap();
			broadcast.announce().unwrap();
			broadcast
		})
		.collect();
	settle().await;

	// `R` learned the route to `D` with its claims.
	let c_routes = routes(&registry, "c");
	assert_eq!((c_routes.changed, c_routes.rewritten), (1, BROADCASTS), "{c_routes:?}");
	// Nothing withdrew a route yet, and `A` caused no change.
	assert_eq!(c_routes.received, withdrawals(0, 0), "{c_routes:?}");
	assert_eq!(routes(&registry, "a"), stats::Routes::default());

	// `D` drains: `C` ranks its route to `D` draining and tells `R`, which tells `A`.
	d_c.client.goaway(Goaway::new()).unwrap();
	settle().await;
	let c_routes = routes(&registry, "c");
	assert_eq!(c_routes.received, withdrawals(0, 1), "{c_routes:?}");
	assert_eq!(
		(c_routes.changed, c_routes.rewritten),
		(2, 2 * BROADCASTS),
		"{c_routes:?}"
	);
	assert_eq!(routes(&registry, "a").sent, withdrawals(0, 1));

	// `D` leaves: `C` names the closed session to `R`, which passes it on.
	drop(d_c);
	settle().await;
	let c_routes = routes(&registry, "c");
	assert_eq!(c_routes.received, withdrawals(1, 1), "{c_routes:?}");
	// The route's end takes its claims with it: they are rewritten as claims ending, not
	// behind the route.
	assert_eq!(
		(c_routes.changed, c_routes.rewritten),
		(3, 2 * BROADCASTS),
		"{c_routes:?}"
	);
	let a_routes = routes(&registry, "a");
	assert_eq!(a_routes.sent, withdrawals(1, 1), "{a_routes:?}");
	assert_eq!(a_routes.received, withdrawals(0, 0), "{a_routes:?}");
	assert_eq!((a_routes.changed, a_routes.rewritten), (0, 0), "{a_routes:?}");
	// No route or session was refused anywhere.
	for routes in [c_routes, a_routes] {
		assert_eq!(routes.refused, Default::default());
		assert_eq!(routes.closed, Default::default());
	}
}

#![cfg(feature = "async")]

//! A server prices the link it accepted as a client prices the one it dialed: its price
//! wins over the one the client declares, and the client charges the one it declares.

use crate::support;

use std::time::Duration;

use moq_net::{Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The cost of the newest route to `live` that `origin` holds.
async fn cost(origin: &origin::Producer) -> u64 {
	kio::clock::sleep(Duration::from_secs(5)).await;
	let mut routes = origin.consume().routes();
	let mut cost = None;
	while let Some(event) = routes.try_next() {
		if event.announce.prefix.as_str() == "live" {
			cost = Some(event.announce.route.cost().value());
		}
	}
	cost.expect("a route to live")
}

/// Link a client and a server priced at `client` and `server`, the client publishing `live`
/// when `push`, and the server otherwise. Returns what must outlive the read, and the
/// receiving origin.
async fn link(
	version: &str,
	client: Option<u64>,
	server: Option<u64>,
	push: bool,
) -> ((MockPair, origin::Route), origin::Producer) {
	let version: Version = version.parse().unwrap();
	let (publisher, receiver) = (produce_origin(1), produce_origin(2));
	let route = publisher.route("live", origin::RouteOptions::default()).unwrap();
	let mut options = MockConnectOptions::new(version);
	options.cost = client;
	options.server_cost = server;
	if push {
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(receiver.clone());
	} else {
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(receiver.clone());
	}
	((connect_mock(options).await, route), receiver)
}

const VERSIONS: [&str; 3] = ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18"];

#[kio::test]
async fn a_server_price_wins_over_the_client_declared_one() {
	for version in VERSIONS {
		let (_pair, receiver) = link(version, Some(3), Some(7), true).await;
		assert_eq!(cost(&receiver).await, 7, "{version}: the server's own price");
		let (_pair, receiver) = link(version, Some(3), None, true).await;
		assert_eq!(cost(&receiver).await, 3, "{version}: the client's declared price");
	}
}

#[kio::test]
async fn a_client_charges_the_server_declared_price() {
	for version in VERSIONS {
		let (_pair, receiver) = link(version, None, Some(7), false).await;
		assert_eq!(cost(&receiver).await, 7, "{version}: the server's declared price");
		let (_pair, receiver) = link(version, None, None, false).await;
		assert_eq!(cost(&receiver).await, 1, "{version}: an unpriced link");
	}
}

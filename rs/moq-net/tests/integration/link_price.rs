#![cfg(feature = "async")]

//! A session's link repriced while it runs re-ranks the routes it already learned, end
//! to end over real sessions.
//!
//! Relays `A` and `B` each pull a publisher `P`'s broadcast and push it on to a
//! subscriber `S`, which accepts both sessions and prices them itself, as a relay prices
//! the link a grant names. `S` reads through whichever link is cheaper, and a new price
//! moves it without a reconnect.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Let every task settle, past the hold a lite-06 update waits out. Time is paused, so
/// this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_secs(5)).await;
}

/// `from` pushes everything it holds to `to`, which accepts the session.
async fn push(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(from.consume());
	options.server_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// What `S`'s route to `live` costs now.
fn cost(subscriber: &origin::Producer) -> u64 {
	let mut routes = subscriber.consume().routes();
	let mut cost = None;
	while let Some(event) = routes.try_next() {
		if let origin::RouteEvent {
			kind: origin::RouteKind::Start | origin::RouteKind::Update | origin::RouteKind::Restart,
			announce,
			..
		} = event && announce.prefix.as_str() == "live"
		{
			cost = Some(announce.route.cost().value());
		}
	}
	cost.expect("S has a route to live")
}

fn price(cost: u64) -> Option<origin::Cost> {
	Some(origin::Cost::from(cost))
}

async fn reprices(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (a, b, subscriber) = (produce_origin(2), produce_origin(3), produce_origin(4));
	let _broadcast = publisher.route("live", origin::RouteOptions::default()).unwrap();
	let _links = [push(version, &publisher, &a).await, push(version, &publisher, &b).await];
	let to_a = push(version, &a, &subscriber).await;
	let to_b = push(version, &b, &subscriber).await;
	to_a.server.price().set(price(10)).unwrap();
	to_b.server.price().set(price(100)).unwrap();
	settle().await;
	let through_a = cost(&subscriber);
	assert!(through_a < 100, "S reads through the cheaper A: {through_a}");

	to_a.server.price().set(price(1_000)).unwrap();
	assert_eq!(to_a.server.price().get(), price(1_000));
	settle().await;
	let through_b = cost(&subscriber);
	assert!(
		(100..1_000).contains(&through_b),
		"A repriced above B moves S to B: {through_b}"
	);

	// Unpriced, the link charges what `A` declared, which is nothing, so the default 1.
	to_a.server.price().set(None).unwrap();
	settle().await;
	assert_eq!(
		cost(&subscriber),
		through_a - 10 + 1,
		"S is back on A at the default price"
	);
}

#[kio::test]
async fn lite_06_reprices_learned_routes() {
	reprices("moq-lite-06").await;
}

#[kio::test]
async fn lite_07_reprices_learned_routes() {
	reprices("moq-lite-07-wip").await;
}

/// moq-transport charges each route as it arrives, so it takes its price at the start
/// and refuses a later one rather than leave its routes on the old price.
#[kio::test]
async fn moq_transport_refuses_a_reprice() {
	let version: Version = "moq-transport-22".parse().unwrap();
	let (relay, subscriber) = (produce_origin(1), produce_origin(2));
	let link = push(version, &relay, &subscriber).await;
	assert!(matches!(link.server.price().set(price(5)), Err(Error::Unsupported)));
	assert_eq!(link.server.price().get(), None, "the refused price is not kept");
}

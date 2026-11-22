#![cfg(feature = "async")]

//! A route a lite-07 relay learns over lite-06 keeps its link priority and its drain
//! when the relay announces it on, end to end over real sessions.
//!
//! A publisher `P` pushes one broadcast to relays `A` and `B` over lite-06. A subscriber
//! `S` dials both over lite-07, pricing `A` far cheaper, so `S` reads through `A` while
//! the two rank alike. `A` demoting its link to `P`, or `P` sending `A` a GOAWAY, ranks
//! `A`'s claim below `B`'s, which `S` sees only if `A` announces the claim as it ranks.

use crate::support;

use std::time::Duration;

use moq_net::{Version, goaway::Goaway, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// What `S` pays for its link to `B`, which costs every route through it at least this.
const B_COST: u64 = 100;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// Let every task settle, past the hold a lite-06 update waits out. Time is paused, so
/// this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_secs(5)).await;
}

/// `P` pushes everything it publishes to `relay` over lite-06, which `relay` ranks at
/// `priority`.
async fn push(publisher: &origin::Producer, relay: &origin::Producer, priority: i8) -> MockPair {
	let mut options = MockConnectOptions::new("moq-lite-06".parse().unwrap());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone().with_priority(origin::Priority::new(priority)));
	connect_mock(options).await
}

/// `S` dials `relay` over lite-07, pricing the link at `cost`.
async fn dial(relay: &origin::Producer, subscriber: &origin::Producer, cost: u64) -> MockPair {
	let lite: Version = "moq-lite-07-wip".parse().unwrap();
	let mut options = MockConnectOptions::new(lite);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(subscriber.clone());
	options.cost = Some(cost);
	connect_mock(options).await
}

/// Whether `S` reaches `live` through `B`, the only link priced at [`B_COST`].
async fn via_b(subscriber: &origin::Producer) -> bool {
	let mut routes = subscriber.consume().routes();
	let mut via = None;
	while let Some(event) = routes.try_next() {
		if let origin::RouteEvent {
			kind: origin::RouteKind::Start | origin::RouteKind::Update | origin::RouteKind::Restart,
			announce,
		} = event && announce.prefix.as_str() == "live"
		{
			via = Some(announce.route.cost().value() >= B_COST);
		}
	}
	via.expect("S has a route to live")
}

/// `P`'s push to `A`, ranked `a_priority` there, and everything else of the topology.
async fn topology(a_priority: i8) -> (Vec<MockPair>, MockPair, origin::Producer, origin::Route) {
	let publisher = produce_origin(1);
	let (a, b, subscriber) = (produce_origin(2), produce_origin(3), produce_origin(4));
	let broadcast = publisher.route("live", origin::RouteOptions::default()).unwrap();
	let to_a = push(&publisher, &a, a_priority).await;
	let rest = vec![
		push(&publisher, &b, 0).await,
		dial(&a, &subscriber, 1).await,
		dial(&b, &subscriber, B_COST).await,
	];
	settle().await;
	(rest, to_a, subscriber, broadcast)
}

#[kio::test]
async fn a_bridged_route_keeps_its_link_priority() {
	let (_rest, _to_a, subscriber, _broadcast) = topology(0).await;
	assert!(
		!via_b(&subscriber).await,
		"S reads through the cheaper A while both rank alike"
	);

	let (_rest, _to_a, subscriber, _broadcast) = topology(-5).await;
	assert!(
		via_b(&subscriber).await,
		"A's demoted link ranks its claim below B's, however cheap"
	);
}

#[kio::test]
async fn a_bridged_route_keeps_its_drain() {
	let (_rest, to_a, subscriber, _broadcast) = topology(0).await;
	assert!(
		!via_b(&subscriber).await,
		"S reads through the cheaper A while both rank alike"
	);

	to_a.client.goaway(Goaway::new()).unwrap();
	settle().await;
	assert!(via_b(&subscriber).await, "A's draining route ranks its claim below B's");
}

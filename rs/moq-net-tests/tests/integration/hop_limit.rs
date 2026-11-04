//! The relay's hop limit refuses a route whose chain is already too long and never
//! forwards one it has no room to stamp.
//!
//! Only a peer's route arrives with hops, so the routes under test land through the
//! hidden `fuzz` module as a session lands them.

use crate::support::{
	Kind, drain,
	harness::{peer, spawn},
	watch,
};

use moq_net::{Hop, cache, origin};

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(Hop::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (producer, driver) = origin::Producer::new(config);
	spawn(driver);
	producer
}

/// Land a peer's route at `path` on `origin`, as its session does.
fn forward(origin: &origin::Producer, path: &str, route: origin::RouteInfo) -> origin::Route {
	moq_net::fuzz::requests_route(origin, moq_net::Path::new(path).unwrap(), route).unwrap()
}

/// A permissive source sends an over-limit path to prove ingress refusal; an
/// accepted boundary path then proves egress counts the relay's own stamp.
#[kio::test]
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
		spawn(driver);
		let sink = produce_origin(3);
		let _upstream = peer(version, &source, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut received = watch(relay.consume().announced());
		let mut sent = watch(sink.consume().announced());
		kio::clock::sim::idle().await;
		let route = |count: u64| {
			moq_net::fuzz::route_info(
				moq_net::Hops::try_from((100..100 + count).map(|id| Hop::new(id).unwrap()).collect::<Vec<_>>())
					.unwrap(),
				0,
				None,
			)
		};
		let _long = forward(&source, "long", route(2));
		let _boundary = forward(&source, "boundary", route(1));
		let short = forward(&source, "short", route(0));
		kio::clock::sim::idle().await;
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
			kio::clock::sim::idle().await;
			assert_eq!(
				drain(&mut sent).get("short").and_then(|kinds| kinds.last()),
				Some(&Kind::End),
				"{name}"
			);
			let _replacement = forward(&source, "short", route(1));
			kio::clock::sim::idle().await;
			assert!(drain(&mut sent).is_empty(), "{name}: forwarded over-limit replacement");
			continue;
		}
		moq_net::fuzz::update_route(&short, route(1)).unwrap();
		kio::clock::sim::idle().await;
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
#[kio::test]
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
		spawn(driver);
		let sink = produce_origin(3);
		let _near = peer(version, &near, &relay).await;
		let _far = peer(version, &far, &relay).await;
		let _downstream = peer(version, &relay, &sink).await;
		let mut sent = watch(sink.consume().announced());
		kio::clock::sim::idle().await;

		let route = |first: u64, count: u64, cost: u64| {
			moq_net::fuzz::route_info(
				moq_net::Hops::try_from(
					(first..first + count)
						.map(|id| Hop::new(id).unwrap())
						.collect::<Vec<_>>(),
				)
				.unwrap(),
				cost,
				None,
			)
		};
		// Three hops at the relay once the sender is appended: in the cap, but full.
		let _cheap = forward(&far, "room", route(100, 2, 1));
		// Two hops at the relay, so it has room for the relay's own.
		let pricey = forward(&near, "room", route(200, 1, 50));
		kio::clock::sim::idle().await;
		let started = drain(&mut sent);
		assert_eq!(
			started.get("room").and_then(|kinds| kinds.last()),
			Some(&Kind::Start),
			"{name}: withdrew a prefix an in-cap route reaches: {started:?}"
		);

		drop(pricey);
		kio::clock::sim::idle().await;
		let ended = drain(&mut sent);
		assert_eq!(
			ended.get("room").and_then(|kinds| kinds.last()),
			Some(&Kind::End),
			"{name}: forwarded a route at the cap: {ended:?}"
		);
	}
}

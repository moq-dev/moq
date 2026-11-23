#![cfg(feature = "async")]

//! A peer asking for announcements below a route's prefix hears that route as
//! covering the prefix it asked for, the way a local consumer rooted there does.
//!
//! The route `.dash` serves every path beneath it, so a cursor rooted at
//! `.dash/nobody` sees it at its own root. Across a session the publisher must
//! translate it into the requested scope rather than end the session.

use crate::support;

use std::time::Duration;

use moq_net::{Pattern, Prefix, Prefixes, Version, origin};
use support::harness::{MockConnectOptions, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

const SERVED: &str = ".dash";
const REQUESTED: &str = ".dash/nobody";

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Drain every event the cursor has pending, as `kind prefix` lines.
fn drain(announced: &mut moq_net::origin::Routes) -> Vec<String> {
	let mut seen = Vec::new();
	while let Some(event) = announced.try_next() {
		seen.push(format!("{:?} {}", event.kind, event.announce.prefix));
	}
	seen
}

/// Announce `.dash`, then a path beneath the requested prefix, and record what a
/// cursor rooted at `.dash/nobody` sees, either on the publishing origin or on a
/// subscriber whose announce interest is that prefix.
async fn covering(version: Option<&str>) -> Vec<String> {
	let publisher = produce_origin(1);
	let everything = Prefixes::all();

	let (observer, pair) = match version {
		None => (publisher.consume(), None),
		Some(version) => {
			let subscriber = produce_origin(2);
			let interest = Prefixes::from(Prefix::try_from(REQUESTED).unwrap());
			let scoped = subscriber.scope("", &interest).unwrap();
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(scoped);
			(subscriber.consume(), Some(connect_mock(options).await))
		}
	};
	let mut announced = observer.scope(REQUESTED, &everything).unwrap().covering().routes();
	let mut log = Vec::new();

	let served = publisher
		.route(SERVED, moq_net::origin::RouteOptions::hidden())
		.unwrap();
	served.announce().unwrap();
	kio::clock::sleep(SETTLE).await;
	log.push(format!("served: {:?}", drain(&mut announced)));

	let below = publisher
		.route(format!("{REQUESTED}/cam"), moq_net::origin::RouteOptions::hidden())
		.unwrap();
	below.announce().unwrap();
	kio::clock::sleep(SETTLE).await;
	log.push(format!("below: {:?}", drain(&mut announced)));

	if let Some(pair) = pair {
		let closed = kio::clock::timeout(SETTLE, pair.client.closed()).await;
		log.push(format!("session open: {}", closed.is_err()));
	}

	log
}

const EXPECTED: &[&str] = &["served: [\"Start \"]", "below: [\"Start cam\"]"];

#[kio::test]
async fn local_cursor_sees_the_covering_route_at_its_root() {
	assert_eq!(covering(None).await, EXPECTED);
}

#[kio::test]
async fn remote_lite_peer_hears_the_covering_route() {
	let mut expected = EXPECTED.to_vec();
	expected.push("session open: true");
	for version in [
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
	] {
		assert_eq!(covering(Some(version)).await, expected, "{version}");
	}
}

#[kio::test]
async fn remote_ietf_peer_hears_the_covering_route() {
	let mut expected = EXPECTED.to_vec();
	expected.push("session open: true");
	for version in ["moq-transport-18", "moq-transport-22", "moq-transport-23"] {
		assert_eq!(covering(Some(version)).await, expected, "{version}");
	}
}

/// A relay forwards a claim at `room` through a scope granting `room/lobby.chat`, since the
/// claim covers it, so a request for `room/lobby.chat` is served across both hops. A
/// `**/*.chat` listing at the far end shows only literal matches, so the claim stays out of it.
/// lite-07 tells a claim at the scope's region instead, here `room/lobby.chat` itself, which the
/// listing then matches.
#[kio::test]
async fn relays_route_a_covering_claim_that_listings_omit() {
	let scope: Prefixes = ["lobby.chat", "room/lobby.chat"]
		.into_iter()
		.map(|prefix| Prefix::try_from(prefix).unwrap())
		.collect();
	let chat = "**/*.chat".parse::<Pattern>().unwrap();
	for version in [
		"moq-lite-03",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		// lite-07 tells the claim at the scope's region, which the listing matches.
		let (expect_listed, expect_covering): (&[&str], &[&str]) = match version {
			"moq-lite-07-wip" => (
				&["Start lobby.chat", "Start room/lobby.chat"],
				&["Start lobby.chat", "Start room/lobby.chat"],
			),
			_ => (&["Start lobby.chat"], &["Start lobby.chat", "Start room"]),
		};
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let claim = publisher
			.claim("room", moq_net::origin::RouteOptions::default())
			.unwrap();
		let listed = publisher
			.route("lobby.chat", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		listed.announce().unwrap();

		let relay = produce_origin(2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let _upstream = connect_mock(options).await;

		let client = produce_origin(3);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(relay.consume().scope("", &scope).unwrap());
		options.client_subscribe = Some(client.clone());
		let _downstream = connect_mock(options).await;

		let scoped = client.consume().filter(&chat).unwrap();
		let mut listed = scoped.routes();
		let mut covering = scoped.clone().covering().routes();
		kio::clock::sleep(SETTLE).await;
		let mut routes = drain(&mut listed);
		routes.sort();
		assert_eq!(routes, expect_listed, "{version}");
		let mut routes = drain(&mut covering);
		routes.sort();
		assert_eq!(routes, expect_covering, "{version}");

		// The claim is asked once something subscribes beneath it.
		let broadcast = kio::clock::timeout(SETTLE, scoped.request("room/lobby.chat", None))
			.await
			.expect("the request resolved")
			.unwrap();
		let track = broadcast.track("video").unwrap();
		let _subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await.map(drop) });
		let request = kio::clock::timeout(SETTLE, claim.requested())
			.await
			.expect("the request crossed both hops")
			.unwrap();
		assert_eq!(request.path().as_str(), "room/lobby.chat", "{version}");
	}
}

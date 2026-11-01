#![cfg(feature = "async")]

//! A peer asking for announcements below a route's prefix hears that route as
//! covering the prefix it asked for, the way a local consumer rooted there does.
//!
//! The route `.dash` serves every path beneath it, so a cursor rooted at
//! `.dash/nobody` sees it at its own root. Across a session the publisher must
//! translate it into the requested scope rather than end the session.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Pattern, Patterns, Version, origin};
use support::harness::{MockConnectOptions, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

const SERVED: &str = ".dash";
const REQUESTED: &str = ".dash/nobody";

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Drain every event the cursor has pending, as `kind prefix` lines.
fn drain(announced: &mut moq_net::announce::Consumer) -> Vec<String> {
	use moq_net::announce::Event;
	let mut seen = Vec::new();
	while let Some(event) = announced.try_next() {
		let (kind, announce) = match event {
			Event::Start(announce) => ("Start", announce),
			Event::Update(announce) => ("Update", announce),
			Event::Restart(announce) => ("Restart", announce),
			Event::Takeover { announce, .. } => ("Takeover", announce),
			Event::End(announce) => ("End", announce),
		};
		seen.push(format!("{kind} {}", announce.prefix));
	}
	seen
}

/// Announce `.dash`, then a path beneath the requested prefix, and record what a
/// cursor rooted at `.dash/nobody` sees, either on the publishing origin or on a
/// subscriber whose announce interest is that prefix.
async fn covering(version: Option<&str>) -> Vec<String> {
	let publisher = produce_origin(1);
	let everything = Patterns::from(Pattern::all());

	let (observer, pair) = match version {
		None => (publisher.consume(), None),
		Some(version) => {
			let subscriber = produce_origin(2);
			let interest = Patterns::from(Pattern::subtree(REQUESTED).unwrap());
			let scoped = subscriber.scope("", &interest).unwrap();
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(scoped);
			(subscriber.consume(), Some(connect_mock(options).await))
		}
	};
	let mut announced = observer.scope(REQUESTED, &everything).unwrap().covering().announced();
	let mut log = Vec::new();

	let served = publisher.create_broadcast(SERVED).unwrap();
	served.announce(Default::default()).unwrap();
	kio::clock::sleep(SETTLE).await;
	log.push(format!("served: {:?}", drain(&mut announced)));

	let below = publisher.create_broadcast(format!("{REQUESTED}/cam")).unwrap();
	below.announce(Default::default()).unwrap();
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
	for version in ["moq-transport-18", "moq-transport-22"] {
		assert_eq!(covering(Some(version)).await, expected, "{version}");
	}
}

/// A relay forwards a claim at `room` through a `**/*.chat` scope, since a path beneath it
/// could match, so a request for `room/lobby.chat` is served across both hops. Listing that
/// scope at the far end shows only literal matches, so the claim stays out of it.
#[kio::test]
async fn relays_route_a_covering_claim_that_listings_omit() {
	let chat = Patterns::from("**/*.chat".parse::<Pattern>().unwrap());
	for version in [
		"moq-lite-03",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let claim = publisher.requests("room").unwrap();
		let listed = publisher.create_broadcast("lobby.chat").unwrap();
		listed.announce(Default::default()).unwrap();

		let relay = produce_origin(2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let _upstream = connect_mock(options).await;

		let client = produce_origin(3);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(relay.consume().scope("", &chat).unwrap());
		options.client_subscribe = Some(client.clone());
		let _downstream = connect_mock(options).await;

		let scoped = client.consume().scope("", &chat).unwrap();
		let mut listed = scoped.announced();
		let mut covering = scoped.clone().covering().announced();
		kio::clock::sleep(SETTLE).await;
		assert_eq!(drain(&mut listed), ["Start lobby.chat"], "{version}");
		let mut routes = drain(&mut covering);
		routes.sort();
		assert_eq!(routes, ["Start lobby.chat", "Start room"], "{version}");

		// The claim is asked once something subscribes beneath it.
		let broadcast = kio::clock::timeout(SETTLE, scoped.request("room/lobby.chat", None))
			.await
			.expect("the request resolved")
			.unwrap();
		let track = broadcast.track("video").unwrap();
		let _subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await.map(drop) });
		let request = kio::clock::timeout(SETTLE, claim.requested_broadcast())
			.await
			.expect("the request crossed both hops")
			.unwrap();
		assert_eq!(request.path().as_str(), "room/lobby.chat", "{version}");
	}
}

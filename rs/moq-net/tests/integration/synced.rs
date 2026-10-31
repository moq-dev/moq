#![cfg(feature = "async")]

//! A subscriber's announce consumers sync once the session has delivered its
//! peer's initial announcements: exactly on versions that mark the boundary, and
//! on the answer, as a best effort, on the rest.

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::{Hop, Version, announce, origin};

use crate::support;
use support::harness::{MockConnectOptions, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Read events until the consumer syncs, returning the prefixes then announced.
async fn read_until_synced(announced: &mut announce::Consumer) -> BTreeSet<String> {
	let mut seen = BTreeSet::new();
	while !announced.poll_synced(&moq_net::kio::Waiter::noop()).is_ready() {
		let event = moq_clock::timeout(SETTLE, announced.next())
			.await
			.expect("synced in time")
			.expect("the cursor stays open");
		match event {
			announce::Event::Start(announce)
			| announce::Event::Restart(announce)
			| announce::Event::Takeover { announce, .. } => {
				seen.insert(announce.prefix.to_string());
			}
			announce::Event::End(announce) => {
				seen.remove(announce.prefix.as_str());
			}
			announce::Event::Update(_) => {}
		}
	}
	seen
}

/// Connect a subscriber to a publisher holding two broadcasts, and return what its
/// origin's consumer had read when it synced.
async fn synced(version: &str) -> BTreeSet<String> {
	let publisher = produce_origin(1);
	let a = publisher.create_broadcast("a").unwrap();
	a.announce(Default::default()).unwrap();
	let b = publisher.create_broadcast("b").unwrap();
	b.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	let _pair = connect_mock(options).await;

	let mut announced = subscriber.consume().announced();
	read_until_synced(&mut announced).await
}

fn both() -> BTreeSet<String> {
	["a", "b"].into_iter().map(String::from).collect()
}

/// Every version that marks the boundary syncs with the whole initial set.
#[moq_clock::test]
async fn an_exact_boundary_catches_up_with_the_initial_set() {
	for version in [
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		assert_eq!(synced(version).await, both(), "{version}");
	}
}

/// Versions without a boundary still sync, so nothing waiting on it hangs.
#[moq_clock::test]
async fn a_best_effort_boundary_still_catches_up() {
	for version in ["moq-lite-03", "moq-lite-04"] {
		synced(version).await;
	}
}

/// A second session into the same origin, as a reconnect makes, syncs too: the
/// consumer waits on every current session.
#[moq_clock::test]
async fn a_second_session_catches_up() {
	let publisher = produce_origin(1);
	let a = publisher.create_broadcast("a").unwrap();
	a.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let connect = || {
		let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		connect_mock(options)
	};
	let first = connect().await;
	let mut announced = subscriber.consume().announced();
	assert_eq!(
		read_until_synced(&mut announced).await,
		["a"].into_iter().map(String::from).collect()
	);

	let second = connect().await;
	drop(first);
	moq_clock::sleep(SETTLE).await;
	read_until_synced(&mut announced).await;
	drop(second);
}

/// A cold relay's empty set is complete before an upstream joins it.
#[moq_clock::test]
async fn a_cold_relay_syncs_then_forwards_upstream_routes_as_live() {
	let relay = produce_origin(2);
	let subscriber = produce_origin(3);
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let mut downstream = MockConnectOptions::new(version);
	downstream.server_publish = Some(relay.consume());
	downstream.client_subscribe = Some(subscriber.clone());
	let _downstream = connect_mock(downstream).await;
	let mut announced = subscriber.consume().announced();
	moq_net::kio::wait(|waiter| announced.poll_synced(waiter)).await;
	assert!(announced.try_next().is_none());

	let upstream = produce_origin(1);
	let broadcast = upstream.create_broadcast("room/cam").unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(upstream.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let event = announced.next().await.unwrap();
	assert!(matches!(event, announce::Event::Start(route) if route.prefix.as_str() == "room/cam"));
	assert!(announced.poll_synced(&moq_net::kio::Waiter::noop()).is_ready());
}

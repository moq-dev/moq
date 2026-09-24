//! An announce cursor on a session's origin yields `Live` once the peer's
//! initial set has landed, on every version.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, announce, origin};
use support::harness::{MockConnectOptions, connect_mock};

fn produce_origin(hop: Hop) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(hop));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Versions whose wire says where the initial set ends: ANNOUNCE_INIT or
/// ANNOUNCE_OK's count.
const COUNTED: &[&str] = &["moq-lite-01", "moq-lite-02", "moq-lite-05", "moq-lite-06"];

/// Versions that land the initial set once the stream goes quiet instead.
const QUIET: &[&str] = &["moq-lite-03", "moq-lite-04", "moq-transport-14", "moq-transport-19"];

/// Connect a subscriber to a peer publishing `paths`, then read its cursor up to
/// the marker, returning the paths delivered before it.
async fn caught_up(version: &str, paths: &[&str]) -> Vec<String> {
	let version: Version = version.parse().unwrap();
	let published = produce_origin(Hop::new(1).unwrap());
	let broadcasts: Vec<_> = paths
		.iter()
		.map(|path| {
			let broadcast = published.create_broadcast(*path).unwrap();
			broadcast.announce(Default::default()).unwrap();
			broadcast
		})
		.collect();

	let subscribed = produce_origin(Hop::new(2).unwrap());
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(published.clone());
	options.client_subscribe = Some(subscribed.clone());
	let _pair = connect_mock(options).await;

	let mut announced = subscribed.consume().announced();
	let mut live = Vec::new();
	let read = async {
		loop {
			match announced.next().await.expect("cursor closed") {
				announce::Event::Live => break,
				announce::Event::Announced(update) => live.push(update.prefix.to_string()),
				other => panic!("{version}: only announcements before the marker: got {other:?}"),
			}
		}
	};
	tokio::time::timeout(Duration::from_secs(10), read)
		.await
		.unwrap_or_else(|_| panic!("{version}: never caught up"));

	drop(broadcasts);
	live.sort();
	live
}

#[tokio::test]
async fn the_marker_follows_the_whole_initial_set() {
	for version in COUNTED.iter().chain(QUIET) {
		assert_eq!(caught_up(version, &["a", "b", "c"]).await, ["a", "b", "c"], "{version}");
	}
}

#[tokio::test]
async fn an_empty_peer_is_caught_up() {
	for version in COUNTED.iter().chain(QUIET) {
		assert!(caught_up(version, &[]).await.is_empty(), "{version}");
	}
}

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

/// Versions whose wire says where the initial set ends: ANNOUNCE_INIT, ANNOUNCE_OK's
/// count, or REQUEST_OK's (MoQ Namespace Count, negotiated from draft-16).
const COUNTED: &[&str] = &[
	"moq-lite-01",
	"moq-lite-02",
	"moq-lite-05",
	"moq-lite-06",
	"moq-transport-16",
	"moq-transport-19",
];

/// Versions that land the initial set once the stream goes quiet instead.
const QUIET: &[&str] = &["moq-lite-03", "moq-lite-04", "moq-transport-14", "moq-transport-15"];

/// The shortest a quiet stream can take to land: the gap after its last announcement.
const QUIET_GAP: Duration = Duration::from_millis(30);

/// Connect a subscriber to a peer publishing `paths`, then read its cursor up to
/// the marker, returning the paths delivered before it and how long the marker took
/// after the session was up.
async fn caught_up(version: &str, paths: &[&str]) -> (Vec<String>, Duration) {
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
	options.server_publish = Some(published.consume());
	options.client_subscribe = Some(subscribed.clone());
	let _pair = connect_mock(options).await;
	let start = tokio::time::Instant::now();

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

	let elapsed = start.elapsed();

	drop(broadcasts);
	live.sort();
	(live, elapsed)
}

// Paused, so time only moves when every task waits on a timer: a counted set lands
// without one, and a quiet one has to wait out the gap.
#[tokio::test(start_paused = true)]
async fn the_marker_follows_the_whole_initial_set() {
	for version in COUNTED.iter().chain(QUIET) {
		let (live, elapsed) = caught_up(version, &["a", "b", "c"]).await;
		assert_eq!(live, ["a", "b", "c"], "{version}");
		assert_eq!(
			QUIET.contains(version),
			elapsed >= QUIET_GAP,
			"{version}: took {elapsed:?}"
		);
	}
}

#[tokio::test(start_paused = true)]
async fn an_empty_peer_is_caught_up() {
	for version in COUNTED.iter().chain(QUIET) {
		let (live, elapsed) = caught_up(version, &[]).await;
		assert!(live.is_empty(), "{version}");
		assert_eq!(
			QUIET.contains(version),
			elapsed >= QUIET_GAP,
			"{version}: took {elapsed:?}"
		);
	}
}

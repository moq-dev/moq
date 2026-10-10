//! A relay answers a fetch over a pre-lite-05 upstream, which has no FETCH, from its live
//! subscription: a group the feed delivers is served, and one it never will is a miss,
//! never a version error that a caller would retry forever.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

const VERSIONS: &[&str] = &["moq-lite-01", "moq-lite-02", "moq-lite-03", "moq-lite-04"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

struct Relay {
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	_broadcast: moq_net::broadcast::Producer,
	_pair: support::harness::MockPair,
	_origins: (moq_net::origin::Producer, moq_net::origin::Producer),
}

/// A publisher on `version` with group 0 of a track written, and the relay's view of it.
async fn relay(version: &str) -> Relay {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let mut track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	track.write_frame(Timestamp::ZERO, &b"0"[..]).unwrap();

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let pair = connect_mock(options).await;

	let consumer = client.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	Relay {
		track,
		remote,
		_broadcast: broadcast,
		_pair: pair,
		_origins: (publisher, client),
	}
}

/// The error a fetch of `sequence` fails with.
async fn fetch_err(relay: &Relay, sequence: u64) -> moq_net::Error {
	let fetching = relay.remote.track("video").unwrap().fetch_group(sequence, None);
	match tokio::time::timeout(TIMEOUT, fetching).await.expect("fetch timed out") {
		Ok(_) => panic!("group {sequence} was served"),
		Err(err) => err,
	}
}

/// The groups a recorder fetches as the live subscription delivers them are served.
#[tokio::test(start_paused = true)]
async fn a_fetch_is_served_from_the_live_feed() {
	for version in VERSIONS {
		let mut relay = relay(version).await;
		let mut subscriber = tokio::time::timeout(TIMEOUT, relay.remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let first = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group")
			.sequence;

		// Reaches the relay before the group exists, so it can't be a plain cache hit.
		let next = first + 1;
		let mut fetching = relay.remote.track("video").unwrap().fetch_group(next, None);
		assert!(
			tokio::time::timeout(Duration::from_millis(100), &mut fetching)
				.await
				.is_err(),
			"{version}: answered before the group exists"
		);
		relay.track.write_frame(Timestamp::ZERO, &b"1"[..]).unwrap();
		let mut group = tokio::time::timeout(TIMEOUT, fetching)
			.await
			.expect("fetch timed out")
			.unwrap_or_else(|err| panic!("{version}: {err}"));
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read timed out");
		assert_eq!(&frame.unwrap().expect("a frame").payload[..], b"1", "{version}");
	}
}

/// A group with no feed at all, or one the feed went past, is a miss rather than a version
/// error.
#[tokio::test(start_paused = true)]
async fn an_unfed_fetch_is_a_miss() {
	for version in VERSIONS {
		let mut relay = relay(version).await;
		let err = fetch_err(&relay, 0).await;
		assert!(matches!(err, moq_net::Error::NotFound), "{version}: no feed: {err}");

		relay.track.write_frame(Timestamp::ZERO, &b"1"[..]).unwrap();
		relay.track.write_frame(Timestamp::ZERO, &b"2"[..]).unwrap();
		// Unfloored, the feed starts at the newest group.
		let mut subscriber = tokio::time::timeout(TIMEOUT, relay.remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let first = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group")
			.sequence;
		assert_eq!(first, 2, "{version}");
		let err = fetch_err(&relay, 1).await;
		assert!(matches!(err, moq_net::Error::NotFound), "{version}: passed: {err}");
	}
}

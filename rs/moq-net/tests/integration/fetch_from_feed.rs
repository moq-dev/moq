#![cfg(feature = "async")]

//! A relay answers a fetch over a pre-lite-05 upstream, which has no FETCH, from its live
//! subscription: a group the feed delivers is served, and one it never will is a miss,
//! never a version error that a caller would retry forever.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

const VERSIONS: &[&str] = &["moq-lite-01", "moq-lite-02", "moq-lite-03", "moq-lite-04"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

struct Relay {
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	_broadcast: moq_net::origin::Publication,
	_pair: support::harness::MockPair,
	_origins: (moq_net::origin::Producer, moq_net::origin::Producer),
}

/// A publisher on `version` with group 0 of an untimed track written, and the relay's view of it.
async fn relay(version: &str) -> Relay {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let mut track = broadcast
		.create_track("video", moq_net::track::Info::default().with_timescale(None))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	track.write_frame(None, &b"0"[..]).unwrap();

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let pair = connect_mock(options).await;

	let consumer = client.consume();
	let remote = kio::clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
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
	match kio::clock::timeout(TIMEOUT, fetching).await.expect("fetch timed out") {
		Ok(_) => panic!("group {sequence} was served"),
		Err(err) => err,
	}
}

/// The groups a recorder fetches as the live subscription delivers them are served.
#[kio::test]
async fn a_fetch_is_served_from_the_live_feed() {
	for version in VERSIONS {
		let mut relay = relay(version).await;
		let mut subscriber = kio::clock::timeout(TIMEOUT, relay.remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let first = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group")
			.sequence;

		// Reaches the relay before the group exists, so it can't be a plain cache hit.
		let next = first + 1;
		let mut fetching = relay.remote.track("video").unwrap().fetch_group(next, None);
		assert!(
			kio::clock::timeout(Duration::from_millis(100), &mut fetching)
				.await
				.is_err(),
			"{version}: answered before the group exists"
		);
		relay.track.write_frame(None, &b"1"[..]).unwrap();
		let mut group = kio::clock::timeout(TIMEOUT, fetching)
			.await
			.expect("fetch timed out")
			.unwrap_or_else(|err| panic!("{version}: {err}"));
		let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read timed out");
		assert_eq!(&frame.unwrap().expect("a frame").payload[..], b"1", "{version}");
	}
}

/// A fetch that reaches the relay alongside the subscription that will carry it, as a
/// recorder starting on an uncached group sends them, waits for that feed. The relay
/// already serves the track, with no feed, for an earlier fetch.
#[kio::test]
async fn a_fetch_waits_for_a_feed_opened_with_it() {
	for version in VERSIONS {
		let mut relay = relay(version).await;
		let err = fetch_err(&relay, 0).await;
		assert!(matches!(err, moq_net::Error::NotFound), "{version}: no feed: {err}");

		let track = relay.remote.track("video").unwrap();
		let subscribing = track.subscribe(None);
		let fetching = track.fetch_group(1, None);
		let _subscriber = kio::clock::timeout(TIMEOUT, subscribing)
			.await
			.expect("subscribe timeout")
			.expect("subscribe");

		relay.track.write_frame(None, &b"1"[..]).unwrap();
		let mut group = kio::clock::timeout(TIMEOUT, fetching)
			.await
			.expect("fetch timed out")
			.unwrap_or_else(|err| panic!("{version}: {err}"));
		let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read timed out");
		assert_eq!(&frame.unwrap().expect("a frame").payload[..], b"1", "{version}");
	}
}

/// A group with no feed at all, or one the feed went past, is a miss rather than a version
/// error.
#[kio::test]
async fn an_unfed_fetch_is_a_miss() {
	for version in VERSIONS {
		let mut relay = relay(version).await;
		let err = fetch_err(&relay, 0).await;
		assert!(matches!(err, moq_net::Error::NotFound), "{version}: no feed: {err}");

		relay.track.write_frame(None, &b"1"[..]).unwrap();
		// The publisher skips group 2, so the feed goes past it.
		let mut skipped = relay.track.create_group(moq_net::group::Info { sequence: 3 }).unwrap();
		skipped.write_frame(None, &b"3"[..]).unwrap();
		skipped.finish().unwrap();
		let mut subscriber = kio::clock::timeout(TIMEOUT, relay.remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let first = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group")
			.sequence;
		assert_eq!(first, 3, "{version}");
		let err = fetch_err(&relay, 2).await;
		assert!(matches!(err, moq_net::Error::NotFound), "{version}: passed: {err}");
	}
}

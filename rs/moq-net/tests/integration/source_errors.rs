#![cfg(feature = "async")]

//! A track that ends because its source ended reports the source's own error across a
//! relay, and a path that loses its route reports `Unroutable`, never an internal error.
//!
//! Each case runs publisher -> relay -> far over two mock sessions. moq-lite only:
//! moq-transport's PUBLISH_DONE registry has no code for either error.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, StreamError, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const VERSIONS: [&str; 3] = ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

const TIMEOUT: Duration = Duration::from_secs(10);

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A publisher announcing `bcast`, a relay subscribed to it, and a far origin subscribed
/// to the relay, with `bcast` resolved at the relay and the far end.
struct Relayed {
	_publisher: moq_net::origin::Producer,
	broadcast: moq_net::origin::Publication,
	upstream: MockPair,
	_downstream: MockPair,
	relayed: moq_net::broadcast::Consumer,
	remote: moq_net::broadcast::Consumer,
}

async fn resolve(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

async fn relayed(version: &str) -> Relayed {
	let version = version.parse::<Version>().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let far = produce_origin(3);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(far.clone());
	let downstream = connect_mock(options).await;

	Relayed {
		_publisher: publisher,
		broadcast,
		upstream,
		_downstream: downstream,
		relayed: resolve(&relay).await,
		remote: resolve(&far).await,
	}
}

/// Subscribe to `name` and read until the track ends.
async fn read_to_end(broadcast: moq_net::broadcast::Consumer, name: &'static str) -> Result<(), Error> {
	let mut sub = broadcast.track(name)?.subscribe(None).await?;
	while let Some(mut group) = sub.recv_group().await? {
		while group.read_frame().await?.is_some() {}
	}
	Ok(())
}

/// Assert `result` failed with `expected`, whether decided here or received off the wire.
#[track_caller]
fn assert_ended(result: Result<(), Error>, expected: StreamError, version: &str) {
	match result {
		// A session or transport failure converts to Unroutable too, so it must not pass as one.
		Err(err) => assert!(
			err.stream() == Some(&expected)
				|| (!matches!(err, Error::Session(_) | Error::Transport(_) | Error::IdleTimeout)
					&& err.stream().is_none()
					&& StreamError::from(&err) == expected),
			"{version}: ended with {err:?}, expected {expected:?}"
		),
		Ok(()) => panic!("{version}: ended cleanly, expected {expected:?}"),
	}
}

/// #5347: a TRACK request is in flight to an upstream publisher session that dies before
/// answering. The relay's request, and the far end's through it, find the path has no
/// route; neither is an internal error.
#[moq_clock::test]
async fn request_in_flight_to_a_dead_upstream_is_unroutable() {
	for version in VERSIONS {
		let setup = relayed(version).await;
		let _track = setup.broadcast.create_track("catalog", timed()).unwrap();
		// The relay's requests never reach the publisher.
		setup.upstream.client_transport.hold_bidis();
		let relayed = moq_clock::sim::spawn(read_to_end(setup.relayed.clone(), "catalog"));
		let remote = moq_clock::sim::spawn(read_to_end(setup.remote.clone(), "catalog"));
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(!relayed.is_finished(), "{version}: answered through a held stream");
		setup.upstream.server.abort(Error::Timeout);
		for reader in [relayed, remote] {
			let result = moq_clock::timeout(TIMEOUT, reader)
				.await
				.expect("request hung")
				.unwrap();
			assert_ended(result, StreamError::Unroutable, version);
		}
	}
}

/// A subscription already delivering when the upstream session dies, with nothing to
/// replace it: the far end learns the path is gone, not that its own session closed.
#[moq_clock::test]
async fn track_in_flight_through_a_dead_upstream_is_unroutable() {
	for version in VERSIONS {
		let setup = relayed(version).await;
		let track = setup.broadcast.create_track("video", timed()).unwrap();
		let mut sub = setup.remote.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"head".as_ref()).unwrap();
		group.finish().unwrap();
		moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.unwrap();
		setup.upstream.server.abort(Error::Timeout);
		let result = moq_clock::timeout(TIMEOUT, async {
			while sub.recv_group().await?.is_some() {}
			Ok(())
		})
		.await
		.expect("track end hung");
		assert_ended(result, StreamError::Unroutable, version);
	}
}

/// #4349: the broadcast is withdrawn under an active subscription, then its track fails
/// with a specific error. The withdrawal does not disturb the subscription, which ends
/// with that error.
#[moq_clock::test]
async fn withdrawn_broadcast_keeps_the_source_error() {
	for version in VERSIONS {
		let setup = relayed(version).await;
		let track = setup.broadcast.create_track("video", timed()).unwrap();
		let mut sub = setup.remote.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"head".as_ref()).unwrap();
		group.finish().unwrap();
		moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.unwrap();
		setup.broadcast.unannounce();
		moq_clock::sleep(Duration::from_secs(1)).await;
		track.abort(Error::App(7)).unwrap();
		let result = moq_clock::timeout(TIMEOUT, async {
			while sub.recv_group().await?.is_some() {}
			Ok(())
		})
		.await
		.expect("track end hung");
		assert_ended(result, StreamError::App(7), version);
	}
}

/// A subscribe on a broadcast whose route was withdrawn is refused as unroutable.
#[moq_clock::test]
async fn subscribe_after_withdrawal_is_unroutable() {
	for version in VERSIONS {
		let setup = relayed(version).await;
		let _track = setup.broadcast.create_track("video", timed()).unwrap();
		setup.broadcast.unannounce();
		moq_clock::sleep(Duration::from_secs(1)).await;
		for broadcast in [setup.relayed.clone(), setup.remote.clone()] {
			let result = moq_clock::timeout(TIMEOUT, read_to_end(broadcast, "video"))
				.await
				.expect("subscribe hung");
			assert_ended(result, StreamError::Unroutable, version);
		}
	}
}

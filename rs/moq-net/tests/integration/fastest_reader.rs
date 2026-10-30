#![cfg(feature = "async")]

//! A relay's ingest buffers at most a window past the group's fastest reader, then leaves
//! the rest in the stream, so flow control pushes back to the publisher. A slower reader
//! is never waited on: it falls off the group's front.

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Hop, Timestamp, Version, origin, track};

use crate::support::harness::{MockPair, peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

const MB: usize = 1 << 20;

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// A publisher's track and a relay's subscription to it, over one peering.
struct Relay {
	track: track::Producer,
	subscriber: track::Subscriber,
	_broadcast: origin::Publication,
	_pair: MockPair,
	_origins: [origin::Producer; 2],
}

async fn relay(version: &str) -> Relay {
	let publisher = origin(1);
	let relay = origin(2);
	let pair = peer(version.parse::<Version>().unwrap(), &publisher, &relay).await;

	let broadcast = publisher.create_broadcast("live").unwrap();
	let track = broadcast
		.create_track("data", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let remote = moq_clock::timeout(TIMEOUT, relay.consume().follow("live", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let subscriber = moq_clock::timeout(
		TIMEOUT,
		remote
			.track("data")
			.unwrap()
			.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600))),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe");
	Relay {
		track,
		subscriber,
		_broadcast: broadcast,
		_pair: pair,
		_origins: [publisher, relay],
	}
}

#[moq_clock::test]
async fn an_idle_reader_paces_the_relay_and_loses_nothing() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let mut reader = moq_clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		// Within the publisher's own 32 MB tail, so only the relay's pacing holds it back.
		for _ in 1..30 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		group.finish().unwrap();

		// Simulated time moves only once every task is parked: the relay's ingest stopped.
		moq_clock::sleep(Duration::from_secs(1)).await;
		let held = reader.frame_count();
		assert!(
			(16..=18).contains(&held),
			"{version}: the relay read {held} MB ahead of a reader that took nothing"
		);

		for index in 0..30 {
			let frame = moq_clock::timeout(TIMEOUT, reader.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{version}: frame {index} never arrived"))
				.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
				.expect("a frame");
			assert_eq!(frame.payload.len(), MB, "{version}: frame {index}");
		}
		let end = moq_clock::timeout(TIMEOUT, reader.read_frame())
			.await
			.expect("end timeout");
		assert!(matches!(end, Ok(None)), "{version}: {end:?}");
	}
}

#[moq_clock::test]
async fn a_slow_reader_falls_off_without_slowing_the_fast_one() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let mut fast = moq_clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		let mut slow = fast.clone();

		// Well past the 32 MB tail: the fast reader keeps up the whole way.
		for index in 0..48 {
			if index > 0 {
				group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
			}
			let frame = moq_clock::timeout(TIMEOUT, fast.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{version}: the fast reader stalled at frame {index}"))
				.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
				.expect("a frame");
			assert_eq!(frame.payload.len(), MB, "{version}: frame {index}");
		}
		group.finish().unwrap();

		let fell = moq_clock::timeout(TIMEOUT, slow.read_frame())
			.await
			.expect("read timeout");
		assert!(
			matches!(fell, Err(moq_net::Error::Evicted)),
			"{version}: the slow reader got {fell:?}"
		);
	}
}

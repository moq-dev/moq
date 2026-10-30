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
	pair: MockPair,
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
		pair,
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
			(15..=18).contains(&held),
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

/// A frame larger than the room left waits to be admitted, since admitting it trims the
/// tail to fit: an idle reader loses nothing to a large frame either.
#[moq_clock::test]
async fn a_large_frame_sheds_nothing_an_idle_reader_wants() {
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
		for _ in 1..15 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		// Let the relay take what fits, so the publisher's egress has sent every frame the
		// large one sheds from the publisher's own tail.
		moq_clock::sleep(Duration::from_secs(1)).await;
		group
			.write_frame(Timestamp::ZERO, Bytes::from(vec![7u8; 20 * MB]))
			.unwrap();
		group.finish().unwrap();

		moq_clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(
			reader.frame_count(),
			15,
			"{version}: the large frame waits for the reader"
		);
		for index in 0..16 {
			let frame = moq_clock::timeout(TIMEOUT, reader.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{version}: frame {index} never arrived"))
				.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
				.expect("a frame");
			let size = if index < 15 { MB } else { 20 * MB };
			assert_eq!(frame.payload.len(), size, "{version}: frame {index}");
		}
	}
}

/// A session whose paused streams would hold more than half its connection window reads
/// on instead, and its idle reader keeps what the live tail holds.
#[moq_clock::test]
async fn a_session_past_its_credit_cap_reads_on() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;
		// One paused stream's credit is already more than half the connection window.
		let window = moq_net::transport::ReceiveWindow::new(MB as u64, MB as u64);
		relay.pair.server_transport.set_receive_window(Some(window));

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let reader = moq_clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		for _ in 1..30 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		group.finish().unwrap();

		moq_clock::sleep(Duration::from_secs(1)).await;
		let held = reader.frame_count();
		assert_eq!(held, 30, "{version}: the relay paused past its credit cap");
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

//! Datagram delivery over the in-memory mock transport, on moq-lite and moq-transport.
//!
//! Covers the whole receive path end to end: publisher encoding, the transport's
//! datagram channel, the subscriber's receive loop and `route_datagram`, and
//! delivery through the model. The mock delivers queued datagrams
//! deterministically, so every wait here is on an observable event.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

const PAYLOAD: &[u8] = b"datagram payload";

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

struct Fixture {
	producer: moq_net::track::Producer,
	subscriber: moq_net::track::Subscriber,
	_broadcast: moq_net::broadcast::Producer,
	_pair: MockPair,
}

/// A publisher and a subscriber joined over the mock transport, sharing one
/// datagram-carrying track.
async fn connect_datagram_track() -> Fixture {
	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let producer = broadcast.create_track("datagrams", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-05".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(consumer_origin.clone());
	let pair = connect_mock(options).await;

	let consumer = consumer_origin.consume();
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();
	let subscriber = remote.track("datagrams").unwrap().subscribe(None).await.unwrap();

	Fixture {
		producer,
		subscriber,
		_broadcast: broadcast,
		_pair: pair,
	}
}

/// Datagrams written by the publisher reach the subscriber over the wire with
/// their sequence, timestamp scale and payload intact.
///
/// Datagrams are best-effort and the model evicts them by wall-clock age, so this
/// asserts what delivery actually guarantees rather than a fixed count: whatever
/// arrives is intact and in order, and the last one written arrives, since nothing
/// is pushed during the drain to evict it.
#[moq_net_sim::test]
async fn datagrams_reach_the_subscriber_in_order() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_datagram_track().await;
		const COUNT: u64 = 32;

		for sequence in 0..COUNT {
			fixture
				.producer
				.insert_datagram(
					sequence,
					Timestamp::from_millis(sequence).unwrap(),
					bytes::Bytes::from_static(PAYLOAD),
				)
				.unwrap();
		}

		let last = COUNT - 1;
		let mut seen = Vec::new();
		loop {
			let datagram = fixture.subscriber.recv_datagram().await.unwrap().unwrap();
			assert_eq!(&datagram.payload[..], PAYLOAD, "payload corrupted in transit");
			assert_eq!(
				datagram.timestamp,
				Timestamp::from_millis(datagram.sequence).unwrap(),
				"timestamp did not survive the wire"
			);
			if let Some(previous) = seen.last() {
				assert!(datagram.sequence > *previous, "datagrams delivered out of order");
			}
			seen.push(datagram.sequence);
			if datagram.sequence >= last {
				break;
			}
		}

		assert!(seen.contains(&last), "the last datagram written never arrived");
		assert!(seen.iter().all(|s| *s < COUNT), "delivered an unwritten sequence");
	})
	.await
	.expect("timed out");
}

/// MoQ Transport carries a datagram as an OBJECT_DATAGRAM at object 0, so it arrives as a
/// datagram with its sequence, alongside the groups on streams.
#[moq_net_sim::test]
async fn ietf_delivers_datagrams() {
	for version in [
		"moq-transport-14",
		"moq-transport-16",
		"moq-transport-17",
		"moq-transport-20",
	] {
		moq_net_sim::timeout(TEST_TIMEOUT, ietf_delivers_datagrams_on(version))
			.await
			.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

async fn ietf_delivers_datagrams_on(version: &str) {
	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let mut producer = broadcast.create_track("datagrams", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(consumer_origin.clone());
	let _pair = connect_mock(options).await;

	let consumer = consumer_origin.consume();
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();
	let mut subscriber = remote.track("datagrams").unwrap().subscribe(None).await.unwrap();

	// A group first, so the subscription's alias is bound before any datagram lands.
	producer
		.write_frame(Timestamp::from_millis(0).unwrap(), &b"before"[..])
		.unwrap();
	let mut before = subscriber.recv_group().await.unwrap().unwrap();
	assert_eq!(before.sequence, 0, "{version}");
	// Drafts without a track timescale stamp objects on arrival, datagrams included.
	let stamped = before.next_frame().await.unwrap().unwrap().timestamp.value() == 0;

	producer
		.insert_datagram(
			5,
			Timestamp::from_millis(7).unwrap(),
			bytes::Bytes::from_static(PAYLOAD),
		)
		.unwrap();
	let datagram = subscriber.recv_datagram().await.unwrap().unwrap();
	assert_eq!(datagram.sequence, 5, "{version}: the relay must not renumber");
	assert_eq!(&datagram.payload[..], PAYLOAD, "{version}");
	if stamped {
		let expected = Timestamp::from_millis(7).unwrap();
		assert_eq!(
			datagram.timestamp.convert(expected.scale()).unwrap(),
			expected,
			"{version}"
		);
	}

	// The group sequence continues past the datagram.
	producer
		.write_frame(Timestamp::from_millis(8).unwrap(), &b"after"[..])
		.unwrap();
	let after = subscriber.recv_group().await.unwrap().unwrap();
	assert_eq!(after.sequence, 6, "{version}");
}

/// Explicit insert keeps the origin sequence on the lite wire, including a gap.
#[moq_net_sim::test]
async fn inserted_sequences_survive_the_lite_wire() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_datagram_track().await;

		fixture
			.producer
			.insert_datagram(
				5,
				Timestamp::from_millis(5).unwrap(),
				bytes::Bytes::from_static(PAYLOAD),
			)
			.unwrap();
		fixture
			.producer
			.insert_datagram(
				2,
				Timestamp::from_millis(2).unwrap(),
				bytes::Bytes::from_static(PAYLOAD),
			)
			.unwrap();

		let first = fixture.subscriber.recv_datagram().await.unwrap().unwrap();
		assert_eq!(first.sequence, 5);
		assert_eq!(
			first.timestamp,
			Timestamp::from_millis(5).unwrap(),
			"timestamp did not survive the wire"
		);
		assert_eq!(&first.payload[..], PAYLOAD);

		let second = fixture.subscriber.recv_datagram().await.unwrap().unwrap();
		assert_eq!(second.sequence, 2);
		assert_eq!(second.timestamp, Timestamp::from_millis(2).unwrap());
		assert_eq!(&second.payload[..], PAYLOAD);
	})
	.await
	.expect("timed out");
}

/// A FETCH for a sequence the publisher sent as a datagram is refused with NOT_FETCHABLE on
/// moq-lite-07, and with the NOT_FOUND an earlier version has for it. A relay carries the
/// answer across in the downstream version's terms. The moq-transport side (DOES_NOT_EXIST)
/// is covered by the IETF publisher's own tests.
#[moq_net_sim::test]
async fn a_fetched_datagram_is_refused_per_version() {
	const NOT_FETCHABLE: &str = "not fetchable";
	const NOT_FOUND: &str = "not found";

	/// Fetch a datagram's sequence over one hop per version, publisher first.
	async fn fetch_a_datagram(versions: &[&str]) -> &'static str {
		let nodes: Vec<_> = (1..=versions.len() as u64 + 1).map(produce_origin).collect();
		let mut _pairs = Vec::new();
		for (pair, version) in nodes.windows(2).zip(versions) {
			_pairs.push(peer(version.parse().unwrap(), &pair[0], &pair[1]).await);
		}

		let broadcast = nodes[0].create_broadcast("room").unwrap();
		let mut producer = broadcast.create_track("datagrams", None).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let sequence = producer.append_datagram(Timestamp::ZERO, &b"x"[..]).unwrap();

		let consumer = nodes.last().unwrap().consume();
		consumer.routed("room").await.unwrap();
		let remote = consumer.request_broadcast("room").await.unwrap();
		let err = match remote.track("datagrams").unwrap().fetch_group(sequence, None).await {
			Ok(_) => panic!("{versions:?}: a datagram was fetched"),
			Err(err) => err,
		};
		match err {
			moq_net::Error::Stream(moq_net::StreamError::NotFetchable) => NOT_FETCHABLE,
			moq_net::Error::NotFound | moq_net::Error::Stream(moq_net::StreamError::NotFound) => NOT_FOUND,
			err => panic!("{versions:?}: refused with {err:?}"),
		}
	}

	for (versions, expected) in [
		(&["moq-lite-05"][..], NOT_FOUND),
		(&["moq-lite-06"], NOT_FOUND),
		(&["moq-lite-07-wip"], NOT_FETCHABLE),
		(&["moq-lite-07-wip", "moq-lite-07-wip"], NOT_FETCHABLE),
		(&["moq-lite-07-wip", "moq-lite-06"], NOT_FOUND),
	] {
		let refused = moq_net_sim::timeout(TEST_TIMEOUT, fetch_a_datagram(versions))
			.await
			.unwrap_or_else(|_| panic!("{versions:?}: timed out"));
		assert_eq!(refused, expected, "{versions:?}");
	}
}

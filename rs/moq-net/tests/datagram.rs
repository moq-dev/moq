//! MoQ Lite datagram delivery over the in-memory mock transport.
//!
//! Covers the whole receive path end to end: publisher encoding, the transport's
//! datagram channel, the subscriber's receive loop and `route_datagram`, and
//! delivery through the model. The mock delivers queued datagrams
//! deterministically, so every wait here is on an observable event.

mod support;

use std::time::Duration;

use moq_net::{Datagram, Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, TokioRuntime, connect_mock};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

const PAYLOAD: &[u8] = b"datagram payload";

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Info::new(Hop::new(hop).unwrap()));
	tokio::spawn(driver.run(TokioRuntime::<()>::new()));
	producer
}

struct Fixture {
	producer: moq_net::track::Producer,
	subscriber: moq_net::track::Subscriber,
	_broadcast: moq_net::broadcast::Producer,
	_announcement: moq_net::announce::Producer,
	_pair: MockPair,
}

/// A publisher and a subscriber joined over the mock transport, sharing one
/// datagram-carrying track.
async fn connect_datagram_track() -> Fixture {
	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let mut broadcast = publisher.create_broadcast("bench").unwrap();
	let announcement = publisher.announce("bench", Default::default()).unwrap();
	let producer = broadcast.create_track("datagrams", None).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-05".parse::<Version>().unwrap());
	options.server_publish = Some(publisher);
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
		_announcement: announcement,
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
#[tokio::test]
async fn datagrams_reach_the_subscriber_in_order() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_datagram_track().await;
		const COUNT: u64 = 32;

		for sequence in 0..COUNT {
			fixture
				.producer
				.write_datagram(Datagram {
					sequence,
					timestamp: Timestamp::from_millis(sequence).unwrap(),
					payload: bytes::Bytes::from_static(PAYLOAD),
				})
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

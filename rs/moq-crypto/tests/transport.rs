//! Grouped tracks on moq-lite and MoQ Transport, and datagram tracks on moq-lite.

mod support;

use std::time::Duration;

use moq_crypto::e2ee::Credential;
use moq_crypto::e2ee::credential::Config;
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// A millisecond datagram track.
fn timed_datagrams() -> moq_net::datagram::Info {
	moq_net::datagram::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A protected broadcast and its subscriber's view across one session.
struct Fixture {
	producer: moq_crypto::e2ee::broadcast::Producer,
	consumer: moq_crypto::e2ee::broadcast::Consumer,
	_broadcast: moq_net::origin::Route,
	_pair: MockPair,
}

async fn connect_protected(version: Version, label: &str) -> Fixture {
	let cred = Credential::new(Config {
		context: format!("transport-{version}-{label}").into(),
		kid: 7,
		secret: *b"moq-e2ee-00 test secret!!!!!!!!!",
	})
	.unwrap();
	let generation = cred.produce();
	let path = cred.path("meeting.hang").unwrap();

	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let options = moq_net::origin::RouteOptions::hidden().with_instance(generation.epoch().instance(), 0);
	let broadcast = publisher.route(&path, options).unwrap();
	let producer = moq_crypto::e2ee::broadcast::Producer::new(broadcast.broadcast().clone(), generation.clone());
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher);
	options.client_subscribe = Some(consumer_origin.clone());
	let pair = connect_mock(options).await;

	// The subscriber knows the opaque path and takes the epoch from the announced route's
	// instance. A wire without instances leaves it to the application's own channel.
	let consumer = consumer_origin.consume();
	let remote = consumer.follow(&path, None).unwrap().available().await.unwrap();
	if version.to_string().starts_with("moq-lite-07") {
		assert_eq!(
			moq_crypto::e2ee::Epoch::of(&remote).as_ref(),
			Some(generation.epoch()),
			"lite-07 announces the epoch"
		);
	}
	let epoch = moq_crypto::e2ee::Epoch::of(&remote).unwrap_or_else(|| generation.epoch().clone());
	let consumer = moq_crypto::e2ee::broadcast::Consumer::new(remote, cred.consume(epoch));

	Fixture {
		producer,
		consumer,
		_broadcast: broadcast,
		_pair: pair,
	}
}

async fn grouped_roundtrip(version: &str, payload: &[u8]) {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let fixture = connect_protected(version.parse().unwrap(), "video").await;
		let mut producer = fixture.producer.create_track("video", timed()).unwrap();
		let mut consumer = fixture
			.consumer
			.subscribe(
				"video",
				moq_net::track::Subscription::default()
					.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(60))),
			)
			.unwrap()
			.await
			.unwrap();
		let mut group = producer.append_group().unwrap();
		group.write_frame(Timestamp::from_millis(1).unwrap(), payload).unwrap();
		group.finish().unwrap();
		let mut group = consumer.recv_group().await.unwrap().unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(&frame.plaintext[..], payload);
	})
	.await
	.expect("timed out");
}

#[tokio::test]
async fn grouped_over_lite() {
	grouped_roundtrip("moq-lite-07-wip", b"lite-frame").await;
}

#[tokio::test]
async fn grouped_over_ietf() {
	grouped_roundtrip("moq-transport-22", b"ietf-frame").await;
}

async fn datagram_roundtrip(version: &str, plaintext: &[u8]) {
	kio::clock::timeout(TEST_TIMEOUT, async {
		let fixture = connect_protected(version.parse().unwrap(), "audio").await;
		let mut producer = fixture.producer.create_datagrams("audio", timed_datagrams()).unwrap();
		let mut consumer = fixture.consumer.datagrams("audio").unwrap();
		// A remote subscriber is live once its request reaches the publisher, which sees its priority.
		consumer.set_priority(1);
		let mut priority = producer.priority();
		while priority != Some(1) {
			priority = kio::wait(|waiter| producer.poll_priority(waiter, priority)).await;
		}

		let sequence = producer.append(Timestamp::from_millis(9).unwrap(), plaintext).unwrap();
		match consumer.recv().await.unwrap() {
			Some(moq_crypto::e2ee::datagram::Event::Datagram(d)) => {
				assert_eq!(&d.plaintext[..], plaintext);
				assert_eq!(d.sequence, sequence);
			}
			other => panic!("{other:?}"),
		}
	})
	.await
	.expect("timed out");
}

#[tokio::test]
async fn datagrams_over_lite_05() {
	datagram_roundtrip("moq-lite-05", b"opus").await;
}

/// The largest plaintext still fits lite-07's datagram body at its widest header.
#[tokio::test]
async fn max_datagram_over_lite_07() {
	datagram_roundtrip("moq-lite-07-wip", &[7; moq_crypto::e2ee::MAX_DATAGRAM_PLAINTEXT]).await;
}

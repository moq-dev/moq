//! Grouped tracks and datagrams on moq-lite and MoQ Transport.

mod support;

use std::time::Duration;

use moq_e2ee::Credential;
use moq_e2ee::credential::Config;
use moq_net::Epoch;
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

struct Fixture {
	producer: moq_e2ee::track::Producer,
	consumer: moq_e2ee::track::Consumer,
	_broadcast: moq_net::broadcast::Producer,
	_pair: MockPair,
}

async fn connect_protected(version: Version, track: &str) -> Fixture {
	let cred = Credential::new(Config {
		context: format!("transport-{version}-{track}").into(),
		kid: 7,
		secret: *b"moq-e2ee-00 test secret!!!!!!!!!",
	})
	.unwrap();
	let generation = cred.generation(Epoch::mint());
	let path = cred.path("meeting.hang").unwrap();
	let name = generation.name(track).unwrap();

	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let broadcast = publisher.create_broadcast(&path).unwrap();
	let net = broadcast.create_track(name.as_str(), None).unwrap();
	broadcast
		.announce(moq_net::origin::Route::default().with_epoch(generation.epoch().clone()))
		.unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher);
	options.client_subscribe = Some(consumer_origin.clone());
	let pair = connect_mock(options).await;

	// The subscriber knows the opaque path and takes the epoch from the announced route.
	// A wire without epochs leaves it to the application's own channel.
	let consumer = consumer_origin.consume();
	let route = consumer.routed(&path).await.unwrap();
	if version.to_string().starts_with("moq-lite-07") {
		assert_eq!(
			route.epoch.as_ref(),
			Some(generation.epoch()),
			"lite-07 announces the epoch"
		);
	}
	let epoch = route.epoch.unwrap_or_else(|| generation.epoch().clone());
	let generation = cred.generation(epoch);
	let remote = consumer.request_broadcast(&path, None).await.unwrap();
	let subscriber = remote
		.track(name.as_str())
		.unwrap()
		.subscribe(Some(
			moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60)),
		))
		.await
		.unwrap();

	Fixture {
		producer: generation.produce(net).unwrap(),
		consumer: generation.consume(subscriber).unwrap(),
		_broadcast: broadcast,
		_pair: pair,
	}
}

#[tokio::test]
async fn grouped_over_lite() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_protected("moq-lite-07-wip".parse().unwrap(), "video").await;
		let mut group = fixture.producer.append_group().unwrap();
		group
			.write_frame(Timestamp::from_millis(1).unwrap(), b"lite-frame")
			.unwrap();
		group.finish().unwrap();
		let mut group = fixture.consumer.recv_group().await.unwrap().unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(&frame.plaintext[..], b"lite-frame");
	})
	.await
	.expect("timed out");
}

#[tokio::test]
async fn grouped_over_ietf() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_protected("moq-transport-21".parse().unwrap(), "video").await;
		let mut group = fixture.producer.append_group().unwrap();
		group
			.write_frame(Timestamp::from_millis(1).unwrap(), b"ietf-frame")
			.unwrap();
		group.finish().unwrap();
		let mut group = fixture.consumer.recv_group().await.unwrap().unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(&frame.plaintext[..], b"ietf-frame");
	})
	.await
	.expect("timed out");
}

async fn datagram_roundtrip(version: &str) {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_protected(version.parse().unwrap(), "audio").await;
		fixture
			.producer
			.append_datagram(Timestamp::from_millis(9).unwrap(), b"opus")
			.unwrap();
		match fixture.consumer.recv_datagram().await.unwrap() {
			Some(moq_e2ee::datagram::Event::Datagram(d)) => {
				assert_eq!(&d.plaintext[..], b"opus");
				assert_eq!(d.sequence, 0);
			}
			other => panic!("{other:?}"),
		}
	})
	.await
	.expect("timed out");
}

#[tokio::test]
async fn datagrams_over_lite() {
	datagram_roundtrip("moq-lite-05").await;
}

#[tokio::test]
async fn datagrams_over_ietf() {
	datagram_roundtrip("moq-transport-21").await;
}

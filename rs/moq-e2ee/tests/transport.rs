//! Grouped tracks and datagrams on moq-lite and MoQ Transport.

mod support;

use std::time::Duration;

use moq_e2ee::Credential;
use moq_e2ee::credential::Config;
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin() -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
	tokio::spawn(support::harness::run(driver));
	producer
}

/// A millisecond datagram track.
fn timed_datagrams() -> moq_net::datagram::Info {
	moq_net::datagram::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A protected broadcast and its subscriber's view across one session.
struct Fixture {
	producer: moq_e2ee::broadcast::Producer,
	consumer: moq_e2ee::broadcast::Consumer,
	_broadcast: moq_net::origin::Route,
	_pair: MockPair,
}

async fn connect_protected(version: Version, label: &str) -> Fixture {
	let cred = Credential::new(Config {
		context: format!("transport-{version}-{label}").into(),
		kid: 7,
		secret: *b"moq-e2ee-01-wip test secret!!!!!",
	})
	.unwrap();
	let generation = cred.produce();
	let path = cred.path("meeting.hang").unwrap();

	let publisher = produce_origin();
	let consumer_origin = produce_origin();

	let options = moq_net::origin::RouteOptions::hidden().with_identity(generation.epoch().identity(), 0);
	let broadcast = publisher.route(&path, options).unwrap();
	let producer = moq_e2ee::broadcast::Producer::new(broadcast.broadcast().clone(), generation.clone());
	broadcast.announce().unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher);
	options.client_subscribe = Some(consumer_origin.clone());
	let pair = connect_mock(options).await;

	// The subscriber knows the opaque path and takes the epoch from the announced route's
	// identity. A wire without identities leaves it to the application's own channel.
	let consumer = consumer_origin.consume();
	let remote = consumer
		.follow(&path, None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	if version.to_string().starts_with("moq-lite-07") {
		assert_eq!(
			moq_e2ee::Epoch::of(&remote).as_ref(),
			Some(generation.epoch()),
			"lite-07 announces the epoch"
		);
	}
	let epoch = moq_e2ee::Epoch::of(&remote).unwrap_or_else(|| generation.epoch().clone());
	let consumer = moq_e2ee::broadcast::Consumer::new(remote, cred.consume(epoch));

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
		let mut producer = fixture
			.producer
			.create_track("video", timed())
			.unwrap()
			.expect("a free name");
		let mut consumer = fixture
			.consumer
			.subscribe(
				"video",
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(60)),
						max: Some(Duration::from_secs(60)),
					},
				)]),
			)
			.unwrap()
			.await
			.unwrap();
		let group = producer
			.append_group(Timestamp::from_millis(1).unwrap(), payload)
			.unwrap();
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
		let mut producer = fixture
			.producer
			.create_datagrams("audio", timed_datagrams())
			.unwrap()
			.expect("a free name");
		let mut consumer = fixture.consumer.datagrams("audio").unwrap();
		// A remote subscriber is live once its request reaches the publisher, which sees its priority.
		consumer.set_priority(1);
		let mut priority = producer.priority();
		while priority != Some(1) {
			priority = kio::wait(|waiter| producer.poll_priority_changed(waiter)).await;
		}

		let sequence = producer.append(Timestamp::from_millis(9).unwrap(), plaintext).unwrap();
		match consumer.recv().await.unwrap() {
			Some(moq_e2ee::datagram::Event::Datagram(d)) => {
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
	datagram_roundtrip("moq-lite-07-wip", &[7; moq_e2ee::MAX_DATAGRAM_PLAINTEXT]).await;
}

#[tokio::test]
async fn datagrams_over_ietf() {
	datagram_roundtrip("moq-transport-22", b"opus").await;
}

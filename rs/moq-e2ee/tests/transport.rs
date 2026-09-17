//! Grouped tracks on moq-lite and MoQ Transport; datagrams on moq-lite.

mod support;

use std::time::Duration;

use moq_e2ee::{Credential, PROFILE};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, TokioRuntime, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(driver.run(TokioRuntime::<()>::new()));
	producer
}

struct Fixture {
	producer: moq_e2ee::track::Producer,
	consumer: moq_e2ee::track::Consumer,
	_broadcast: moq_net::broadcast::Producer,
	_pair: MockPair,
}

async fn connect_protected(version: Version, track: &str) -> Fixture {
	let cred = Credential::new(
		PROFILE,
		format!("transport-{version}-{track}"),
		1,
		7,
		*b"moq-e2ee-01 test secret!!!!!!!!!",
	)
	.unwrap();
	let publication = cred.publish().unwrap();
	let physical = cred.physical_name(track).unwrap();

	let publisher = produce_origin(1);
	let consumer_origin = produce_origin(2);

	let mut broadcast = publisher.create_broadcast("bench.e2ee").unwrap();
	let net = broadcast.create_track(physical.as_str(), None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher);
	options.client_subscribe = Some(consumer_origin.clone());
	let pair = connect_mock(options).await;

	let consumer = consumer_origin.consume();
	consumer.routed("bench.e2ee").await.unwrap();
	let remote = consumer.request_broadcast("bench.e2ee").await.unwrap();
	let subscriber = remote
		.track(physical.as_str())
		.unwrap()
		.subscribe(Some(
			moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60)),
		))
		.await
		.unwrap();

	Fixture {
		producer: publication.track(net, track).unwrap(),
		consumer: moq_e2ee::track::Consumer::new(&cred, subscriber, Some(&cred.pin())).unwrap(),
		_broadcast: broadcast,
		_pair: pair,
	}
}

#[tokio::test]
async fn grouped_over_lite() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_protected("moq-lite-05".parse().unwrap(), "video").await;
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

#[tokio::test]
async fn datagrams_over_lite() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let mut fixture = connect_protected("moq-lite-05".parse().unwrap(), "audio").await;
		fixture
			.producer
			.append_datagram(Timestamp::from_millis(9).unwrap(), b"opus")
			.unwrap();
		match fixture.consumer.recv_datagram().await.unwrap() {
			Some(moq_e2ee::DatagramEvent::Datagram(d)) => {
				assert_eq!(&d.plaintext[..], b"opus");
				assert_eq!(d.sequence, 0);
			}
			other => panic!("{other:?}"),
		}
	})
	.await
	.expect("timed out");
}

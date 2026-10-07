//! A group cut below a finished track's end fails the subscription, but a publisher
//! serving it still sends the groups already in flight: a cut upstream must not reset
//! healthy siblings downstream.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn sibling_survives(version: &str) {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let broadcast = publisher.create_broadcast("bench").unwrap();
	let mut track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _pair = connect_mock(options).await;

	let consumer = relay.consume();
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();
	let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();

	let mut local = publisher
		.consume()
		.request_broadcast("bench")
		.await
		.unwrap()
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.unwrap();
	// Group 0 is still sending when its sibling is cut.
	let mut sibling = track.append_group().unwrap();
	sibling.write_frame(Timestamp::ZERO, b"head".as_ref()).unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.sequence, 0);
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"head".as_ref());

	let cut = track.append_group().unwrap();
	track.finish_at(2).unwrap();
	cut.abort(moq_net::Error::Stream(moq_net::StreamError::Cancel)).unwrap();
	// Let the serving side reach the cut while the sibling is still open.
	moq_net_sim::sleep(Duration::from_millis(100)).await;
	// The origin's own reader sees the cut, then the end.
	assert_eq!(local.recv_group().await.unwrap().unwrap().sequence, 0);
	assert!(matches!(
		local.recv_group().await,
		Err(moq_net::Error::Stream(moq_net::StreamError::Cancel))
	));
	assert!(matches!(local.recv_group().await, Ok(None)));

	sibling
		.write_frame(Timestamp::from_millis(1).unwrap(), b"tail".as_ref())
		.unwrap();
	sibling.finish().unwrap();

	let tail = moq_net_sim::timeout(Duration::from_secs(2), group.read_frame())
		.await
		.expect("sibling read hung");
	assert_eq!(
		tail.expect("the cut reset its sibling").unwrap().payload,
		b"tail".as_ref()
	);
	let end = moq_net_sim::timeout(Duration::from_secs(2), group.read_frame())
		.await
		.expect("sibling end hung");
	assert!(matches!(end, Ok(None)), "the sibling finishes, got {end:?}");
	// TODO(#4998): the downstream subscription must end with the cut, not resubscribe.
	let next = moq_net_sim::timeout(Duration::from_secs(2), sub.recv_group()).await;
	assert!(matches!(next, Ok(Err(_))), "downstream end: {:?}", next.map(|r| r.map(|g| g.map(|g| g.sequence))));
}

macro_rules! sibling_test {
	($name:ident, $version:literal) => {
		#[moq_net_sim::test]
		async fn $name() {
			sibling_survives($version).await;
		}
	};
}
sibling_test!(lite_03, "moq-lite-03");
sibling_test!(lite_04, "moq-lite-04");
sibling_test!(lite_05, "moq-lite-05");
sibling_test!(lite_06, "moq-lite-06");
sibling_test!(ietf_14, "moq-transport-14");
sibling_test!(ietf_22, "moq-transport-22");

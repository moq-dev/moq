//! A group sequence only moq-lite 07 can carry, relayed to a moq-lite 06 reader.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The relay accepts the wide sequence from lite-07, refuses that subscription to the
/// lite-06 reader that cannot encode it, and keeps the session serving other tracks.
#[moq_net_sim::test]
async fn wide_sequence_fails_only_its_subscription() {
	const WIDE: u64 = 1 << 62;

	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let client = produce_origin(3);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let wide = broadcast.create_track("wide", None).unwrap();
	let narrow = broadcast.create_track("narrow", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = wide.create_group(moq_net::group::Info { sequence: WIDE }).unwrap();
	group.write_frame(Timestamp::ZERO, b"wide".as_ref()).unwrap();
	let mut group = narrow.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"narrow".as_ref()).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let _downstream = connect_mock(options).await;

	// lite-07 carries the full 64 bits, so the relay holds the group.
	relay.consume().routed("bench").await.unwrap();
	let cached = relay.consume().request_broadcast("bench").await.unwrap();
	let mut sub = cached.track("wide").unwrap().subscribe(None).await.unwrap();
	assert_eq!(sub.recv_group().await.unwrap().unwrap().sequence, WIDE);

	let consumer = client.consume();
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();

	let result = moq_net_sim::timeout(Duration::from_secs(5), async {
		let mut sub = remote.track("wide").unwrap().subscribe(None).await?;
		sub.recv_group().await.map(|group| group.map(|group| group.sequence))
	})
	.await
	.expect("the wide subscription hung instead of failing");
	// The group does not fit a QUIC varint. Encode failures have no dedicated stream
	// code, so the subscribe stream resets with INTERNAL_ERROR and the session stays up.
	assert!(
		matches!(result, Err(moq_net::Error::Stream(moq_net::StreamError::Internal))),
		"lite-06 should see an internal stream error, not {result:?}"
	);

	let mut sub = remote.track("narrow").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"narrow".as_ref());
}

/// A subscription that starts inside the QUIC range and later receives a group of
/// `1 << 62` fails only that subscription. The session and the other track continue.
#[moq_net_sim::test]
async fn a_later_wide_group_fails_only_that_subscription() {
	const WIDE: u64 = 1 << 62;

	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let client = produce_origin(3);

	let broadcast = publisher.create_broadcast("bench").unwrap();
	let wide = broadcast.create_track("wide", None).unwrap();
	let narrow = broadcast.create_track("narrow", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = wide.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"first".as_ref()).unwrap();
	let mut group = narrow.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"narrow".as_ref()).unwrap();

	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let _downstream = connect_mock(options).await;

	let consumer = client.consume();
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();

	let mut sub = remote.track("wide").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.sequence, 0);
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"first".as_ref());

	let mut group = wide.create_group(moq_net::group::Info { sequence: WIDE }).unwrap();
	group.write_frame(Timestamp::ZERO, b"wide".as_ref()).unwrap();

	let result = moq_net_sim::timeout(Duration::from_secs(5), sub.recv_group())
		.await
		.expect("the wide group hung the subscription instead of failing it");
	match result {
		Err(moq_net::Error::Stream(moq_net::StreamError::Internal)) => {}
		Err(err) => {
			panic!("the later wide group should fail the subscription with an internal stream error, not {err:?}")
		}
		Ok(_) => panic!("the later wide group should fail the subscription with an internal stream error, got a group"),
	}

	let mut sub = remote.track("narrow").unwrap().subscribe(None).await.unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"narrow".as_ref());
}

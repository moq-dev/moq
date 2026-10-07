//! A relay with nothing cached still names its upstream's Largest, so a draft-16
//! live join receives the current group from object 0.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn read(group: &mut moq_net::group::Consumer) -> Vec<u8> {
	moq_net_sim::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("no frame")
		.expect("read_frame")
		.expect("group ended")
		.payload
		.to_vec()
}

/// Publisher, cold relay, draft-16 subscriber. The relay has no object cached when
/// the subscriber joins. The join is a relative FETCH at offset 0, and the group
/// arrives from its first object.
#[moq_net_sim::test]
async fn a_cold_relay_serves_the_current_group_from_object_zero() {
	let version: Version = "moq-transport-16".parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut upstream = MockConnectOptions::new(version);
	upstream.server_publish = Some(publisher.consume());
	upstream.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(upstream).await;

	let peer = produce_origin(3);
	let mut downstream = MockConnectOptions::new(version);
	downstream.server_publish = Some(relay.consume());
	downstream.client_subscribe = Some(peer.clone());
	let _downstream = connect_mock(downstream).await;

	let consumer = peer.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");

	// The objects exist only at the publisher. The relay subscribes when the peer
	// does, so its cache is empty at that SUBSCRIBE_OK.
	const GROUP: u64 = 7;
	let mut group = track.create_group(moq_net::group::Info { sequence: GROUP }).unwrap();
	for payload in [b"o0".as_slice(), b"o1", b"o2"] {
		group.write_frame(Timestamp::from_millis(0).unwrap(), payload).unwrap();
	}
	group.finish().unwrap();

	let remote = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let mut sub = moq_net_sim::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut got = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("no group")
		.expect("recv_group")
		.expect("group");
	assert_eq!(got.sequence, GROUP);
	assert_eq!(read(&mut got).await, b"o0");
	assert_eq!(read(&mut got).await, b"o1");
	assert_eq!(read(&mut got).await, b"o2");

	drop((
		got,
		sub,
		group,
		track,
		broadcast,
		_downstream,
		_upstream,
		publisher,
		relay,
		peer,
	));
}

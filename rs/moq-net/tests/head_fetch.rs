//! A relay that fetches the head of a group it is receiving mid-group keeps delivering
//! that group to arrival-order subscribers, along with the frames written into it after.
//!
//! A mesh peer subscribing from partway into an open group (a catalog: snapshot, then
//! deltas) has its relay pull the group headless. Reading the peer's copy from frame 0
//! then fetches the head through the relay, whose cache must not trade the live group
//! for the fetched copy.

mod support;

use std::time::Duration;

use moq_net::track::{Position, Subscription};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn resolve(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
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

/// Publisher P holds an open group, relay R pulls P, peer M pulls R from the group's
/// second frame and then reads its copy from the first. A fresh subscriber on R must
/// still get the whole group, and a frame P writes afterward.
async fn round(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("catalog.json", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut group = track.append_group().unwrap();
	let sequence = group.sequence;
	for payload in [&b"snapshot"[..], b"delta1", b"delta2"] {
		group.write_frame(Timestamp::now(), payload).unwrap();
	}

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let peer = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(peer.clone());
	let downstream = connect_mock(options).await;

	let remote = resolve(&relay).await;
	let peer_remote = resolve(&peer).await;

	let start = Position {
		group: sequence,
		frame: 1,
	};
	let mut peer_sub = peer_remote
		.track("catalog.json")
		.unwrap()
		.subscribe(Subscription::default().with_start(start))
		.await
		.expect("peer subscribe");
	let mut peer_group = moq_net_sim::timeout(TIMEOUT, peer_sub.recv_group())
		.await
		.expect("peer got no group")
		.unwrap()
		.unwrap();
	// The peer reads its headless copy from frame 0: a fetch of the head through R.
	let _ = moq_net_sim::timeout(Duration::from_millis(500), peer_group.read_frame()).await;

	// A fresh reader on R wants the latest group from its snapshot.
	let mut sub = remote.track("catalog.json").unwrap().subscribe(None).await.unwrap();
	let mut got = moq_net_sim::timeout(TIMEOUT, sub.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{version}: no group"))
		.unwrap()
		.unwrap();
	assert_eq!(got.sequence, sequence, "{version}");
	assert_eq!(read(&mut got).await, b"snapshot", "{version}");
	assert_eq!(read(&mut got).await, b"delta1", "{version}");
	assert_eq!(read(&mut got).await, b"delta2", "{version}");

	// The publisher keeps writing into the group: the reader on R hears it.
	moq_net_sim::sleep(Duration::from_secs(1)).await;
	group.write_frame(Timestamp::now(), &b"delta3"[..]).unwrap();
	assert_eq!(
		read(&mut got).await,
		b"delta3",
		"{version}: frame written after the fetch"
	);

	// Every reader of the fetched copy leaves, so its fetch is abandoned, while both
	// subscriptions keep the live feed running. The live group still holds R's slot: a
	// later reader gets the whole group again.
	drop((got, peer_group));
	moq_net_sim::sleep(Duration::from_secs(1)).await;
	let mut late_sub = remote.track("catalog.json").unwrap().subscribe(None).await.unwrap();
	let mut late = moq_net_sim::timeout(TIMEOUT, late_sub.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{version}: no group after the fetch was abandoned"))
		.unwrap()
		.unwrap();
	assert_eq!(late.sequence, sequence, "{version}");
	for payload in [&b"snapshot"[..], b"delta1", b"delta2", b"delta3"] {
		assert_eq!(
			read(&mut late).await,
			payload,
			"{version}: after the fetch was abandoned"
		);
	}

	drop((
		late, late_sub, sub, peer_sub, group, track, broadcast, downstream, upstream, publisher, relay, peer,
	));
}

#[moq_net_sim::test]
async fn a_fetched_head_keeps_the_live_group_visible() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		if moq_net_sim::spawn(round(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

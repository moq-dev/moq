//! A track aborted by its publisher while the broadcast stays announced, read by a
//! subscriber on the far side of one session.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A group reader alone must drive failover, without polling the next group.
async fn after_abort(version: &str, hops: u32) {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let far = produce_origin(3);
	let broadcast = publisher.create_broadcast("bench").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _pair = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(far.clone());
	let _pair2 = connect_mock(options).await;
	let consumer = if hops == 2 { far.consume() } else { relay.consume() };
	consumer.routed("bench").await.unwrap();
	let remote = consumer.request_broadcast("bench").await.unwrap();
	let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
	let mut open = track.append_group().unwrap();
	open.write_frame(Timestamp::ZERO, b"head".as_ref()).unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.read_frame().await.unwrap().unwrap().payload, b"head".as_ref());
	track.abort(moq_net::Error::Cancel).unwrap();
	drop(open);
	assert!(
		moq_net_sim::timeout(Duration::from_secs(2), group.read_frame())
			.await
			.expect("group-only read hung")
			.is_err()
	);
}

macro_rules! abort_test {
	($name:ident, $version:literal, $hops:literal) => {
		#[moq_net_sim::test]
		async fn $name() {
			after_abort($version, $hops).await;
		}
	};
}
abort_test!(abort_03_one, "moq-lite-03", 1);
abort_test!(abort_03_two, "moq-lite-03", 2);
abort_test!(abort_04_one, "moq-lite-04", 1);
abort_test!(abort_04_two, "moq-lite-04", 2);
abort_test!(abort_05_one, "moq-lite-05", 1);
abort_test!(abort_05_two, "moq-lite-05", 2);
abort_test!(abort_06_one, "moq-lite-06", 1);
abort_test!(abort_06_two, "moq-lite-06", 2);

async fn recreate(finish: bool) {
	let origin = produce_origin(1);
	let broadcast = origin.create_broadcast("bench").unwrap();
	broadcast.announce(Default::default()).unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	let remote = origin.consume().request_broadcast("bench").await.unwrap();
	let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
	let mut open = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
	open.write_frame(Timestamp::ZERO, b"head".as_ref()).unwrap();
	let mut group = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(group.sequence, 2);
	group.read_frame().await.unwrap().unwrap();
	track.abort(moq_net::Error::Cancel).unwrap();
	let fresh = broadcast.create_track("video", None).unwrap();
	let mut next = fresh.create_group(moq_net::group::Info { sequence: 3 }).unwrap();
	next.write_frame(Timestamp::from_millis(1).unwrap(), b"next".as_ref())
		.unwrap();
	drop(open);
	let result = moq_net_sim::timeout(Duration::from_secs(2), async {
		if finish {
			group.finished().await.map(|_| ())
		} else {
			group.read_frame().await.map(|_| ())
		}
	})
	.await
	.expect("group-only failover hung");
	assert!(result.is_err());
	let next = sub.recv_group().await.unwrap().unwrap();
	assert_eq!(next.sequence, 3);
}

#[moq_net_sim::test]
async fn recreated_track_skips_old_group() {
	recreate(false).await;
}

#[moq_net_sim::test]
async fn recreated_track_finishes_old_group() {
	recreate(true).await;
}

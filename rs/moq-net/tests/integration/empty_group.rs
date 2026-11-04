#![cfg(feature = "async")]

//! A group finished with no frames reaches the subscriber as an empty group.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin};
use support::harness::{peer, subscribed};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-transport-18", "moq-transport-22"];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

#[kio::test]
async fn an_empty_group_arrives_empty() {
	for version in VERSIONS {
		let publisher = produce_origin(1);
		let subscriber = produce_origin(2);
		let _pair = peer(version.parse::<Version>().unwrap(), &publisher, &subscriber).await;

		let broadcast = publisher.create_broadcast("room").unwrap();
		let track = broadcast.create_track("data", timed()).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let consumer = subscriber.consume();
		let remote = kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		let mut sub = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		subscribed(&track).await;

		track.append_group().unwrap().finish().unwrap();
		let mut empty = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: the empty group was dropped"))
			.unwrap()
			.expect("a group");
		assert_eq!(empty.sequence, 0, "{version}");
		let frame = kio::clock::timeout(TIMEOUT, empty.read_frame())
			.await
			.expect("frame timeout")
			.unwrap();
		assert!(frame.is_none(), "{version}: the empty group carried a frame");

		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, &b"ok"[..]).unwrap();
		group.finish().unwrap();
		let mut next = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		assert_eq!(next.sequence, 1, "{version}");
		let frame = next.read_frame().await.unwrap().expect("a frame");
		assert_eq!(frame.payload.as_ref(), b"ok", "{version}");
	}
}

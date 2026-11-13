#![cfg(feature = "async")]

//! Viewers joining a relay that already holds a track's history, each on its own session.

use std::time::Duration;

use moq_net::{Timestamp, Version, cache, origin, track};

use crate::support::AppendNext;
use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin.with_proxy(true)
}

/// Every viewer that joins reads the group the relay holds. Each join subscribes upstream
/// again once the last viewer left, so the publisher serves the held group again; the relay
/// refuses that copy, and a lite publisher before lite-07 then drops the group for good.
/// That drop must not hide the group the relay holds whole.
#[kio::test]
async fn each_joining_viewer_reads_the_held_group() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let relay = origin(2);
		let mut options = MockConnectOptions::new(version);
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(relay.clone());
		let _upstream = connect_mock(options).await;

		let broadcast = publisher.route("live", origin::RouteOptions::default()).unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let source = broadcast.broadcast().create_track("video", info).unwrap();
		let mut group = source.append_next().unwrap();
		group
			.write_frame(Timestamp::from_millis(0).unwrap(), &b"x"[..])
			.unwrap();
		group.close().unwrap();

		for join in 0..3 {
			let viewer = origin(3 + join);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(viewer.clone());
			let downstream = connect_mock(options).await;
			kio::clock::timeout(TIMEOUT, async {
				let remote = viewer.consume().follow("live", None).unwrap().available().await;
				let remote = remote.unwrap().expect("the broadcast resolves");
				let mut subscriber = remote.track("video").unwrap().subscribe(None).await.unwrap();
				let mut group = subscriber.recv_group().await.unwrap().expect("a group");
				assert_eq!(group.start, 0, "{version}: viewer {join}");
				let frame = group.read_frame().await.unwrap().expect("a frame");
				assert_eq!(&frame.payload[..], b"x", "{version}: viewer {join}");
			})
			.await
			.unwrap_or_else(|_| panic!("{version}: viewer {join} never read the held group"));
			drop(downstream);
		}
	}
}

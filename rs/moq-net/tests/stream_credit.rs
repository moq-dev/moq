//! A peer granting one uni stream at a time still receives every group, on each protocol.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, track};
use support::harness::{MockConnectOptions, connect_mock};

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

#[tokio::test(start_paused = true)]
async fn one_stream_credit_serves_every_backlogged_group() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let viewer = produce_origin(2);
		let broadcast = publisher.create_broadcast("live").unwrap();
		let tracks: Vec<_> = (0..2)
			.map(|i| broadcast.create_track(format!("{i}"), None).unwrap())
			.collect();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		// Draft 22 keeps its SETUP stream open, so it needs one more for groups.
		options.max_concurrent_uni_streams = Some(1 + usize::from(version.is_ietf()));
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let consumer = viewer.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live").await.unwrap();
		let prefs = track::Subscription::default().with_max_age(Duration::from_secs(30));
		let mut subscribers = Vec::new();
		for i in 0..tracks.len() {
			let track = remote.track(&format!("{i}")).unwrap();
			subscribers.push(track.subscribe(prefs.clone()).await.unwrap());
		}

		for _ in 0..4 {
			for track in &tracks {
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
				group.finish().unwrap();
			}
		}
		for subscriber in &mut subscribers {
			// The transport, not the publisher, picks which parked open gets the next grant.
			let mut received = Vec::new();
			while received.len() < 4 {
				let group = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_group())
					.await
					.unwrap_or_else(|_| panic!("{version}: only {received:?} arrived"))
					.unwrap()
					.unwrap();
				received.push(group.sequence);
			}
			received.sort();
			assert_eq!(received, [0, 1, 2, 3], "{version}");
		}
	}
}

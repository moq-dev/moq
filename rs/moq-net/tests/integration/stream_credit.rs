#![cfg(feature = "async")]

//! A peer granting one uni stream at a time still receives every group, on each protocol.

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

#[moq_clock::test]
async fn one_stream_credit_serves_every_backlogged_group() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher.create_broadcast("live").unwrap();
		let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let tracks: Vec<_> = (0..2)
			.map(|i| broadcast.create_track(format!("{i}"), info.clone()).unwrap())
			.collect();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		// Draft 22 keeps its SETUP stream open, so it needs one more for groups.
		options.transport.max_concurrent_uni_streams = Some(1 + usize::from(!version.is_lite()));
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let _pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let prefs = track::Subscription::default().with_max_delay(Duration::from_secs(30));
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
				let group = moq_clock::timeout(Duration::from_secs(5), subscriber.recv_group())
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

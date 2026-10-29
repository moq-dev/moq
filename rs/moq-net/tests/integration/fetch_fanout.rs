#![cfg(feature = "async")]

//! A relay fetches each group's payload upstream once, independently of how many
//! viewers ask for it, and keeps serving the cached copy after a reader leaves.

use crate::support;
use moq_net::{Hop, Version, origin, track};
use std::time::Duration;
use support::harness::{MockConnectOptions, connect_mock};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	origin
}

#[moq_clock::test]
async fn sequential_viewers_share_the_relays_fetched_groups() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let broadcast = publisher.publish("room", Default::default()).unwrap();
		let track = broadcast
			.create_track(
				"video",
				track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let relay = origin(2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let upstream = connect_mock(options).await;
		let mut viewers = Vec::new();
		for i in 0..64 {
			let viewer = origin(3 + i);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(relay.consume());
			options.client_subscribe = Some(viewer.clone());
			let pair = connect_mock(options).await;
			let remote = viewer
				.consume()
				.follow("room", None)
				.unwrap()
				.available()
				.await
				.unwrap();
			viewers.push((viewer, pair, remote));
		}
		for round in 0..3 {
			let mut group = track.append_group().unwrap();
			let sequence = group.sequence;
			group
				.write_frame(
					moq_net::Timestamp::from_millis(round * 1000).unwrap(),
					b"payload".as_ref(),
				)
				.unwrap();
			group.finish().unwrap();
			for (i, (_, _, remote)) in viewers.iter().enumerate() {
				let mut group = remote
					.track("video")
					.unwrap()
					.fetch_group(sequence, None)
					.await
					.unwrap();
				assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"payload");
				assert!(group.read_frame().await.unwrap().is_none());
				// FETCH's lite stream type is 3, and its varint occupies one byte.
				let fetches = upstream
					.client_transport
					.bidi_types()
					.into_iter()
					.filter(|kind| *kind == 3)
					.count();
				assert_eq!(fetches, round as usize + 1, "{version}: viewer {i}, group {sequence}");
			}
			moq_clock::sleep(Duration::from_secs(1)).await;
		}
		let types = upstream.client_transport.bidi_types();
		assert_eq!(
			types.iter().filter(|kind| **kind == 6).count(),
			1,
			"metadata is learned once"
		);
		assert!(!types.contains(&2), "a fetch-only relay must never subscribe");
		assert!(track.subscription().is_none());
	}
}

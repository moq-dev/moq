#![cfg(feature = "async")]

//! A relay fetches each group's payload upstream once, independently of how many
//! viewers ask for it, and keeps serving the cached copy after a reader leaves.

use crate::support;
use moq_net::{Version, origin, track};
use std::time::Duration;
use support::harness::{MockConnectOptions, connect_mock};

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (origin, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	origin
}

#[kio::test]
async fn sequential_viewers_share_the_relays_fetched_groups() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let broadcast = publisher.route("room", origin::RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
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
				.unwrap()
				.unwrap();
			viewers.push((viewer, pair, remote));
		}
		for round in 0..3 {
			let group = track
				.append_group(
					moq_net::Timestamp::from_millis(round * 1000).unwrap(),
					b"payload".as_ref(),
				)
				.unwrap();
			let key = group.start;
			group.finish().unwrap();
			for (i, (_, _, remote)) in viewers.iter().enumerate() {
				let mut group = remote.track("video").unwrap().fetch_group(key, None).await.unwrap();
				assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"payload");
				assert!(group.read_frame().await.unwrap().is_none());
				// FETCH's lite stream type is 3 on lite-05/06; lite-07 uses one-shot SUBSCRIBE (2).
				let lite07 = version == "moq-lite-07-wip".parse::<Version>().unwrap();
				let fetch_kind = if lite07 { 2 } else { 3 };
				let fetches = upstream
					.client_transport
					.bidi_types()
					.into_iter()
					.filter(|kind| *kind == fetch_kind)
					.count();
				assert_eq!(fetches, round as usize + 1, "{version}: viewer {i}, group {key}");
			}
			kio::clock::sleep(Duration::from_secs(1)).await;
		}
		let types = upstream.client_transport.bidi_types();
		assert_eq!(
			types.iter().filter(|kind| **kind == 6).count(),
			1,
			"metadata is learned once"
		);
		let lite07 = version == "moq-lite-07-wip".parse::<Version>().unwrap();
		if lite07 {
			assert!(!types.contains(&3), "lite-07 must never send FETCH");
		} else {
			assert!(!types.contains(&2), "a fetch-only relay must never subscribe");
		}
		assert!(track.subscription().is_none());
	}
}

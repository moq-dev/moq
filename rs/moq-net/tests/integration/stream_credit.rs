#![cfg(feature = "async")]

//! A peer granting few streams still gets everything asked of it, on each protocol: opens
//! wait their turn for credit rather than failing.

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

#[kio::test]
async fn one_stream_credit_serves_every_backlogged_group() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher.create_broadcast("live").unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
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
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
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
			// Both tracks rank alike, so the order across them is the line's arrival order.
			let mut received = Vec::new();
			while received.len() < 4 {
				let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
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

/// A relay's upstream session out of request credit waits for it rather than refusing: a
/// subscription past the publisher's credit is served once an earlier one leaves.
#[kio::test]
async fn a_subscription_past_the_credit_is_served_once_credit_returns() {
	// Room for the bidirectional streams the session holds before the second subscription:
	// the second then waits for one of the first's.
	for (version, credit) in [("moq-lite-07-wip", 4), ("moq-transport-22", 3)] {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let viewer = origin(2);
		let broadcast = publisher.create_broadcast("live").unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let first = broadcast.create_track("first", info.clone()).unwrap();
		let second = broadcast.create_track("second", info).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_bi_streams = Some(credit);
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
		let prefs =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(30)));
		let held = remote.track("first").unwrap().subscribe(prefs.clone()).await.unwrap();
		let mut waiting = std::pin::pin!(remote.track("second").unwrap().subscribe(prefs));
		assert!(
			kio::clock::timeout(Duration::from_secs(5), waiting.as_mut()).await.is_err(),
			"{version}: the second subscription had no credit to wait for"
		);

		drop(held);
		drop(first);
		let mut subscriber = kio::clock::timeout(Duration::from_secs(60), waiting)
			.await
			.unwrap_or_else(|_| panic!("{version}: the waiting subscription never got credit"))
			.unwrap_or_else(|err| panic!("{version}: the waiting subscription failed: {err}"));
		let mut group = second.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		group.finish().unwrap();
		let group = kio::clock::timeout(Duration::from_secs(5), subscriber.recv_group())
			.await
			.unwrap_or_else(|_| panic!("{version}: no group after credit returned"))
			.unwrap()
			.unwrap();
		assert_eq!(group.sequence, 0, "{version}");
	}
}

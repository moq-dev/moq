#![cfg(feature = "async")]

//! A relay's subscriber asking from a time before the copy it joins is fed.
//!
//! The relay pulls the track over lite-07 for a reader of its own, from the oldest group
//! its budget keeps, so its copy starts at the live edge. A downstream peer then subscribes
//! from an earlier group, as an older relay resuming onto this one does, after the publisher
//! expired everything there. Its range adds nothing to what the copy already asks, so the
//! publisher never reports where a range from that time starts, and the group containing
//! it never arrives: the subscription must still start, at the oldest group the copy holds.

use std::time::Duration;

use moq_net::track::{Delay, Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version, origin};

use crate::route_change::{link, produce_origin, settle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// One group every 100ms, keyed by its start in milliseconds.
const STEP: u64 = 100;

fn write(track: &moq_net::track::Producer, key: u64) {
	let mut group = track
		.create_group(moq_net::group::Info { start: key })
		.unwrap()
		.unwrap();
	group
		.write_frame(Timestamp::from_millis(key).unwrap(), &b"x"[..])
		.unwrap();
	group.finish().unwrap();
}

/// Write the groups `keys` in real time.
async fn play(track: &moq_net::track::Producer, keys: std::ops::Range<u64>) {
	for key in keys.step_by(STEP as usize) {
		write(track, key);
		kio::clock::sleep(Duration::from_millis(STEP)).await;
	}
}

/// The groups a downstream subscriber over `version` reads when it asks from 500, long
/// expired at the publisher, while the relay's own reader already holds the copy.
async fn read_from_expired(version: &str) -> Vec<u64> {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let downstream = produce_origin(3);

	let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			Info::default()
				.with_timescale(Timescale::MILLI)
				.with_max_age(Some(Duration::from_millis(500))),
		)
		.unwrap();
	broadcast.announce().unwrap();
	play(&track, 0..1000).await;

	// The relay's own reader pulls the copy from the live edge.
	let _up = link("moq-lite-07-wip".parse().unwrap(), &publisher, &relay).await;
	let local = kio::clock::timeout(TIMEOUT, relay.consume().follow("live", None).unwrap().available())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	let _local = local
		.track("video")
		.unwrap()
		.subscribe(Subscription::default())
		.await
		.unwrap();
	play(&track, 1000..2000).await;

	let _down = link(version.parse::<Version>().unwrap(), &relay, &downstream).await;
	let remote = kio::clock::timeout(TIMEOUT, downstream.consume().follow("live", None).unwrap().available())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(Duration::from_secs(60)))
		.with_ranges([Range {
			start: Some(500),
			end: None,
		}]);
	let mut sub = remote.track("video").unwrap().subscribe(subscription).await.unwrap();
	play(&track, 2000..3000).await;
	settle().await;

	let mut read = Vec::new();
	while let Ok(Ok(Some(group))) = kio::clock::timeout(Duration::from_millis(1), sub.recv_group()).await {
		read.push(group.start);
	}
	read.sort_unstable();
	read
}

#[kio::test]
async fn an_older_start_on_a_shared_copy_is_fed() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let read = read_from_expired(version).await;
		// Everything from the oldest group the relay still holds, through the live edge.
		assert_eq!(
			read.last(),
			Some(&2900),
			"{version}: fed to the live edge, read {read:?}"
		);
		assert!(
			read.windows(2).all(|pair| pair[1] - pair[0] == STEP),
			"{version}: no gap once started, read {read:?}"
		);
	}
}

#![cfg(feature = "async")]

//! Relays forward their subscribers' delay: the most tolerant negotiated max, and the smallest stated min.

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin, track};

use crate::support::harness::{peer, spawn};

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

async fn budget(track: &mut track::Producer, millis: u64, version: Version) {
	kio::clock::timeout(Duration::from_secs(5), async {
		loop {
			if track
				.subscription()
				.is_some_and(|subscription| subscription.delay.max == Some(Duration::from_millis(millis)))
			{
				return;
			}
			track.subscription_changed().await.unwrap();
		}
	})
	.await
	.unwrap_or_else(|_| panic!("{version}: wanted {millis}ms, publisher has {:?}", track.subscription()));
}

#[kio::test]
async fn relay_forwards_the_most_tolerant_subscriber_max_age() {
	for draft in [18, 22] {
		let version: Version = format!("moq-transport-{draft}").parse().unwrap();
		let publisher = origin(1);
		let relay = origin(2);
		let viewer = origin(3);
		let _upstream = peer(version, &publisher, &relay).await;
		let _downstream = peer(version, &relay, &viewer).await;
		let broadcast = publisher.create_broadcast("live").unwrap();
		let mut source = broadcast
			.create_track(
				"catalog",
				track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let mut group = source.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"catalog").unwrap();
		group.finish().unwrap();
		broadcast.announce(Default::default()).unwrap();
		let consumer = viewer.consume();
		let remote = consumer.follow("live", None).unwrap().available().await.unwrap();
		let cache = remote.track("catalog").unwrap();
		let prefs = |millis| track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_millis(millis)));
		let mut short = cache.subscribe(prefs(250)).await.unwrap();
		budget(&mut source, 250, version).await;
		let tolerant = cache.subscribe(prefs(2000)).await.unwrap();
		budget(&mut source, 2000, version).await;
		short.update(prefs(10)).unwrap();
		// Drive the logical cursor so its serving copy observes the new preferences.
		let mut snapshot = short.recv_group().await.unwrap().unwrap();
		// Dropping the tolerant subscriber exposes the updated shorter budget, across both hops.
		drop(tolerant);
		budget(&mut source, 10, version).await;
		// A quiet catalog survives far longer than the budget because it has no successor.
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(snapshot.sequence, 0, "{version}");
		assert_eq!(
			snapshot.read_frame().await.unwrap().unwrap().payload,
			b"catalog".as_slice()
		);
	}
}

async fn min_delay(track: &mut track::Producer, min: Option<Duration>) {
	kio::clock::timeout(Duration::from_secs(5), async {
		loop {
			if track.subscription().is_some_and(|subscription| subscription.delay.min == min) {
				return;
			}
			track.subscription_changed().await.unwrap();
		}
	})
	.await
	.unwrap_or_else(|_| panic!("wanted min {min:?}, publisher has {:?}", track.subscription()));
}

/// A lite-07 relay forwards the smallest stated min delay upstream, and a lite-06 subscriber,
/// whose wire cannot state one, never pulls it to zero.
#[kio::test]
async fn relay_forwards_the_smallest_stated_min_delay() {
	let lite07: Version = "moq-lite-07-wip".parse().unwrap();
	let lite06: Version = "moq-lite-06".parse().unwrap();
	let publisher = origin(1);
	let relay = origin(2);
	let modern = origin(3);
	let legacy = origin(4);
	let _upstream = peer(lite07, &publisher, &relay).await;
	let _modern = peer(lite07, &relay, &modern).await;
	let _legacy = peer(lite06, &relay, &legacy).await;
	let broadcast = publisher.create_broadcast("live").unwrap();
	let mut source = broadcast
		.create_track("catalog", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	let mut group = source.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"catalog").unwrap();
	group.finish().unwrap();
	broadcast.announce(Default::default()).unwrap();

	let cache = |origin: &origin::Producer| {
		let origin = origin.clone();
		async move {
			let remote = origin.consume().follow("live", None).unwrap().available().await.unwrap();
			remote.track("catalog").unwrap()
		}
	};
	let prefs = |min: Option<u64>| {
		track::Subscription::default().with_delay(track::Delay::default().with_min(min.map(Duration::from_millis)))
	};

	let unstated = cache(&legacy).await.subscribe(prefs(Some(0))).await.unwrap();
	min_delay(&mut source, None).await;
	let modern = cache(&modern).await;
	let slow = modern.subscribe(prefs(Some(500))).await.unwrap();
	min_delay(&mut source, Some(Duration::from_millis(500))).await;
	let fast = modern.subscribe(prefs(Some(100))).await.unwrap();
	min_delay(&mut source, Some(Duration::from_millis(100))).await;
	drop(fast);
	min_delay(&mut source, Some(Duration::from_millis(500))).await;
	drop(slow);
	min_delay(&mut source, None).await;
	drop(unstated);
}

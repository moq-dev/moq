#![cfg(feature = "async")]

//! Relays forward their subscribers' delay: the most tolerant negotiated max, and the largest stated min.

use std::time::Duration;

use moq_net::{Version, origin, track};

use crate::support::AppendNext;
use crate::support::AtStart;
use crate::support::harness::{peer, spawn};

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (origin, driver) = origin::Producer::new(config);
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

/// moq-transport carries no subscriber budget, so only moq-lite forwards one.
#[kio::test]
async fn relay_forwards_the_most_tolerant_subscriber_max_age() {
	for alpn in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = alpn.parse().unwrap();
		let publisher = origin(1);
		let relay = origin(2);
		let viewer = origin(3);
		let _upstream = peer(version, &publisher, &relay).await;
		let _downstream = peer(version, &relay, &viewer).await;
		let broadcast = publisher
			.route("live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let mut source = broadcast
			.broadcast()
			.create_track(
				"catalog",
				track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let mut group = source.append_next().unwrap();
		group.write_frame(group.at_start(), b"catalog").unwrap();
		group.finish().unwrap();
		broadcast.announce().unwrap();
		let consumer = viewer.consume();
		let remote = consumer
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let cache = remote.track("catalog").unwrap();
		let prefs = |millis| {
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_millis(millis)))
		};
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
		assert_eq!(snapshot.start, 0, "{version}");
		assert_eq!(
			snapshot.read_frame().await.unwrap().unwrap().payload,
			b"catalog".as_slice()
		);
	}
}

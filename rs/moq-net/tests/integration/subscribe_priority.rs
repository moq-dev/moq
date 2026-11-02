#![cfg(feature = "async")]

//! One upstream subscription carries the aggregate priority of the subscribers it serves,
//! and follows it as they come and go, directly and through a relay.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Subscription};
use moq_net::{Hop, Timescale, Version, origin};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"];

const LOW: i8 = -100;
const HIGH: i8 = 100;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Wait until the subscription `track` serves asks for `want`.
async fn priority(track: &moq_net::track::Producer, want: i8) -> Result<(), String> {
	let mut track = track.clone();
	let reached = kio::clock::timeout(TIMEOUT, async {
		while track
			.subscription()
			.is_none_or(|subscription| subscription.priority != want)
		{
			track.subscription_changed().await.expect("track open");
		}
	})
	.await;
	reached.map_err(|_| {
		format!(
			"priority stuck at {:?}, wanted {want}",
			track.subscription().map(|s| s.priority)
		)
	})
}

/// A low priority subscriber, joined by a high priority one that then leaves: the
/// publisher sees the priority rise and fall back.
async fn follows(version: Version, relay: bool) -> Result<(), String> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("video", Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let subscriber = produce_origin(3);
	let mut pairs = Vec::new();
	if relay {
		pairs.push(peer(version, &publisher, &relay_origin).await);
		pairs.push(peer(version, &relay_origin, &subscriber).await);
	} else {
		pairs.push(peer(version, &publisher, &subscriber).await);
	}

	let consumer = subscriber.consume();
	let remote = kio::clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let subscribe = |priority: i8| {
		let track = remote.track("video").unwrap();
		async move {
			kio::clock::timeout(
				TIMEOUT,
				track.subscribe(Subscription::default().with_priority(priority)),
			)
			.await
			.expect("subscribe timeout")
			.expect("subscribe")
		}
	};

	let _low = subscribe(LOW).await;
	priority(&track, LOW)
		.await
		.map_err(|err| format!("first subscriber: {err}"))?;

	let high = subscribe(HIGH).await;
	priority(&track, HIGH).await.map_err(|err| format!("joined: {err}"))?;

	drop(high);
	priority(&track, LOW).await.map_err(|err| format!("left: {err}"))?;
	Ok(())
}

#[kio::test]
async fn upstream_priority_follows_the_subscribers() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		for relay in [false, true] {
			if let Err(err) = follows(version.parse().unwrap(), relay).await {
				failures.push(format!("{version} relay={relay}: {err}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

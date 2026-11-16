#![cfg(feature = "async")]

//! A live join stating a Min Age starts that far behind the live edge, through relays that
//! hold nothing yet.

use std::time::Duration;

use moq_net::{Timestamp, Version, cache, origin, track};

use crate::support::harness::{peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin.with_proxy(true)
}

/// Append the 500 ms group starting at `start`, presenting there.
fn append(source: &track::Producer, start: u64) {
	let mut group = source.append_group(start).unwrap();
	group
		.write_frame(Timestamp::from_millis(start).unwrap(), &b"x"[..])
		.unwrap();
	group.close().unwrap();
}

/// A viewer joining through two relays that hold nothing gets the four groups behind the
/// live group its 2 s Min Age asks for, oldest first, then plays on to live with no gap.
#[kio::test]
async fn a_cold_relay_chain_reaches_back_by_min_age() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let edge = origin(2);
	let inner = origin(3);
	let viewer = origin(4);
	let _a = peer(version, &publisher, &inner).await;
	let _b = peer(version, &inner, &edge).await;
	let _c = peer(version, &edge, &viewer).await;

	let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let source = broadcast.broadcast().create_track("video", info).unwrap();
	for start in (0..=5000).step_by(500) {
		append(&source, start);
	}
	broadcast.announce().unwrap();

	let joining = track::Subscription::default()
		.with_ranges([track::Range::live()])
		.with_order(track::Order::Ascending)
		.with_delay(
			track::Delay::default()
				.with_min(Duration::from_secs(2))
				.with_max(Duration::from_secs(30)),
		);
	kio::clock::timeout(TIMEOUT, async {
		let remote = viewer.consume().follow("live", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		let mut subscriber = remote.track("video").unwrap().subscribe(joining).await.unwrap();
		let mut starts = Vec::new();
		while starts.len() < 5 {
			starts.push(subscriber.recv_group().await.unwrap().expect("a group").start);
		}
		assert_eq!(starts, [3000, 3500, 4000, 4500, 5000]);
		append(&source, 5500);
		assert_eq!(subscriber.recv_group().await.unwrap().expect("a group").start, 5500);
	})
	.await
	.expect("the viewer read its reach back");
}

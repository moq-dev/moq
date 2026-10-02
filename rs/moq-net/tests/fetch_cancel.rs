//! A FETCH nobody wants anymore is cancelled all the way to the publisher.
//!
//! Lite has no FETCH_OK, so until a publisher answers, each relay on the path holds
//! a FETCH stream and the group request behind it. When the reader gives up, every
//! hop must let go; otherwise a publisher that never answers pins them for good.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, origin, track};
use support::harness::peer;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// `relays` relays between the publisher and the reader.
async fn abandoned_fetch_reaches_the_publisher(version: &str, relays: u64) {
	let version: Version = version.parse().unwrap();
	let nodes: Vec<_> = (1..=relays + 1).map(produce_origin).collect();
	let mut _pairs = Vec::new();
	for pair in nodes.windows(2) {
		_pairs.push(peer(version, &pair[0], &pair[1]).await);
	}

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();
	tokio::time::sleep(Duration::from_secs(1)).await;

	let consumer = nodes[relays as usize]
		.consume()
		.request_broadcast("room")
		.await
		.unwrap();
	let mut waiting = Box::pin(consumer.track("video").unwrap().fetch_group(0, None));

	// Answer the TRACK_INFO, but hold the FETCH unanswered.
	let request = tokio::select! {
		request = dynamic.requested_track() => request.expect("the track request reaches the publisher"),
		_ = &mut waiting => panic!("nothing answered the track"),
	};
	let groups = request.dynamic();
	let _track = request.accept(track::Info::default());
	let request = tokio::select! {
		request = groups.requested_group() => request.expect("the fetch reaches the publisher"),
		_ = &mut waiting => panic!("nothing answered the fetch"),
	};

	// The reader gives up.
	drop(waiting);
	tokio::time::timeout(Duration::from_secs(5), kio::wait(|waiter| request.poll_unused(waiter)))
		.await
		.unwrap_or_else(|_| panic!("{version:?} over {relays} relays: the publisher's fetch is still wanted"));
}

#[tokio::test(start_paused = true)]
async fn abandoned_fetch_reaches_the_publisher_lite05() {
	for relays in [1, 2] {
		abandoned_fetch_reaches_the_publisher("moq-lite-05", relays).await;
	}
}

#[tokio::test(start_paused = true)]
async fn abandoned_fetch_reaches_the_publisher_lite06() {
	for relays in [1, 2] {
		abandoned_fetch_reaches_the_publisher("moq-lite-06", relays).await;
	}
}

//! A track request nobody wants anymore is cancelled all the way to the publisher.
//!
//! Until a publisher answers TRACK_INFO, each relay on the path holds a task, a TRACK
//! stream, and the track for the request. When the reader gives up, every hop must
//! let go; otherwise a peer that never answers (one still hunting for a route, or
//! with its stream credit spent) pins them for good, and a relay retrying across
//! routes accumulates them until it runs out of memory.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, origin};
use support::harness::peer;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// `relays` relays between the publisher and the reader.
async fn abandoned_request_reaches_the_publisher(version: &str, relays: u64) {
	let version: Version = version.parse().unwrap();
	let nodes: Vec<_> = (1..=relays + 1).map(produce_origin).collect();
	let mut _pairs = Vec::new();
	for pair in nodes.windows(2) {
		_pairs.push(peer(version, &pair[0], &pair[1]).await);
	}

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();
	moq_net_sim::sleep(Duration::from_secs(1)).await;

	let consumer = nodes[relays as usize]
		.consume()
		.request_broadcast("room")
		.await
		.unwrap();
	let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
	let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the request reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the track"),
	};

	// The publisher never answers; the reader gives up.
	drop(waiting);
	moq_net_sim::timeout(
		Duration::from_secs(5),
		kio::wait(|waiter| request.demand().poll_unused(waiter)),
	)
	.await
	.unwrap_or_else(|_| panic!("{version:?} over {relays} relays: the publisher's request is still wanted"))
	.expect("the pending request is still open");
}

#[moq_net_sim::test]
async fn abandoned_request_reaches_the_publisher_lite05() {
	for relays in [1, 2] {
		abandoned_request_reaches_the_publisher("moq-lite-05", relays).await;
	}
}

#[moq_net_sim::test]
async fn abandoned_request_reaches_the_publisher_lite06() {
	for relays in [1, 2] {
		abandoned_request_reaches_the_publisher("moq-lite-06", relays).await;
	}
}

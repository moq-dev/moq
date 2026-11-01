#![cfg(feature = "async")]

//! A track request nobody wants anymore is cancelled all the way to the publisher.
//!
//! Until a publisher answers TRACK_INFO, each relay on the path holds a task, a TRACK
//! stream, and the track for the request. When the reader gives up, every hop must
//! let go; otherwise a peer that never answers (one still hunting for a route, or
//! with its stream credit spent) pins them for good, and a relay retrying across
//! routes accumulates them until it runs out of memory.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Version, origin};
use support::harness::{MockConnectOptions, connect_mock, peer};

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
	let mut dynamic = broadcast.requests();
	broadcast.announce(Default::default()).unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;

	let consumer = nodes[relays as usize].consume().request("room", None).await.unwrap();
	let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
	let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut waiting).await {
		futures::future::Either::Left((request, _)) => request.expect("the request reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered the track"),
	};

	// The publisher never answers; the reader gives up.
	drop(waiting);
	kio::clock::timeout(Duration::from_secs(5), crate::support::unwanted(request.demand()))
		.await
		.unwrap_or_else(|_| panic!("{version:?} over {relays} relays: the publisher's request is still wanted"))
		.expect("the pending request is still open");
}

#[kio::test]
async fn abandoned_request_reaches_the_publisher_lite05() {
	for relays in [1, 2] {
		abandoned_request_reaches_the_publisher("moq-lite-05", relays).await;
	}
}

#[kio::test]
async fn abandoned_request_reaches_the_publisher_lite06() {
	for relays in [1, 2] {
		abandoned_request_reaches_the_publisher("moq-lite-06", relays).await;
	}
}

/// Withdrawing a broadcast releases its TRACK interest even while the reader keeps
/// the local query. An answered query needs no wire resources for its immutable info.
async fn withdrawn_broadcast_releases_track_info(version: &str, answered: bool, explicit: bool) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let reader = produce_origin(2);
	let broadcast = publisher.create_broadcast("room").unwrap();
	let mut dynamic = broadcast.requests();
	let mut route = origin::Route::default();
	if explicit {
		route.epoch = Some("019a1234-5678-7000-8000-000000000001".parse().unwrap());
	}
	broadcast.announce(route).unwrap();
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(reader.clone());
	let pair = connect_mock(options).await;
	let remote = reader
		.consume()
		.follow("room", None)
		.unwrap()
		.available()
		.await
		.unwrap();
	let mut query = remote.track("video").unwrap().info();
	let request = match futures::future::select(std::pin::pin!(dynamic.requested_track()), &mut query).await {
		futures::future::Either::Left((request, _)) => request.expect("TRACK reaches the publisher"),
		futures::future::Either::Right(_) => panic!("nothing answered TRACK"),
	};
	let demand = request.demand();
	let _track = if answered {
		let track = request.accept(moq_net::track::Info::default());
		(&mut query).await.expect("TRACK_INFO answers");
		Some(track)
	} else {
		// A dynamic handler may retain its unanswered request after broadcast close.
		// Its remote TRACK interest must still leave.
		None
	};
	let finished = pair.client_transport.finished_streams();
	broadcast.close();
	kio::clock::timeout(Duration::from_secs(5), crate::support::unwanted(demand))
		.await
		.unwrap_or_else(|_| panic!("{version}, answered={answered}, explicit={explicit}: TRACK outlived its broadcast"))
		.expect("the publisher still owns its track");
	if answered {
		assert!(
			pair.client_transport.finished_streams() > finished,
			"TRACK was not FIN'd"
		);
		(&mut query)
			.await
			.expect("withdrawal preserves the answered local query");
	}
	drop(query);
}

#[kio::test]
async fn a_pending_track_info_ends_with_its_broadcast() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		for explicit in [false, true] {
			withdrawn_broadcast_releases_track_info(version, false, explicit).await;
		}
	}
}

#[kio::test]
async fn an_answered_track_info_ends_with_its_broadcast() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		for explicit in [false, true] {
			withdrawn_broadcast_releases_track_info(version, true, explicit).await;
		}
	}
}

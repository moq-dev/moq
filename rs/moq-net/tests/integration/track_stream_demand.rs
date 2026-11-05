#![cfg(feature = "async")]

//! A subscription's demand holds across its TRACK and SUBSCRIBE streams.
//!
//! On moq-lite 05 and later a subscriber learns a track's properties over a TRACK stream
//! before it subscribes. The publisher counts that stream as interest until the subscriber
//! closes it, and the subscriber keeps it open until its SUBSCRIBE is answered, so a
//! publisher watching its demand sees one `used` edge for a viewer, not `used`, `unused`,
//! `used` with a round trip per hop in between.

use crate::support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Hop, Timestamp, Version, origin};
use support::{
	harness::{MockConnectOptions, MockPair, connect_mock},
	mock::MockSession,
};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);
const LATENCY: Duration = Duration::from_millis(10);
const VERSIONS: [&str; 3] = ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];
/// How long the publisher takes to start a track once asked, like a transcoder spinning up.
const STARTUP: Duration = Duration::from_millis(5);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// `subscriber` reads what `publisher` serves, over a link with [`LATENCY`] each way.
async fn link(version: Version, publisher: &origin::Producer, subscriber: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	options.latency = LATENCY;
	connect_mock(options).await
}

/// Every demand edge a `track::Demand` reports from here on, `true` for wanted.
macro_rules! record_track {
	($demand:expr) => {{
		let edges = Rc::new(RefCell::new(Vec::new()));
		let log = edges.clone();
		let mut demand = $demand;
		drop(kio::clock::sim::spawn(async move {
			let mut was = false;
			while let Ok(wanted) = demand.wanted_changed().await {
				if wanted.is_some() != was {
					was = wanted.is_some();
					log.borrow_mut().push(was);
				}
			}
		}));
		edges
	}};
}

/// A reader subscribes through `relays` relays and stays: the publisher, which starts
/// the track on demand, is asked for it once, and its track sees exactly one `used` edge,
/// then `unused` once the reader leaves.
///
/// With `reorder`, the reader's SUBSCRIBE stream reaches the first hop well after its
/// TRACK stream's data, as QUIC may deliver them, so a hop that let go of the TRACK
/// before its SUBSCRIBE arrived would drop its demand in between.
async fn one_used_edge(version: &str, relays: usize, reorder: bool) {
	let version: Version = version.parse().unwrap();
	let label = format!("{version} over {relays} relays, reorder={reorder}");

	let nodes: Vec<_> = (1..=relays as u64 + 2).map(produce_origin).collect();
	let mut links = Vec::new();
	for pair in nodes.windows(2) {
		links.push(link(version, &pair[0], &pair[1]).await);
	}
	let reader: &MockSession = &links.last().unwrap().client_transport;

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let mut dynamic = broadcast.requests();
	broadcast.announce(Default::default()).unwrap();
	// Serve every request for the track, keeping what it started.
	let requests = Rc::new(RefCell::new(Vec::new()));
	let track_edges = Rc::new(RefCell::new(None));
	let served = (requests.clone(), track_edges.clone());
	let publisher = kio::clock::sim::spawn(async move {
		while let Ok(request) = dynamic.requested_track().await {
			served
				.1
				.borrow_mut()
				.get_or_insert_with(|| record_track!(request.demand()));
			kio::clock::sleep(STARTUP).await;
			let track = request.accept(timed());
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"key".as_ref()).unwrap();
			served.0.borrow_mut().push((track, group));
		}
	});

	let consumer = nodes.last().unwrap().consume();
	let remote = consumer.follow("room", None).unwrap().available().await.unwrap();

	let subscribing = remote.track("video").unwrap().subscribe(None);
	if reorder {
		// The TRACK stream opens at once; the SUBSCRIBE only once TRACK_INFO is back.
		kio::clock::sleep(Duration::from_millis(1)).await;
		reader.hold_bidis();
	}
	let mut subscriber = kio::clock::timeout(TIMEOUT, subscribing)
		.await
		.unwrap_or_else(|_| panic!("{label}: subscribe stalled"))
		.unwrap_or_else(|err| panic!("{label}: subscribe failed: {err}"));
	if reorder {
		kio::clock::sleep(LATENCY * 20).await;
		reader.release_bidis();
	}

	let mut received = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{label}: no group arrived"))
		.unwrap()
		.expect("the track is live");
	received.read_frame().await.unwrap().expect("the group has a frame");

	// Stay subscribed while every hop settles.
	kio::clock::sleep(Duration::from_secs(2)).await;
	assert_eq!(requests.borrow().len(), 1, "{label}: asked for the track again");
	let track_edges = track_edges.borrow().clone().expect("the track was requested");
	assert_eq!(*track_edges.borrow(), [true], "{label}: track demand");

	// Holding the TRACK stream must not outlive the reader.
	drop(received);
	drop(subscriber);
	let track = requests.borrow()[0].0.clone();
	kio::clock::timeout(TIMEOUT, crate::support::unwanted(track.demand()))
		.await
		.unwrap_or_else(|_| panic!("{label}: demand outlived the reader"))
		.unwrap();
	kio::clock::sleep(Duration::from_millis(100)).await;
	assert_eq!(*track_edges.borrow(), [true, false], "{label}: track demand");
	publisher.abort();
}

#[kio::test]
async fn direct() {
	for version in VERSIONS {
		one_used_edge(version, 0, false).await;
		one_used_edge(version, 0, true).await;
	}
}

#[kio::test]
async fn through_a_relay() {
	for version in VERSIONS {
		one_used_edge(version, 1, false).await;
		one_used_edge(version, 1, true).await;
	}
}

/// A bare track handle is no demand, through `relays` relays: holding one asks the
/// publisher for nothing, a query on it does until it is dropped, and once a subscription
/// made from it ends, the publisher reads unused while the handle is still held.
async fn bare_handle_is_lazy(version: &str, relays: usize) {
	let version: Version = version.parse().unwrap();
	let label = format!("{version} over {relays} relays");

	let nodes: Vec<_> = (1..=relays as u64 + 2).map(produce_origin).collect();
	let mut links = Vec::new();
	for pair in nodes.windows(2) {
		links.push(link(version, &pair[0], &pair[1]).await);
	}

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let mut dynamic = broadcast.requests();
	broadcast.announce(Default::default()).unwrap();
	let requests = Rc::new(RefCell::new(Vec::new()));
	let served = requests.clone();
	let publisher = kio::clock::sim::spawn(async move {
		while let Ok(request) = dynamic.requested_track().await {
			let track = request.accept(timed());
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"key".as_ref()).unwrap();
			served.borrow_mut().push((track, group));
		}
	});

	let consumer = nodes.last().unwrap().consume();
	let remote = consumer.follow("room", None).unwrap().available().await.unwrap();

	let handle = remote.track("video").unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;
	assert!(
		requests.borrow().is_empty(),
		"{label}: a bare handle asked for the track"
	);

	// A held query reaches the publisher, and holds the track there until dropped.
	let mut query = handle.info();
	kio::clock::timeout(TIMEOUT, &mut query)
		.await
		.unwrap_or_else(|_| panic!("{label}: query stalled"))
		.unwrap_or_else(|err| panic!("{label}: query failed: {err}"));
	kio::clock::sleep(Duration::from_secs(1)).await;
	assert_eq!(
		requests.borrow().len(),
		1,
		"{label}: the query never reached the publisher"
	);
	let track = requests.borrow()[0].0.clone();
	assert!(track.demand().wanted().is_some(), "{label}: a held query let go");
	drop(query);
	kio::clock::timeout(TIMEOUT, crate::support::unwanted(track.demand()))
		.await
		.unwrap_or_else(|_| panic!("{label}: a dropped query is still demand"))
		.unwrap();

	// A subscription ending leaves the held handle unread at both ends.
	let mut subscriber = kio::clock::timeout(TIMEOUT, handle.subscribe(None))
		.await
		.unwrap_or_else(|_| panic!("{label}: subscribe stalled"))
		.unwrap_or_else(|err| panic!("{label}: subscribe failed: {err}"));
	kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{label}: no group arrived"))
		.unwrap()
		.expect("the track is live");
	assert!(
		track.demand().wanted().is_some(),
		"{label}: the subscription is not demand"
	);
	drop(subscriber);
	kio::clock::timeout(TIMEOUT, crate::support::unwanted(track.demand()))
		.await
		.unwrap_or_else(|_| panic!("{label}: the publisher still reads the track as used"))
		.unwrap();
	drop(handle);
	publisher.abort();
}

#[kio::test]
async fn bare_handle_is_lazy_direct() {
	for version in VERSIONS {
		bare_handle_is_lazy(version, 0).await;
	}
}

#[kio::test]
async fn bare_handle_is_lazy_through_a_relay() {
	for version in VERSIONS {
		bare_handle_is_lazy(version, 1).await;
	}
}

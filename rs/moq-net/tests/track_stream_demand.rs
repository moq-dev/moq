//! A subscription's demand holds across its TRACK and SUBSCRIBE streams.
//!
//! On moq-lite 05 and later a subscriber learns a track's properties over a TRACK stream
//! before it subscribes. The publisher counts that stream as interest until the subscriber
//! closes it, and the subscriber keeps it open until its SUBSCRIBE is answered, so a
//! publisher watching its demand sees one `used` edge for a viewer, not `used`, `unused`,
//! `used` with a round trip per hop in between.

mod support;

use std::{cell::RefCell, rc::Rc, time::Duration};

use moq_net::{Hop, Timestamp, Version, origin};
use support::{
	harness::{MockConnectOptions, MockPair, connect_mock},
	mock::MockSession,
};

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

/// Every demand edge a `broadcast::Demand` or `track::Demand` reports from here on,
/// `true` for used.
macro_rules! record {
	($demand:expr) => {{
		let edges = Rc::new(RefCell::new(Vec::new()));
		let log = edges.clone();
		let demand = $demand;
		drop(moq_net_sim::spawn(async move {
			while demand.used().await.is_ok() {
				log.borrow_mut().push(true);
				if demand.unused().await.is_err() {
					break;
				}
				log.borrow_mut().push(false);
			}
		}));
		edges
	}};
}

/// A reader subscribes through `relays` relays and stays: the publisher, which starts
/// the track on demand, is asked for it once, and its broadcast and track each see exactly
/// one `used` edge, then `unused` once the reader leaves.
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
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();
	let broadcast_edges = record!(broadcast.demand());

	// Serve every request for the track, keeping what it started.
	let requests = Rc::new(RefCell::new(Vec::new()));
	let track_edges = Rc::new(RefCell::new(None));
	let served = (requests.clone(), track_edges.clone());
	let publisher = moq_net_sim::spawn(async move {
		while let Ok(request) = dynamic.requested_track().await {
			served.1.borrow_mut().get_or_insert_with(|| record!(request.demand()));
			moq_net_sim::sleep(STARTUP).await;
			let track = request.accept(None);
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"key".as_ref()).unwrap();
			served.0.borrow_mut().push((track, group));
		}
	});

	let consumer = nodes.last().unwrap().consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("room"))
		.await
		.unwrap_or_else(|_| panic!("{label}: never announced"))
		.unwrap();
	let remote = consumer.request_broadcast("room", None).await.unwrap();

	let subscribing = remote.track("video").unwrap().subscribe(None);
	if reorder {
		// The TRACK stream opens at once; the SUBSCRIBE only once TRACK_INFO is back.
		moq_net_sim::sleep(Duration::from_millis(1)).await;
		reader.hold_bidis();
	}
	let mut subscriber = moq_net_sim::timeout(TIMEOUT, subscribing)
		.await
		.unwrap_or_else(|_| panic!("{label}: subscribe stalled"))
		.unwrap_or_else(|err| panic!("{label}: subscribe failed: {err}"));
	if reorder {
		moq_net_sim::sleep(LATENCY * 20).await;
		reader.release_bidis();
	}

	let mut received = moq_net_sim::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{label}: no group arrived"))
		.unwrap()
		.expect("the track is live");
	received.read_frame().await.unwrap().expect("the group has a frame");

	// Stay subscribed while every hop settles.
	moq_net_sim::sleep(Duration::from_secs(2)).await;
	assert_eq!(requests.borrow().len(), 1, "{label}: asked for the track again");
	let track_edges = track_edges.borrow().clone().expect("the track was requested");
	assert_eq!(*broadcast_edges.borrow(), [true], "{label}: broadcast demand");
	assert_eq!(*track_edges.borrow(), [true], "{label}: track demand");

	// Holding the TRACK stream must not outlive the reader.
	drop(received);
	drop(subscriber);
	let track = requests.borrow()[0].0.clone();
	moq_net_sim::timeout(TIMEOUT, track.demand().unused())
		.await
		.unwrap_or_else(|_| panic!("{label}: demand outlived the reader"))
		.unwrap();
	moq_net_sim::sleep(Duration::from_millis(100)).await;
	assert_eq!(*broadcast_edges.borrow(), [true, false], "{label}: broadcast demand");
	assert_eq!(*track_edges.borrow(), [true, false], "{label}: track demand");
	publisher.abort();
}

#[moq_net_sim::test]
async fn direct() {
	for version in VERSIONS {
		one_used_edge(version, 0, false).await;
		one_used_edge(version, 0, true).await;
	}
}

#[moq_net_sim::test]
async fn through_a_relay() {
	for version in VERSIONS {
		one_used_edge(version, 1, false).await;
		one_used_edge(version, 1, true).await;
	}
}

/// Held TRACK streams count against the server's subscription cap, and each SUBSCRIBE
/// takes over its TRACK's place: a client at the cap still subscribes to every track it
/// holds without a demand gap, and one more TRACK closes the session.
#[moq_net_sim::test]
async fn held_tracks_share_the_cap_with_their_subscriptions() {
	const CAP: usize = 3;

	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let server = produce_origin(1);
		let client = produce_origin(2);

		let broadcast = server.create_broadcast("room").unwrap();
		let mut tracks = Vec::new();
		for i in 0..=CAP {
			let track = broadcast.create_track(format!("t{i}"), None).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"key".as_ref()).unwrap();
			let edges = record!(track.demand());
			tracks.push((track, group, edges));
		}
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(server.consume());
		options.client_subscribe = Some(client.clone());
		options.latency = LATENCY;
		let mut limits = moq_net::session::Limits::default();
		limits.subscriptions = CAP;
		options.server_limits = Some(limits);
		let pair = connect_mock(options).await;

		let remote = moq_net_sim::timeout(TIMEOUT, client.consume().routed_broadcast("room"))
			.await
			.expect("announce timeout")
			.unwrap();

		// A held handle keeps each track wanted, and so its TRACK stream open.
		let mut held = Vec::new();
		for i in 0..CAP {
			let track = remote.track(&format!("t{i}")).unwrap();
			moq_net_sim::timeout(TIMEOUT, track.query())
				.await
				.unwrap_or_else(|_| panic!("{version}: t{i} info stalled"))
				.unwrap_or_else(|err| panic!("{version}: t{i} info refused: {err}"));
			held.push(track);
		}
		moq_net_sim::sleep(Duration::from_secs(1)).await;
		for (i, (.., edges)) in tracks.iter().enumerate().take(CAP) {
			assert_eq!(*edges.borrow(), [true], "{version}: t{i} is not held");
		}

		let mut subscribers = Vec::new();
		for (i, track) in held.iter().enumerate() {
			let mut subscriber = moq_net_sim::timeout(TIMEOUT, track.subscribe(None))
				.await
				.unwrap_or_else(|_| panic!("{version}: t{i} subscribe stalled"))
				.unwrap_or_else(|err| panic!("{version}: t{i} refused at the cap: {err}"));
			moq_net_sim::timeout(TIMEOUT, subscriber.recv_group())
				.await
				.unwrap_or_else(|_| panic!("{version}: t{i} delivered nothing"))
				.unwrap()
				.expect("the track is live");
			subscribers.push(subscriber);
		}
		moq_net_sim::sleep(Duration::from_secs(1)).await;
		for (i, (.., edges)) in tracks.iter().enumerate().take(CAP) {
			assert_eq!(*edges.borrow(), [true], "{version}: t{i} demand");
		}

		// The subscriptions alone fill the cap now.
		let refused = moq_net_sim::timeout(TIMEOUT, remote.track(&format!("t{CAP}")).unwrap().query())
			.await
			.expect("info timeout");
		assert!(refused.is_err(), "{version}: one more TRACK was admitted");
		let err = moq_net_sim::timeout(TIMEOUT, pair.client.closed())
			.await
			.unwrap_or_else(|_| panic!("{version}: the session stayed open"));
		assert!(
			matches!(err, moq_net::Error::Session(moq_net::SessionError::TooManyRequests)),
			"{version}: closed with {err}"
		);
	}
}

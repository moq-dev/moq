#![cfg(feature = "async")]

//! On moq-lite 05 and later a subscriber opens its TRACK and SUBSCRIBE streams together,
//! so first data arrives one round trip after asking, per hop, rather than two.
//!
//! QUIC does not order streams, so either may reach the publisher first, or the SUBSCRIBE a
//! round trip after the TRACK, as from a subscriber that waited for TRACK_INFO. Whichever
//! it is, the reader gets the track, including when the SUBSCRIBE lands first and the
//! publisher's groups beat the TRACK_INFO they are read with. Lite-03/04 carry no TRACK
//! stream and are unchanged.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const LATENCY: Duration = Duration::from_millis(10);
const RTT: Duration = Duration::from_millis(20);
const VERSIONS: [&str; 5] = [
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
];

/// How the reader's TRACK and SUBSCRIBE reach the first hop.
#[derive(Debug, Clone, Copy)]
enum Arrival {
	/// As sent: together.
	Together,
	/// The SUBSCRIBE a round trip after the TRACK, once its TRACK_INFO is back.
	Serial,
	/// The SUBSCRIBE first, so the publisher's groups head back before its TRACK_INFO.
	SubscribeFirst,
}

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

/// A reader subscribes through `relays` relays and reads a cached group, then a live one.
/// Returns how long the cached group's first frame took.
async fn first_frame(version: &str, relays: usize, arrival: Arrival) -> Duration {
	let version: Version = version.parse().unwrap();
	let label = format!("{version} over {relays} relays, {arrival:?}");

	let nodes: Vec<_> = (1..=relays as u64 + 2).map(produce_origin).collect();
	let mut links = Vec::new();
	for pair in nodes.windows(2) {
		links.push(link(version, &pair[0], &pair[1]).await);
	}
	let reader = &links.last().unwrap().client_transport;

	let broadcast = nodes[0].create_broadcast("room").unwrap();
	let track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let mut cached = track.append_group().unwrap();
	cached.write_frame(Timestamp::ZERO, b"cached".as_ref()).unwrap();
	cached.finish().unwrap();
	broadcast.announce(Default::default()).unwrap();

	let consumer = nodes.last().unwrap().consume();
	let remote = consumer.follow("room", None).unwrap().available().await.unwrap();

	if !matches!(arrival, Arrival::Together) {
		reader.hold_bidis();
	}
	let start = moq_clock::now();
	let subscribing = moq_clock::sim::spawn(remote.track("video").unwrap().subscribe(None));
	match arrival {
		Arrival::Together => {}
		Arrival::Serial => {
			moq_clock::sleep(Duration::from_millis(1)).await;
			reader.release_oldest_bidi();
			moq_clock::sleep(RTT).await;
			reader.release_bidis();
		}
		Arrival::SubscribeFirst => {
			moq_clock::sleep(Duration::from_millis(1)).await;
			reader.release_bidis_reversed();
		}
	}
	let mut subscriber = moq_clock::timeout(TIMEOUT, subscribing)
		.await
		.unwrap_or_else(|_| panic!("{label}: subscribe stalled"))
		.unwrap()
		.unwrap_or_else(|err| panic!("{label}: subscribe failed: {err}"));

	let mut group = moq_clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{label}: no group arrived"))
		.unwrap()
		.expect("the track is live");
	let frame = group.read_frame().await.unwrap().expect("the group has a frame");
	let elapsed = moq_clock::now() - start;
	assert_eq!(frame.payload.as_ref(), b"cached", "{label}");

	let mut live = track.append_group().unwrap();
	live.write_frame(Timestamp::from_millis(1).unwrap(), b"live".as_ref())
		.unwrap();
	live.finish().unwrap();
	let mut group = moq_clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{label}: the live group did not arrive"))
		.unwrap()
		.expect("the track is live");
	let frame = group.read_frame().await.unwrap().expect("the group has a frame");
	assert_eq!(frame.payload.as_ref(), b"live", "{label}");
	elapsed
}

#[moq_clock::test]
async fn every_arrival_order_delivers() {
	for version in VERSIONS {
		let pipelined = !matches!(version, "moq-lite-03" | "moq-lite-04");
		for relays in 0..=2 {
			first_frame(version, relays, Arrival::Together).await;
			// Without a TRACK stream there is nothing to reorder.
			if pipelined {
				first_frame(version, relays, Arrival::Serial).await;
				first_frame(version, relays, Arrival::SubscribeFirst).await;
			}
		}
	}
}

/// The first frame arrives one round trip after asking, per hop.
#[moq_clock::test]
async fn first_frame_takes_one_round_trip_per_hop() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		for relays in 0..=2 {
			let elapsed = first_frame(version, relays, Arrival::Together).await;
			let hops = relays as u32 + 1;
			assert!(
				elapsed <= RTT * hops,
				"{version} over {relays} relays: first frame after {elapsed:?}"
			);
		}
	}
}

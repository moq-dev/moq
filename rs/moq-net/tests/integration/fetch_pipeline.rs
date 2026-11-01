#![cfg(feature = "async")]

//! A fetch-only reader's first FETCH goes upstream with the request for the track's info
//! (TRACK on moq-lite, TRACK_STATUS on moq-transport) at every hop, rather than a round trip
//! after it. The fetched group still waits for that info before it is handed out; see
//! `model::resume`'s staged copies.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

const VERSIONS: &[&str] = &["moq-lite-05", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"];

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &moq_net::origin::Producer, to: &moq_net::origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// What keeps the publisher and the hops to the reader up.
struct Held {
	_publisher: moq_net::origin::Producer,
	_broadcast: moq_net::origin::Publication,
	_links: Vec<MockPair>,
}

/// A publisher whose track is requested on demand, so the test decides when its info goes
/// out; a reader `relays` hops away fetches its group 0. Returns the publisher's pending
/// request once the fetch reached it, before anything answered the info.
async fn fetch_ahead(
	version: &str,
	relays: u64,
) -> (
	moq_net::track::Request,
	moq_net::track::Fill,
	kio::clock::sim::JoinHandle<Result<moq_net::group::Consumer, Error>>,
	Held,
) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let mut dynamic = broadcast.requests();
	broadcast.announce(Default::default()).unwrap();

	let mut links = Vec::new();
	let mut upstream = publisher.clone();
	for hop in 0..=relays {
		let next = produce_origin(hop + 2);
		links.push(link(version, &upstream, &next).await);
		upstream = next;
	}
	let remote = kio::clock::timeout(TIMEOUT, upstream.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let fetching = kio::clock::sim::spawn(async move { remote.track("video").unwrap().fetch_group(0, None).await });

	let request = kio::clock::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("the info request arrives")
		.unwrap();
	let fill = kio::clock::timeout(TIMEOUT, request.requests().requested_fill())
		.await
		.unwrap_or_else(|_| panic!("{version}: the FETCH waited for the info"))
		.unwrap();
	let held = Held {
		_publisher: publisher,
		_broadcast: broadcast,
		_links: links,
	};
	(request, fill, fetching, held)
}

/// The FETCH is on the wire before the info is answered, at every hop, and the group comes
/// back once it is.
#[kio::test]
async fn the_first_fetch_goes_out_with_the_info_request() {
	for version in VERSIONS {
		for relays in [0, 1] {
			let (request, fill, fetching, _held) = fetch_ahead(version, relays).await;
			let _track = request.accept(timed());
			let mut group = fill.accept(0, timed()).unwrap();
			group.write_frame(Timestamp::ZERO, b"early".as_ref()).unwrap();
			group.finish().unwrap();
			let mut fetched = kio::clock::timeout(TIMEOUT, fetching)
				.await
				.expect("the fetch resolves")
				.expect("fetch panicked")
				.unwrap_or_else(|err| panic!("{version}, {relays} relays: {err:?}"));
			let frame = kio::clock::timeout(TIMEOUT, fetched.read_frame())
				.await
				.expect("read timeout")
				.unwrap()
				.expect("a frame");
			assert_eq!(&frame.payload[..], b"early", "{version}, {relays} relays");
		}
	}
}

/// A refused info fails the fetch sent alongside it rather than leaving it waiting.
#[kio::test]
async fn a_refused_info_fails_the_fetch_sent_with_it() {
	for version in VERSIONS {
		for relays in [0, 1] {
			let (request, fill, fetching, _held) = fetch_ahead(version, relays).await;
			request.reject(Error::NotFound);
			let res = kio::clock::timeout(TIMEOUT, fetching)
				.await
				.unwrap_or_else(|_| panic!("{version}, {relays} relays: the fetch never ended"))
				.expect("fetch panicked");
			assert!(res.is_err(), "{version}, {relays} relays: fetched past a refused info");
			drop(fill);
		}
	}
}

/// How long a fetch-only reader `relays` hops from a publisher waits for its first fetched
/// group over links of `rtt`, counted from the fetch once the broadcast resolved.
async fn first_fetch(version: &str, relays: u64, rtt: Duration) -> Duration {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
	group.finish().unwrap();

	let mut links = Vec::new();
	let mut upstream = publisher.clone();
	for hop in 0..=relays {
		let next = produce_origin(hop + 2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(upstream.consume());
		options.client_subscribe = Some(next.clone());
		options.latency = rtt / 2;
		links.push(connect_mock(options).await);
		upstream = next;
	}
	let remote = kio::clock::timeout(TIMEOUT, upstream.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let start = kio::clock::now();
	let mut fetched = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().fetch_group(0, None))
		.await
		.expect("fetch timeout")
		.unwrap_or_else(|err| panic!("{version}: {err:?}"));
	kio::clock::timeout(TIMEOUT, fetched.read_frame())
		.await
		.expect("read timeout")
		.unwrap()
		.expect("a frame");
	let elapsed = kio::clock::now() - start;
	drop((links, track, broadcast, publisher));
	elapsed
}

/// The first fetch costs one round trip per hop, not the two of asking for the track's info
/// first: the info and the fetch travel together.
#[kio::test]
async fn the_first_fetch_takes_one_round_trip_per_hop() {
	const RTT: Duration = Duration::from_millis(100);
	for version in VERSIONS {
		for relays in [0, 1] {
			let elapsed = first_fetch(version, relays, RTT).await;
			let hops = u32::try_from(relays + 1).unwrap();
			assert!(elapsed <= RTT * hops, "{version}, {relays} relays: {elapsed:?}");
		}
	}
}

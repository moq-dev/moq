#![cfg(feature = "async")]

//! A group that fails is that group's gap, never the track's end: a remote reader holding a
//! group an on-demand track's dropped producer left open, or one its publisher aborted, sees
//! that group end with an error and reads on to the next group, on every wire.

use crate::support;

use std::time::Duration;

use moq_net::{Timestamp, Version, track};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-18",
	"moq-transport-22",
	"moq-transport-23",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn info() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn at(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

/// A publisher and a downstream hop joined by one session.
struct Hop {
	route: moq_net::origin::Route,
	remote: moq_net::broadcast::Consumer,
	_pair: support::harness::MockPair,
	_origins: (moq_net::origin::Producer, moq_net::origin::Producer),
}

impl Hop {
	async fn new(version: &str) -> Self {
		let publisher = produce_origin(1);
		let downstream = produce_origin(2);
		let pair = peer(version.parse::<Version>().unwrap(), &publisher, &downstream).await;
		let route = publisher
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		route.announce().unwrap();
		let remote = kio::clock::timeout(TIMEOUT, downstream.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		Self {
			route,
			remote,
			_pair: pair,
			_origins: (publisher, downstream),
		}
	}
}

/// Wait for `group` to end with an error, as a cut group does.
async fn read_failure(group: &mut moq_net::group::Consumer) {
	let read = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("the held group never ended");
	assert!(read.is_err(), "the group ended cleanly: {read:?}");
}

/// Read the first group, and its first frame, from `reader`.
async fn hold(reader: &mut track::Subscriber) -> moq_net::group::Consumer {
	let mut held = kio::clock::timeout(TIMEOUT, reader.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	assert!(held.read_frame().await.unwrap().is_some());
	held
}

/// Write one finished group at `ms` to `track`, then read it from `reader`.
async fn reads_on(track: &track::Producer, reader: &mut track::Subscriber, version: &str) {
	let mut group = track.append_group(at(100)).unwrap();
	group.write_frame(at(100), b"next".as_slice()).unwrap();
	group.finish().unwrap();
	let after = kio::clock::timeout(TIMEOUT, reader.recv_group())
		.await
		.expect("next group timeout")
		.unwrap()
		.expect("the track ended at a failed group");
	assert_eq!(after.start, 100, "{version}");
}

#[kio::test]
async fn reads_past_a_group_a_drop_left_open() {
	for version in VERSIONS {
		let hop = Hop::new(version).await;
		let mut requests = hop.route.broadcast().requests();
		let subscribing = hop.remote.track("video").unwrap().subscribe(None);
		let first = kio::clock::timeout(TIMEOUT, requests.requested_track())
			.await
			.expect("request timeout")
			.unwrap()
			.accept(info());
		let mut reader = kio::clock::timeout(TIMEOUT, subscribing)
			.await
			.expect("subscribe timeout")
			.expect("subscribe");

		let mut open = first.append_group(at(0)).unwrap();
		open.write_frame(at(0), b"held".as_slice()).unwrap();
		let mut held = hold(&mut reader).await;
		drop((first, open));

		// The held group may wait on the renewed track before it gives up, so both run at once.
		let renewal = async {
			kio::clock::timeout(TIMEOUT, requests.requested_track())
				.await
				.expect("renewal timeout")
				.unwrap()
				.accept(info())
		};
		let ((), next) = futures::join!(read_failure(&mut held), renewal);
		reads_on(&next, &mut reader, version).await;
	}
}

#[kio::test]
async fn reads_past_a_group_its_publisher_aborted() {
	for version in VERSIONS {
		let hop = Hop::new(version).await;
		let track = hop.route.broadcast().create_track("video", info()).unwrap();
		let mut reader = kio::clock::timeout(TIMEOUT, hop.remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		support::harness::subscribed(&track).await;

		let mut open = track.append_group(at(0)).unwrap();
		open.write_frame(at(0), b"cut".as_slice()).unwrap();
		let mut held = hold(&mut reader).await;
		open.abort(moq_net::Error::App(7)).unwrap();

		read_failure(&mut held).await;
		reads_on(&track, &mut reader, version).await;
	}
}

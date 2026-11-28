#![cfg(feature = "async")]

//! A group an on-demand track's dropped producer left open while a remote reader held it
//! reaches that reader as superseded on every wire, so it reads on to the next producer's
//! group, while a publisher aborting a group itself never reads as superseded.

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

/// Read `group`'s next frame, which a cut leaves as the error that ended it.
async fn read_end(group: &mut moq_net::group::Consumer) -> moq_net::Error {
	match kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("the held group never ended")
	{
		Ok(frame) => panic!("the group ended cleanly: {frame:?}"),
		Err(err) => err,
	}
}

#[kio::test]
async fn a_held_group_a_drop_left_open_reads_as_superseded() {
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
		let mut held = kio::clock::timeout(TIMEOUT, reader.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		assert!(held.read_frame().await.unwrap().is_some());
		drop((first, open));

		let err = read_end(&mut held).await;
		assert!(err.superseded(), "{version}: the held group ended with {err:?}");

		let next = kio::clock::timeout(TIMEOUT, requests.requested_track())
			.await
			.expect("renewal timeout")
			.unwrap()
			.accept(info());
		let mut group = next.append_group(at(100)).unwrap();
		group.write_frame(at(100), b"next".as_slice()).unwrap();
		group.finish().unwrap();
		let after = kio::clock::timeout(TIMEOUT, reader.recv_group())
			.await
			.expect("next group timeout")
			.unwrap()
			.expect("the next producer's group");
		assert_eq!(after.start, 100, "{version}");
	}
}

#[kio::test]
async fn a_publisher_abort_never_reads_as_superseded() {
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
		let mut held = kio::clock::timeout(TIMEOUT, reader.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		assert!(held.read_frame().await.unwrap().is_some());
		open.abort(moq_net::Error::App(7)).unwrap();

		let err = read_end(&mut held).await;
		assert!(!err.superseded(), "{version}: the abort read as superseded: {err:?}");
	}
}

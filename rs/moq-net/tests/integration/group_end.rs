#![cfg(feature = "async")]

//! A group finished whole reaches the next hop with its end, over a subscription and over a
//! fetch, and a closed one without it. Lite-07 says which with GROUP_END; earlier versions
//! end every group with a bare FIN, so each arrives closed.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-07-wip"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// Group 0 finished at 500ms and group 1000 closed, on a live track that keeps both.
fn publish(origin: &moq_net::origin::Producer) -> (moq_net::origin::Route, moq_net::track::Producer) {
	let broadcast = origin.route("room", moq_net::origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	for (start, end) in [(0, Some(500)), (1000, None)] {
		let group = track
			.append_group(Timestamp::from_millis(start).unwrap(), b"x".as_slice())
			.unwrap();
		match end {
			Some(end) => group.finish_at(end).unwrap(),
			None => group.finish().unwrap(),
		};
	}
	(broadcast, track)
}

/// Read `group` to its end, returning where it says it ends.
async fn read_to_end(mut group: moq_net::group::Consumer) -> Option<u64> {
	while kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read timeout")
		.unwrap()
		.is_some()
	{}
	group.end()
}

async fn round(version: &str) -> (Vec<(u64, Option<u64>)>, Option<u64>) {
	let publisher = produce_origin(1);
	let (_broadcast, _track) = publish(&publisher);
	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let _pair = connect_mock(options).await;

	let remote = kio::clock::timeout(TIMEOUT, client.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	let mut subscriber = kio::clock::timeout(
		TIMEOUT,
		remote
			.track("video")
			.unwrap()
			.subscribe(Subscription::default().with_ranges([Range::ALL]).with_max(None)),
	)
	.await
	.expect("subscribe timeout")
	.unwrap();
	let mut subscribed = Vec::new();
	while subscribed.len() < 2 {
		let group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		let start = group.start;
		subscribed.push((start, read_to_end(group).await));
	}
	// Which of the two is read first depends on which lands first, so only the set is checked.
	subscribed.sort();

	// A second reader fetching group 0 from a hop that never held it.
	let fetcher = produce_origin(3);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(fetcher.clone());
	let fetch_pair = connect_mock(options).await;
	let remote = kio::clock::timeout(TIMEOUT, fetcher.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	let fetched = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().fetch_group(0, None))
		.await
		.expect("fetch timeout")
		.unwrap();
	let fetched = read_to_end(fetched).await;
	// The bidi stream types: SUBSCRIBE is 2, FETCH is 3 (removed on lite-07).
	const SUBSCRIBE: u8 = 2;
	const FETCH: u8 = 3;
	if version == "moq-lite-07-wip" {
		assert!(
			!fetch_pair.client_transport.bidi_types().contains(&FETCH),
			"{version}: unexpected FETCH"
		);
		assert!(
			fetch_pair.client_transport.bidi_types().contains(&SUBSCRIBE),
			"{version}: no SUBSCRIBE"
		);
	} else {
		assert!(
			fetch_pair.client_transport.bidi_types().contains(&FETCH),
			"{version}: no FETCH"
		);
	}
	(subscribed, fetched)
}

#[kio::test]
async fn a_finished_group_arrives_with_its_end() {
	for version in VERSIONS {
		let (subscribed, fetched) = round(version).await;
		let whole = match *version {
			"moq-lite-07-wip" => Some(500),
			_ => None,
		};
		assert_eq!(subscribed, [(0, whole), (1000, None)], "{version}: subscribed");
		assert_eq!(fetched, whole, "{version}: fetched");
	}
}

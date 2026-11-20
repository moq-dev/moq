#![cfg(feature = "async")]

//! A group finished whole reaches the next hop whole, over a subscription and over a fetch,
//! and a closed one only closed. Lite-07 says which with GROUP_END; earlier versions end every
//! group with a bare FIN, so each arrives closed.

use crate::support;

use std::time::Duration;

use moq_net::track::{Delay, Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// Group 0 finished whole and group 1000 closed, on a live track that keeps both.
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
	for (start, whole) in [(0, true), (1000, false)] {
		let mut group = track.append_group(Timestamp::from_millis(start).unwrap()).unwrap();
		group
			.write_frame(Timestamp::from_millis(start).unwrap(), b"x".as_slice())
			.unwrap();
		match whole {
			true => group.finish_whole().unwrap(),
			false => group.finish().unwrap(),
		};
	}
	(broadcast, track)
}

/// Read `group` to its end, returning whether it says it is whole.
async fn read_to_end(mut group: moq_net::group::Consumer) -> bool {
	while kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read timeout")
		.unwrap()
		.is_some()
	{}
	group.is_whole()
}

async fn round(version: &str) -> (Vec<(u64, bool)>, bool) {
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
		remote.track("video").unwrap().subscribe(
			Subscription::default()
				.with_delay(Delay::default().with_max(None))
				.with_ranges([Range::oldest()]),
		),
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
	// The bidi stream type a FETCH opens with.
	const FETCH: u8 = 3;
	assert!(
		fetch_pair.client_transport.bidi_types().contains(&FETCH),
		"{version}: no FETCH"
	);
	(subscribed, fetched)
}

#[kio::test]
async fn a_whole_group_arrives_whole() {
	for version in VERSIONS {
		let (subscribed, fetched) = round(version).await;
		let whole = *version == "moq-lite-07-wip";
		assert_eq!(subscribed, [(0, whole), (1000, false)], "{version}: subscribed");
		assert_eq!(fetched, whole, "{version}: fetched");
	}
}

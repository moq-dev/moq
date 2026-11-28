#![cfg(feature = "async")]

//! A lite-07 relay whose viewers' ranges, merged, outgrow one message asks its upstream for a
//! covering superset rather than give back the copy every viewer reads through.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const VERSION: &str = "moq-lite-07-wip";

/// Group keys past 2^32, so each range's times take five bytes on the wire.
const BASE: u64 = 1 << 32;
/// Sparse viewers, each asking for every `VIEWERS * 2`th group from its own offset.
const VIEWERS: u64 = 3;
/// Single-group ranges per sparse viewer: each viewer's fits what a subscriber asks in one
/// message, and all together outgrow even a whole message.
const RANGES: u64 = 2_000;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, key: u64) {
	let group = track
		.create_group(Timestamp::from_millis(key).unwrap(), &b"x"[..])
		.unwrap();
	group.finish().unwrap();
}

/// A viewer of the relay, on a session of its own.
async fn viewer(relay: &moq_net::origin::Producer, hop: u64) -> (moq_net::broadcast::Consumer, impl Sized) {
	let origin = produce_origin(hop);
	let mut options = MockConnectOptions::new(VERSION.parse::<Version>().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(origin.clone());
	let session = connect_mock(options).await;
	let broadcast = kio::clock::timeout(TIMEOUT, origin.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	(broadcast, (session, origin))
}

/// Every `VIEWERS * 2`th group from `offset`, one range each, so no two viewers' ranges touch.
fn sparse(offset: u64) -> Vec<Range> {
	(0..RANGES)
		.map(|index| BASE + 2 * VIEWERS * index + offset)
		.map(|key| support::keys(key..=key))
		.collect()
}

/// Whether `ranges` asks for every group `want` does.
fn covers(ranges: &[Range], want: &[Range]) -> bool {
	want.iter().all(|want| {
		let moq_net::track::Start::Time(key) = want.start else {
			unreachable!()
		};
		ranges.iter().any(|range| {
			let start = matches!(range.start, moq_net::track::Start::Time(start) if start <= key);
			let end = range.end.is_none_or(|end| key <= end);
			start && end
		})
	})
}

/// Ranges no group resolves yet reach the publisher as asked, so its view is what the relay
/// sent.
#[kio::test]
async fn a_union_too_large_for_one_message_asks_a_covering_superset() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = broadcast.broadcast().create_track("quiet", info).unwrap();
	broadcast.announce().unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(VERSION.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;

	let mut viewers = Vec::new();
	let mut readers = Vec::new();
	let mut asked = Vec::new();
	for index in 0..VIEWERS {
		let (broadcast, session) = viewer(&relay, 4 + index).await;
		let ranges = sparse(2 * index);
		let subscription = Subscription::default().with_ranges(ranges.clone()).with_max(None);
		readers.push(
			broadcast
				.track("quiet")
				.unwrap()
				.subscribe(subscription)
				.await
				.expect("subscribe"),
		);
		asked.extend(ranges);
		viewers.push((broadcast, session));
	}

	// A plain viewer joins the copy the sparse ones share, asking for everything past them.
	let key = BASE + 2 * VIEWERS * RANGES;
	let (plain, _plain) = viewer(&relay, 3).await;
	let mut live = plain
		.track("quiet")
		.unwrap()
		.subscribe(Subscription::default().with_ranges([support::keys(key..)]))
		.await
		.expect("subscribe");

	// The publisher is asked for every group a viewer wants, in fewer ranges, none reaching
	// below them.
	let mut demand = track.clone();
	kio::clock::timeout(TIMEOUT, async {
		loop {
			if demand
				.subscription()
				.is_some_and(|subscription| covers(&subscription.ranges, &asked))
			{
				break;
			}
			demand.subscription_changed().await.unwrap();
		}
	})
	.await
	.expect("the union never reached the publisher");
	let ranges = track.subscription().unwrap().ranges;
	assert!(ranges.len() < asked.len(), "coarsened to {} ranges", ranges.len());
	assert!(
		ranges
			.iter()
			.all(|range| matches!(range.start, moq_net::track::Start::Time(start) if start >= BASE)),
		"nothing below the viewers' ranges"
	);

	// Each viewer reads only its own groups, none of the coarsened extra.
	write_group(&track, BASE + 2);
	write_group(&track, BASE + 3);
	write_group(&track, BASE + 2 * VIEWERS + 2);
	write_group(&track, key);
	// Which of its two is read first depends on which lands first, so only the set is checked.
	let mut got = Vec::new();
	for _ in 0..2 {
		let group = kio::clock::timeout(TIMEOUT, readers[1].recv_group())
			.await
			.unwrap()
			.unwrap()
			.expect("a group");
		got.push(group.start);
	}
	got.sort();
	assert_eq!(got, [BASE + 2, BASE + 2 * VIEWERS + 2]);
	let got = kio::clock::timeout(TIMEOUT, live.recv_group()).await.unwrap().unwrap();
	assert_eq!(
		got.map(|group| group.start),
		Some(key),
		"the plain viewer keeps receiving"
	);
}

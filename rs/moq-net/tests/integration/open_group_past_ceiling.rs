#![cfg(feature = "async")]

//! A track whose newest group stays open past its publisher's cache ceiling, above history
//! stale under it, still reaches a reader two relays away that asks from the oldest group.

use crate::support;

use std::time::Duration;

use moq_net::{Timestamp, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

/// How long the publisher's cache keeps a group fresh.
const CEILING: Duration = Duration::from_secs(30);

fn produce_origin(hop: u64, ceiling: Option<Duration>) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	config.cache_duration = ceiling;
	let (producer, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// `publisher` announces into `subscriber` over one session, as a relay forwards upstream.
async fn upstream(version: Version, publisher: &origin::Producer, subscriber: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume().with_hidden(true));
	options.server_subscribe = Some(subscriber.clone());
	connect_mock(options).await
}

#[kio::test]
async fn an_open_group_past_the_ceiling_reaches_an_oldest_reader() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = produce_origin(1, Some(CEILING));
	let relay = produce_origin(2, None);
	let reader = produce_origin(3, None);

	let route = publisher.route("stats", origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let producer = route.broadcast().create_track("bucket", info).unwrap();
	route.announce().unwrap();

	// A closed group, then an open one written past the ceiling: by the time anyone reads, the
	// closed group is gone and everything below the open one, the skips each append declared
	// included, is stale under the ceiling, as a long-lived track's evicted history is.
	let at = |ms: u64| Timestamp::from_millis(ms).unwrap();
	let mut old = producer.append_group(at(10)).unwrap();
	old.write_frame(at(10), &b"old"[..]).unwrap();
	old.finish().unwrap();
	let mut group = producer.append_group(at(1_000)).unwrap();
	for n in 0..60 {
		group.write_frame(at(1_000 + n * 1_000), &b"frame"[..]).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
	}

	// The relays join only now.
	let _upstream = upstream(version, &publisher, &relay).await;
	let _downstream = upstream(version, &relay, &reader).await;
	let consumer = kio::clock::timeout(TIMEOUT, reader.consume().request("stats", None))
		.await
		.unwrap()
		.unwrap();
	let subscription = track::Subscription::default().with_ranges([track::Range::from(track::Start::Oldest)]);
	let mut subscriber = kio::clock::timeout(TIMEOUT, consumer.track("bucket").unwrap().subscribe(subscription))
		.await
		.unwrap()
		.unwrap()
		.ordered();
	group.write_frame(at(61_000), &b"frame"[..]).unwrap();
	let mut got = kio::clock::timeout(TIMEOUT, subscriber.next_group())
		.await
		.expect("the open group never arrived")
		.unwrap()
		.unwrap();
	assert_eq!(got.start, 1_000);
	let frame = kio::clock::timeout(TIMEOUT, got.read_frame())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(&frame.payload[..], b"frame");
}

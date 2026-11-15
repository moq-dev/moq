#![cfg(feature = "async")]

//! A relay passes on the timestamps it receives over lite-06 and moq-transport, with their
//! values, even for a group that arrives after a later one.

use crate::support;
use moq_net::track::Delay;

use std::time::Duration;

use moq_net::Version;
use moq_net::track::{Info, Subscription};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A group's key and its first frame's timestamp, or the error reading it.
type Read = (u64, Result<moq_net::Timestamp, moq_net::Error>);

fn millis(ms: u64) -> moq_net::Timestamp {
	moq_net::Timestamp::from_millis(ms).unwrap()
}

/// Long enough that both groups are fresh.
const BUDGET: Duration = Duration::from_secs(60);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// A group starting at `key` milliseconds.
fn write_group(track: &moq_net::track::Producer, key: u64) {
	let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
	let timestamp = millis(key);
	group.write_frame(timestamp, &b"x"[..]).unwrap();
	group.close().unwrap();
}

/// The groups at one second then at zero of a millisecond track, into a relay over
/// `upstream` and out over `downstream`. Returns the scale the subscriber learned and, per
/// group read, its key and its first frame's timestamp or the error reading it.
async fn relay(upstream: &str, downstream: &str) -> (Option<moq_net::Timescale>, Vec<Read>) {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"data",
			Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();

	let relay_origin = produce_origin(2);
	let mut options = MockConnectOptions::new(upstream.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay_origin.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(downstream.parse::<Version>().unwrap());
	options.server_publish = Some(relay_origin.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = kio::clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();

	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(BUDGET))
		.with_ranges([support::keys(0..)]);
	let reader = kio::clock::sim::spawn(async move {
		let mut sub = remote
			.track("data")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let timescale = sub.info().timescale;
		let mut got = Vec::new();
		loop {
			let next = kio::clock::timeout(TIMEOUT, sub.recv_group())
				.await
				.expect("recv_group timed out");
			let Some(mut group) = next.expect("recv_group") else {
				break;
			};
			let read = kio::clock::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.map(|frame| frame.expect("a frame").timestamp);
			got.push((group.start, read));
		}
		(timescale, got)
	});

	kio::clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	kio::clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1000);
	kio::clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	let track = track.finish().unwrap();

	let got = kio::clock::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, pair, upstream, relay_origin, broadcast, publisher, subscriber));
	got
}

/// A timed track's timestamps cross a lite-06 or moq-transport hop between two origins with
/// their values, and the relay serves them on over every version.
#[kio::test]
async fn a_timed_track_keeps_its_timestamps_through_a_relay() {
	let mut failures = Vec::new();
	for upstream in ["moq-lite-06", "moq-transport-22"] {
		for downstream in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
			let (timescale, got) = relay(upstream, downstream).await;
			let ok = timescale == Some(moq_net::Timescale::MILLI)
				&& matches!(
					got.as_slice(),
					[(1000, Ok(one)), (0, Ok(zero))] if *one == millis(1000) && *zero == millis(0)
				);
			if !ok {
				failures.push(format!("{upstream} -> {downstream}: {timescale:?} {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

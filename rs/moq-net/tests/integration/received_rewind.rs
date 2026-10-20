#![cfg(feature = "async")]

//! A relay re-serves what it received without checking it for a rewind.
//!
//! Lite-06 stamps an untimed track's frames with their send time and declares
//! milliseconds, so the relay's copy reads as timed and re-advertises that scale. A group
//! sent after a higher one carries the later stamp, which the next hop must not refuse:
//! only a local publisher's writes are checked.

use crate::support;

use std::time::Duration;

use moq_net::Version;
use moq_net::track::{Info, Subscription};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough that both groups are fresh.
const BUDGET: Duration = Duration::from_secs(60);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group.write_frame(None, &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// Groups 1 then 0 of an untimed track, into a relay over lite-06 and out over `downstream`.
/// Returns the scale the subscriber learned and the groups it read.
async fn relay(downstream: &str) -> (Option<moq_net::Timescale>, Vec<u64>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("data", Info::default()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay_origin.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(downstream.parse::<Version>().unwrap());
	options.server_publish = Some(relay_origin.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_clock::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let subscription = Subscription::default().with_max_delay(BUDGET).with_groups(0..);
	let reader = moq_clock::sim::spawn(async move {
		let mut sub = remote
			.track("data")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let timescale = sub.info().timescale;
		let mut got = Vec::new();
		loop {
			let next = moq_clock::timeout(TIMEOUT, sub.recv_group())
				.await
				.expect("recv_group timed out");
			let Some(mut group) = next.expect("recv_group") else {
				break;
			};
			while moq_clock::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.expect("read_frame")
				.is_some()
			{}
			got.push(group.sequence);
		}
		(timescale, got)
	});

	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	// Sent later, so group 0's send time lands after group 1's.
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();

	let got = moq_clock::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, pair, upstream, relay_origin, broadcast, publisher, subscriber));
	got
}

#[moq_clock::test]
async fn a_relay_forwards_descending_send_times_without_closing_the_track() {
	let mut failures = Vec::new();
	for downstream in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let got = relay(downstream).await;
		// The relay's copy reads as timed, so every hop past it sees a timescale.
		if got.0.is_none() || got.1 != [1, 0] {
			failures.push(format!("{downstream}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Fetch groups 1 then 0 of a finished untimed track over `version`, with no live
/// subscription. Each answer carries its send time, so group 0 lands after group 1; the
/// copy the fetches fill is still received, so neither is refused.
async fn fetch_only(version: &str) -> Vec<Result<u64, moq_net::Error>> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("data", Info::default()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	write_group(&track, 0);
	write_group(&track, 1);
	track.finish().unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_clock::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let mut got = Vec::new();
	for sequence in [1, 0] {
		let read = async {
			let mut group = remote.track("data").unwrap().fetch_group(sequence, None).await?;
			while group.read_frame().await?.is_some() {}
			Ok(group.sequence)
		};
		got.push(moq_clock::timeout(TIMEOUT, read).await.expect("fetch timed out"));
		// Answered later, so group 0's send time lands after group 1's.
		moq_clock::sleep(Duration::from_millis(10)).await;
	}
	drop((pair, remote, broadcast, publisher, subscriber));
	got
}

#[moq_clock::test]
async fn fetches_alone_accept_descending_send_times() {
	let mut failures = Vec::new();
	for version in ["moq-lite-05", "moq-lite-06"] {
		let got = fetch_only(version).await;
		if got.iter().any(Result::is_err) {
			failures.push(format!("{version}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

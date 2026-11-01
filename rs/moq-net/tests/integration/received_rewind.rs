#![cfg(feature = "async")]

//! A relay drops the timestamps it receives over lite-05/06.
//!
//! Lite-06 stamps an untimed track's frames with their send time and declares
//! milliseconds, so a group sent after a higher one carries the later stamp. The relay
//! cannot tell that from media time, so its copy is untimed and every hop past it sees an
//! untimed track that nothing can refuse.

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
	// A timed track's group starts at its sequence in seconds, so none rewinds.
	let timestamp = track
		.info()
		.timescale
		.map(|_| moq_net::Timestamp::from_millis(sequence * 1000).unwrap());
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group.write_frame(timestamp, &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// Groups 1 then 0 of a track at `timescale`, into a relay over `upstream` and out over
/// `downstream`. Returns the scale the subscriber learned and the groups it read.
async fn relay(
	upstream: &str,
	downstream: &str,
	timescale: Option<moq_net::Timescale>,
) -> (Option<moq_net::Timescale>, Vec<u64>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("data", Info::default().with_timescale(timescale))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

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
		.expect("broadcast resolves");

	let subscription = Subscription::default().with_max_delay(BUDGET).with_range(0..);
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
			while kio::clock::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.expect("read_frame")
				.is_some()
			{}
			got.push(group.sequence);
		}
		(timescale, got)
	});

	kio::clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	kio::clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	// Sent later, so group 0's send time lands after group 1's.
	kio::clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();

	let got = kio::clock::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, pair, upstream, relay_origin, broadcast, publisher, subscriber));
	got
}

#[kio::test]
async fn a_relay_forwards_descending_send_times_without_closing_the_track() {
	let mut failures = Vec::new();
	for downstream in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let got = relay("moq-lite-06", downstream, None).await;
		if got.0.is_some() || got.1 != [1, 0] {
			failures.push(format!("{downstream}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// A moq-transport peer may stamp arrival times, so a relay keeps a timed track's timestamps
/// unchecked and passes them on only over moq-transport: a moq-lite hop, which checks every
/// timestamp it receives, is served the track untimed.
#[kio::test]
async fn moq_transport_timestamps_stay_on_moq_transport() {
	let mut failures = Vec::new();
	for (downstream, expected) in [
		("moq-lite-06", None),
		("moq-lite-07-wip", None),
		("moq-transport-22", Some(moq_net::Timescale::MILLI)),
	] {
		let got = relay("moq-transport-22", downstream, Some(moq_net::Timescale::MILLI)).await;
		if got.0 != expected || got.1 != [1, 0] {
			failures.push(format!("{downstream}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Fetch groups 1 then 0 of a finished untimed track over `version`, with no live
/// subscription. Each answer carries its send time, so group 0 lands after group 1; the
/// copy the fetches fill drops them, so neither is refused.
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
	let remote = kio::clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
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
		got.push(kio::clock::timeout(TIMEOUT, read).await.expect("fetch timed out"));
		// Answered later, so group 0's send time lands after group 1's.
		kio::clock::sleep(Duration::from_millis(10)).await;
	}
	drop((pair, remote, broadcast, publisher, subscriber));
	got
}

#[kio::test]
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

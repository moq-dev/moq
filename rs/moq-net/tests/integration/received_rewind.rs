#![cfg(feature = "async")]

//! A relay passes on the timestamps it receives over lite-05/06, unchecked.
//!
//! Lite-06 stamps an untimed track's frames with their send time and declares
//! milliseconds, so a group sent after a higher one carries the later stamp. The relay
//! cannot tell that from media time, so its copy skips the rewind check, as a
//! moq-transport one does, and serves the timestamps on over every version. A lite-07
//! receiver checks them, so it refuses that out-of-order group.

use crate::support;

use std::time::Duration;

use moq_net::Version;
use moq_net::track::{Info, Subscription};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A group's sequence and its first frame's timestamp, or the error reading it.
type Read = (u64, Result<Option<moq_net::Timestamp>, moq_net::Error>);

fn millis(ms: u64) -> moq_net::Timestamp {
	moq_net::Timestamp::from_millis(ms).unwrap()
}

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
	let timestamp = track.info().timescale.map(|_| millis(sequence * 1000));
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group.write_frame(timestamp, &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// Groups 1 then 0 of a track at `timescale`, into a relay over `upstream` and out over
/// `downstream`. Returns the scale the subscriber learned and, per group read, its sequence
/// and its first frame's timestamp or the error reading it.
async fn relay(
	upstream: &str,
	downstream: &str,
	timescale: Option<moq_net::Timescale>,
) -> (Option<moq_net::Timescale>, Vec<Read>) {
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
			let read = kio::clock::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.map(|frame| frame.expect("a frame").timestamp);
			got.push((group.sequence, read));
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

/// A relay passes on lite-06 send times unchecked, so a lite-06 or moq-transport reader
/// reads both groups timed and the track stays open. A lite-07 reader checks them and
/// refuses the group that rewinds, the cost of an untimed origin on lite-06.
#[kio::test]
async fn a_relay_forwards_descending_send_times_without_closing_the_track() {
	let mut failures = Vec::new();
	for downstream in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let (timescale, got) = relay("moq-lite-06", downstream, None).await;
		let sequences: Vec<_> = got.iter().map(|(sequence, _)| *sequence).collect();
		let refused = downstream == "moq-lite-07-wip";
		let ok = timescale == Some(moq_net::Timescale::MILLI)
			&& sequences == [1, 0]
			&& matches!(got[0].1, Ok(Some(_)))
			&& match &got[1].1 {
				Err(moq_net::Error::TimestampMismatch) => refused,
				Ok(Some(_)) => !refused,
				_ => false,
			};
		if !ok {
			failures.push(format!("{downstream}: {timescale:?} {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// A timed track's timestamps cross a lite-06 or moq-transport hop between two origins with
/// their values, and the relay serves them on over every version.
#[kio::test]
async fn a_timed_track_keeps_its_timestamps_through_a_relay() {
	let mut failures = Vec::new();
	for upstream in ["moq-lite-06", "moq-transport-22"] {
		for downstream in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
			let (timescale, got) = relay(upstream, downstream, Some(moq_net::Timescale::MILLI)).await;
			let ok = timescale == Some(moq_net::Timescale::MILLI)
				&& matches!(
					got.as_slice(),
					[(1, Ok(Some(one))), (0, Ok(Some(zero)))] if *one == millis(1000) && *zero == millis(0)
				);
			if !ok {
				failures.push(format!("{upstream} -> {downstream}: {timescale:?} {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Fetch groups 1 then 0 of a finished untimed track over `version`, with no live
/// subscription. Each answer carries its send time, so group 0 lands after group 1; the
/// copy the fetches fill is unchecked, so neither is refused. Returns each first frame's
/// timestamp.
async fn fetch_only(version: &str) -> Vec<Result<Option<moq_net::Timestamp>, moq_net::Error>> {
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
			let timestamp = group.read_frame().await?.expect("a frame").timestamp;
			while group.read_frame().await?.is_some() {}
			Ok(timestamp)
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
		// Both arrive timed with their send times, the later fetch's last.
		if !matches!(got.as_slice(), [Ok(Some(one)), Ok(Some(zero))] if zero > one) {
			failures.push(format!("{version}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

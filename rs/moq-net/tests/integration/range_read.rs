#![cfg(feature = "async")]

//! A one-range read is how a past group is fetched: it gets each group of its range, then
//! ends, on every wire, from the publisher directly or through a relay. A range reaching
//! past the live edge ends once the groups up to its end arrive.

use crate::support;

use std::time::Duration;

use moq_net::track::{Delay, Order, Subscription};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &mut moq_net::track::Producer, sequence: u64) {
	let mut group = track.append_group().unwrap();
	assert_eq!(group.sequence, sequence);
	for frame in 0..2u64 {
		let at = Timestamp::from_millis(sequence * 100 + frame).unwrap();
		group.write_frame(at, payload(sequence, frame)).unwrap();
	}
	group.finish().unwrap();
}

fn payload(sequence: u64, frame: u64) -> Vec<u8> {
	format!("{sequence}-{frame}").into_bytes()
}

/// Each group a one-range read of `range` gets, read whole, until it ends.
async fn read(sub: &mut moq_net::track::Subscriber) -> Vec<u64> {
	let mut read = Vec::new();
	while let Some(mut group) = kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("the read neither ended nor got a group")
		.expect("recv group")
	{
		let mut frames = Vec::new();
		while let Some(frame) = kio::clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read timed out")
			.expect("read frame")
		{
			frames.push(frame.payload.to_vec());
		}
		assert_eq!(frames, [payload(group.sequence, 0), payload(group.sequence, 1)]);
		read.push(group.sequence);
	}
	read
}

/// A live track with groups 0 to 2 written, read on `version` through a relay hop when
/// `relay` is set, as the reader's broadcast and the publisher's track.
async fn fixture(
	version: Version,
	relay: bool,
) -> (moq_net::broadcast::Consumer, moq_net::track::Producer, impl Sized) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let mut track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..3 {
		write_group(&mut track, sequence);
	}

	let middle = produce_origin(2);
	let client = produce_origin(3);
	let mut sessions = Vec::new();
	if relay {
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(middle.clone());
		sessions.push(connect_mock(options).await);
	}
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(match relay {
		true => middle.consume(),
		false => publisher.consume(),
	});
	options.client_subscribe = Some(client.clone());
	sessions.push(connect_mock(options).await);

	let remote = kio::clock::timeout(TIMEOUT, client.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	(remote, track, (sessions, broadcast, publisher, middle, client))
}

fn one_range(groups: std::ops::Range<u64>) -> Subscription {
	Subscription::default()
		.with_range(groups)
		.with_delay(Delay::default().with_max(None))
		.with_order(Order::Ascending)
}

async fn history(version: Version, relay: bool, finished: bool) {
	let (remote, track, _held) = fixture(version, relay).await;
	if finished {
		track.finish().unwrap();
	}
	let mut sub = remote.track("video").unwrap().subscribe(one_range(1..2)).await.unwrap();
	assert_eq!(read(&mut sub).await, [1], "{version} relay={relay} finished={finished}");
}

/// A range wholly below the live edge reads its group and ends, live or finished.
#[kio::test]
async fn a_range_of_history_reads_its_groups_and_ends() {
	let mut failures = Vec::new();
	for version in support::every_version() {
		for relay in [false, true] {
			for finished in [false, true] {
				if kio::clock::sim::spawn(history(version, relay, finished)).await.is_err() {
					failures.push((version, relay, finished));
				}
			}
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

async fn straddling(version: Version, relay: bool) {
	let (remote, mut track, _held) = fixture(version, relay).await;
	let mut sub = remote.track("video").unwrap().subscribe(one_range(1..5)).await.unwrap();
	let reading = kio::clock::sim::spawn(async move { read(&mut sub).await });
	kio::clock::sleep(Duration::from_millis(50)).await;
	for sequence in 3..6 {
		write_group(&mut track, sequence);
	}
	// Arrival order: an older wire sends a backlog in no particular order.
	let mut read = reading.await.unwrap();
	read.sort_unstable();
	assert_eq!(read, [1, 2, 3, 4], "{version} relay={relay}");
}

/// A range from below the live edge to past it reads the history and the live groups up
/// to its end, then ends. Draft-18 has no bounded SUBSCRIBE, so it joins live and the
/// reader stops at its own end.
#[kio::test]
async fn a_range_across_the_live_edge_reads_up_to_its_end() {
	let mut failures = Vec::new();
	for version in support::every_version() {
		for relay in [false, true] {
			if kio::clock::sim::spawn(straddling(version, relay)).await.is_err() {
				failures.push((version, relay));
			}
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

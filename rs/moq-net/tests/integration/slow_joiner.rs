#![cfg(feature = "async")]

//! A reader stalled before its first read, behind a relay serving it one stream at a time,
//! loses none of the groups its budget covers: they wait at the relay for stream credit.

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::{Hop, Timescale, Timestamp, Version, kio, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, spawn, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: [&str; 3] = ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

/// Groups published one per second while the reader is stalled, which resumes a second
/// after the last: group N ends `GROUPS - N - 1` seconds behind the clock then.
const GROUPS: u64 = 5;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// The groups a reader subscribed with `budget` receives once it resumes from a stall
/// spanning every group.
async fn stalled_read(version: Version, budget: Duration) -> BTreeSet<u64> {
	let publisher = origin(1);
	let relay = origin(2);
	let viewer = origin(3);
	let broadcast = publisher.create_broadcast("live").unwrap();
	let source = broadcast
		.create_track("audio", track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut upstream = MockConnectOptions::new(version);
	upstream.client_publish = Some(publisher.consume());
	upstream.server_subscribe = Some(relay.clone());
	let _upstream = connect_mock(upstream).await;

	// Gating the viewer's driver stalls it like a starved process: its transport stops
	// accepting streams, so the stream credit it grants runs out.
	let gate = kio::Producer::new(true);
	let mut downstream = MockConnectOptions::new(version);
	// Draft 22 keeps its SETUP stream open, so it needs one more for groups.
	downstream.transport.max_concurrent_uni_streams = Some(1 + usize::from(!version.is_lite()));
	downstream.client_publish = Some(relay.consume());
	downstream.server_subscribe = Some(viewer.clone());
	downstream.server_gate = Some(gate.consume());
	let _downstream = connect_mock(downstream).await;

	let consumer = viewer.consume();
	crate::support::announced(&consumer, "live").await;
	let remote = consumer.request("live", None).await.unwrap();
	let subscription = track::Subscription::default().with_max_delay(budget);
	let mut reader = remote.track("audio").unwrap().subscribe(subscription).await.unwrap();
	kio::clock::timeout(TIMEOUT, subscribed(&source)).await.unwrap();

	*gate.write().ok().unwrap() = false;
	for sequence in 0..GROUPS {
		let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_millis(sequence * 1000).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
	}
	source.finish().unwrap();
	*gate.write().ok().unwrap() = true;

	// Read until the track ends, so a skipped group can't be one that merely arrived late.
	let mut got = BTreeSet::new();
	while let Some(mut group) = kio::clock::timeout(TIMEOUT, reader.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{version}: the track never finished"))
		.unwrap()
	{
		let sequence = group.sequence;
		let frame = group.read_frame().await;
		assert!(
			frame.is_ok_and(|frame| frame.is_some()),
			"{version}: group {sequence} was cut short"
		);
		assert!(got.insert(sequence), "{version}: group {sequence} delivered twice");
	}
	got
}

#[kio::test]
async fn a_stalled_reader_keeps_every_group_its_budget_covers() {
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let got = stalled_read(version, Duration::from_secs(5)).await;
		assert_eq!(got, (0..GROUPS).collect(), "{version}");
	}
}

/// The stall backs up the relay: with a budget shorter than it, the groups that waited for
/// credit past the budget are skipped. Group 0 took the one free credit as the stall began.
#[kio::test]
async fn a_budget_shorter_than_the_stall_skips_what_waited_past_it() {
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let got = stalled_read(version, Duration::from_millis(1500)).await;
		assert_eq!(got, [0, 3, 4].into(), "{version}");
	}
}

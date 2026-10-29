#![cfg(feature = "async")]

//! A subscriber with an explicit floor receives a group created below the first group
//! that was served, while it is still inside `max_delay`.
//!
//! An unfloored pre-06 subscribe joins where the publisher starts, so that same group
//! is dropped. Lite-06 and later encode a floor of group 0 as 0, which is the only
//! spelling of both an omitted floor and group 0; the draft says that 0 is group 0.

use crate::support;

use std::time::Duration;

use moq_net::track::{Anchor, Info, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough that two groups written one after the other are both fresh.
const BUDGET: Duration = Duration::from_secs(60);

/// A budget no group outlives, on the publisher.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

const LITE: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

const TRANSPORT: &[&str] = &["moq-transport-18", "moq-transport-22"];

/// From `start`, or the oldest group still around when there is none.
fn floored(start: Option<u64>) -> Range {
	Range {
		start: start.map_or(Range::oldest().start, Anchor::Absolute),
		end: None,
	}
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Four frames starting at `sequence` seconds, so a group created late still starts
/// below the one after it.
fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	for frame in 0..4 {
		let timestamp = Timestamp::from_millis(sequence * 1000 + frame).unwrap();
		group.write_frame(timestamp, &b"x"[..]).unwrap();
	}
	group.finish().unwrap();
}

async fn read_all(sub: &mut moq_net::track::Subscriber) -> Vec<(u64, usize)> {
	let mut got = Vec::new();
	loop {
		let next = moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv_group timed out");
		let Some(mut group) = next.expect("recv_group") else {
			break;
		};
		let mut frames = 0;
		loop {
			let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
				.await
				.expect("read_frame timed out")
				.expect("read_frame");
			match frame {
				Some(_) => frames += 1,
				None => break,
			}
		}
		got.push((group.sequence, frames));
	}
	got
}

/// Group 1, then a fresh group 0, over `version`. `relay` inserts one hop.
async fn session(version: &str, relay: bool, start: Option<u64>) -> Vec<(u64, usize)> {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track(
			"late",
			Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let upstream = match relay {
		true => {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay_origin.clone());
			Some(connect_mock(options).await)
		}
		false => None,
	};

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(match relay {
		true => relay_origin.consume(),
		false => publisher.consume(),
	});
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let subscription = Subscription::default()
		.with_max_delay(BUDGET)
		.with_ranges([floored(start)]);
	let reader = moq_clock::sim::spawn(async move {
		let mut sub = remote
			.track("late")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		read_all(&mut sub).await
	});

	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	// Idle time lets the subscription settle, and again lets SUBSCRIBE_START land,
	// before the lower group is created.
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
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

async fn in_process(start: Option<u64>) -> Vec<(u64, usize)> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track(
			"late",
			Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(FOREVER),
		)
		.unwrap();
	let subscription = Subscription::default()
		.with_max_delay(BUDGET)
		.with_ranges([floored(start)]);
	let mut sub = track.subscribe(subscription);
	let reader = moq_clock::sim::spawn(async move { read_all(&mut sub).await });
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();
	let got = moq_clock::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((track, broadcast, publisher));
	got
}

#[moq_clock::test]
async fn an_explicit_floor_receives_a_group_created_below_the_first_served_group() {
	let want = vec![(1, 4), (0, 4)];
	let mut failures = Vec::new();

	let got = in_process(Some(0)).await;
	if got != want {
		failures.push(format!("in-process: {got:?}"));
	}

	for version in LITE.iter().chain(TRANSPORT) {
		for relay in [false, true] {
			let got = session(version, relay, Some(0)).await;
			if got != want {
				failures.push(format!("{version} relay={relay}: {got:?}"));
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// A live reader on group 1, then a reader from group 0, behind one relay. Returns the
/// groups each reads.
async fn merged(version: &str) -> (Vec<(u64, usize)>, Vec<(u64, usize)>) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track(
			"late",
			Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay_origin.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay_origin.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let subscribe = |range: Range| {
		let track = remote.track("late").unwrap();
		async move {
			let subscription = Subscription::default().with_max_delay(BUDGET).with_ranges([range]);
			moq_clock::timeout(TIMEOUT, track.subscribe(subscription))
				.await
				.expect("subscribe timeout")
				.expect("subscribe")
		}
	};
	let mut live = subscribe(Range::live()).await;
	moq_clock::timeout(TIMEOUT, crate::support::wanted(track.demand()))
		.await
		.expect("no subscriber appeared")
		.unwrap();
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 1);

	// The live reader is on group 1 before the other joins.
	let first = moq_clock::timeout(TIMEOUT, live.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group")
		.expect("group 1");
	assert_eq!(first.sequence, 1, "{version}: the live reader starts at group 1");
	let mut oldest = subscribe(floored(Some(0))).await;

	// Idle time lets the merged demand reach the publisher before the lower group exists.
	moq_clock::sleep(Duration::from_millis(10)).await;
	write_group(&track, 0);
	track.finish().unwrap();

	let mut live_read = vec![(1, 4)];
	live_read.extend(read_all(&mut live).await);
	let mut oldest_read = read_all(&mut oldest).await;
	oldest_read.sort_unstable();
	drop((track, pair, upstream, relay_origin, broadcast, publisher, subscriber));
	(live_read, oldest_read)
}

/// Merged at a relay, a live reader and one from group 0 still ask for group 0 upstream.
/// A wire with one span cannot carry both starts, and lite-05 reads an absent start as
/// the latest group, so the relay must spell group 0 for a fresh group 0 to reach the
/// reader that asked for it. The live reader joined above it and never sees it.
#[moq_clock::test]
async fn a_merged_subscription_keeps_an_absolute_floor() {
	let mut failures = Vec::new();
	for version in LITE {
		let (live, oldest) = merged(version).await;
		if live != [(1, 4)] || oldest != [(0, 4), (1, 4)] {
			failures.push(format!("{version}: live {live:?}, oldest {oldest:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Pre-06, an omitted floor joins at the first served group. Group 0, created after
/// group 1 was served, is below that join. Lite-06 cannot spell this separately from
/// a floor of group 0; that case is the explicit-floor test.
#[moq_clock::test]
async fn an_unfloored_lite05_join_drops_a_group_created_below_the_first_served_group() {
	let want = vec![(1, 4)];
	let mut failures = Vec::new();
	for relay in [false, true] {
		let got = session("moq-lite-05", relay, None).await;
		if got != want {
			failures.push(format!("relay={relay}: {got:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

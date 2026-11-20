#![cfg(feature = "async")]

//! A lite-07 reader's time start that the publisher's cache can't place reaches the
//! publisher's history handler as an ordinary group fill, at the start's key with a floor
//! below it. A handler that knows its groups' times answers with the group containing the
//! time; one serving key by key serves the key, which is that group when one starts there.

use crate::support;

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::track::{Anchor, Delay, FillGroups, Info, Order, Range, Requests, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A budget no group outlives, so only the range decides what is read.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

/// The groups only the handler holds, each keyed by its start in milliseconds.
const HISTORY: [u64; 4] = [0, 1000, 2000, 3000];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

fn info() -> Info {
	Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER)
}

fn write(mut group: moq_net::group::Producer, key: u64) {
	group
		.write_frame(Timestamp::from_millis(key).unwrap(), &b"x"[..])
		.unwrap();
	group.finish().unwrap();
}

/// What the handler saw: each fill's start key and floor.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(u64, u64)>>>;

/// Serve [`HISTORY`] for every fill, answering a time start with the newest group at or
/// below it when `by_time`, and key by key otherwise.
async fn serve(requests: Requests, by_time: bool, seen: Seen) {
	while let Ok(fill) = requests.requested_fill().await {
		seen.lock().unwrap().push((fill.start().group, fill.floor()));
		drop(kio::clock::sim::spawn(serve_fill(fill, by_time)));
	}
}

async fn serve_fill(mut fill: FillGroups, by_time: bool) {
	let held: BTreeSet<u64> = HISTORY.into();
	while let Some(key) = fill.wanted_changed().await {
		let start = fill.start().group;
		let found = match by_time && key == start && fill.floor() < start {
			true => held.range(fill.floor()..=key).next_back().copied(),
			false => held.contains(&key).then_some(key),
		};
		match found {
			Some(found) => {
				if let Some(group) = fill.accept(found, None).unwrap() {
					write(group, found);
				}
			}
			None => {
				// Nothing in the history at this key: drop it, up to the next held group.
				let next = held.range(key + 1..).next().copied().unwrap_or(fill.end());
				fill.drop_groups(key..next.min(fill.end()));
			}
		}
	}
}

/// A publisher whose cache holds 4000 and 5000 with [`HISTORY`] behind a handler, then what a
/// lite-07 subscriber to `range` reads, and the fills the handler saw.
async fn read(range: Range, by_time: bool) -> (BTreeSet<u64>, Vec<(u64, u64)>) {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("room", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let mut track = broadcast.broadcast().create_track("data", info()).unwrap();
	let seen = Seen::default();
	let handler = kio::clock::sim::spawn(serve(track.requests(), by_time, seen.clone()));
	broadcast.announce().unwrap();
	track.start_at(4000).unwrap();
	for key in [4000, 5000] {
		write(track.create_group(moq_net::group::Info { start: key }).unwrap(), key);
	}

	let subscriber = produce_origin(2);
	let _pair = peer("moq-lite-07-wip".parse::<Version>().unwrap(), &publisher, &subscriber).await;
	let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();

	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(FOREVER))
		.with_order(Order::Ascending)
		.with_ranges([range]);
	let mut reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.unwrap();

	let mut read = BTreeSet::new();
	while let Ok(group) = kio::clock::timeout(TIMEOUT, reader.recv_group()).await {
		match group.unwrap() {
			Some(group) => assert!(read.insert(group.start), "read once"),
			None => break,
		}
	}
	drop(handler);
	let seen = seen.lock().unwrap().clone();
	(read, seen)
}

/// A handler that knows its groups' times answers a time start between them with the group
/// containing it, below the start's key.
#[kio::test]
async fn a_time_start_is_answered_by_the_group_containing_it() {
	let range = Range {
		start: Anchor::Time(2500),
		end: Some(Anchor::Time(4500)),
	};
	let (read, seen) = read(range, true).await;
	assert_eq!(read, [2000, 3000, 4000].into());
	assert_eq!(
		seen.first(),
		Some(&(2500, 0)),
		"the start reaches the handler as key 2500"
	);
}

/// A handler serving key by key serves a time start at a group's own key as that group.
#[kio::test]
async fn a_time_start_at_a_key_is_served_key_by_key() {
	let range = Range {
		start: Anchor::Time(2000),
		end: Some(Anchor::Time(4500)),
	};
	let (read, seen) = read(range, false).await;
	assert_eq!(read, [2000, 3000, 4000].into());
	assert_eq!(
		seen.first(),
		Some(&(2000, 0)),
		"the start reaches the handler as key 2000"
	);
}

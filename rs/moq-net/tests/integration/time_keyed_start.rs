#![cfg(feature = "async")]

//! A downstream reader of a time-keyed track, through a chain of relays.
//!
//! The track's groups are keyed by their start in milliseconds since the epoch, so its first
//! group sits far above 0. A reader asking from the oldest group, in key order, must read that
//! group first: no group below it ever existed, so no relay may start the reader below it, nor
//! report the keys under it as lost.

use crate::support::EveryRange;
use std::time::Duration;

use moq_net::track::{Item, Order, Range, Start, Subscription};
use moq_net::{Timescale, Timestamp, Version, cache, origin, track};

use crate::support::harness::{peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A wall-clock key: milliseconds since the epoch.
const KEY: u64 = 1_795_510_341_688;

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	config.proxy = true;
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin
}

/// Write the track's first item as a live publisher does: appended at its wall-clock time.
fn write(track: &mut track::Producer, key: u64) {
	track
		.write_frame(Timestamp::from_millis(key).unwrap(), &b"x"[..])
		.unwrap();
}

/// What a key-order reader from the oldest group reads first across `links` sessions, with
/// the only group written before (`early`) or after it subscribes.
async fn first_item(links: u64, early: bool) -> String {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let mut hops = vec![publisher.clone()];
	let mut pairs = Vec::new();
	for hop in 1..=links {
		let next = origin(hop + 1);
		pairs.push(peer(version, hops.last().unwrap(), &next).await);
		hops.push(next);
	}
	let reader = hops.last().unwrap().clone();

	let route = publisher.route("agent", origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(Some(Duration::from_secs(30)));
	let mut source = route.broadcast().create_track("text", info).unwrap();
	route.announce().unwrap();
	if early {
		write(&mut source, KEY);
	}

	let item = kio::clock::timeout(TIMEOUT, async {
		let remote = reader.consume().follow("agent", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		let subscription = Subscription::default()
			.with_ranges([Range {
				start: Start::Oldest,
				end: None,
				..Default::default()
			}])
			.ordered(Order::Ascending);
		let mut ordered = remote
			.track("text")
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap()
			.ordered();
		if !early {
			write(&mut source, KEY);
		}
		match ordered.next_item().await.unwrap() {
			Some(Item::Group(group)) => format!("group {}", group.start),
			Some(Item::Gap { range, error, .. }) => format!("gap {range:?} {error}"),
			Some(_) => "another item".to_string(),
			None => "end".to_string(),
		}
	})
	.await
	.unwrap_or_else(|_| "hang".to_string());
	drop(pairs);
	item
}

/// Read over one session or through a relay, with the group written before or after the
/// reader subscribes, the reader's first item is the track's first group.
#[kio::test]
async fn a_time_keyed_track_starts_at_its_first_group() {
	let mut wrong = Vec::new();
	for links in [1, 2] {
		for early in [true, false] {
			let item = first_item(links, early).await;
			if item != format!("group {KEY}") {
				wrong.push(format!("{links} links, early {early}: {item}"));
			}
		}
	}
	assert!(wrong.is_empty(), "{wrong:#?}");
}

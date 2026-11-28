#![cfg(feature = "async")]

//! A late oldest-group reader of a finished track whose first group was created, not appended.
//!
//! The group sits at a wall-clock key far above 0. Reading it locally and through relays must
//! give the same items, whether or not a latest-group reader filled the relay's copy first.

use std::time::Duration;

use moq_net::track::{Item, Order, Range, Start, Subscription};
use moq_net::{Timescale, Timestamp, Version, cache, group, origin, track};

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

/// What an oldest-group, key-order reader reads first across `links` sessions, after a
/// latest-group reader (`warm`) took the group through the same copy.
async fn first_item(links: u64, warm: bool) -> String {
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
	let source = route.broadcast().create_track("text", info).unwrap();
	route.announce().unwrap();
	let mut group = source.create_group(group::Info { start: KEY }).unwrap();
	group.write_frame(Timestamp::from_millis(KEY).unwrap(), &b"x"[..]).unwrap();
	group.finish().unwrap();
	let _finished = source.finish().unwrap();

	let item = kio::clock::timeout(TIMEOUT, async {
		let remote = reader.consume().follow("agent", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		if warm {
			let mut latest = remote
				.track("text")
				.unwrap()
				.subscribe(Subscription::default())
				.await
				.unwrap()
				.ordered();
			assert!(matches!(latest.next_item().await.unwrap(), Some(Item::Group(_))));
		}
		let subscription = Subscription::default()
			.with_ranges([Range {
				start: Start::Oldest,
				end: None,
			}])
			.with_order(Order::Ascending);
		let mut ordered = match remote.track("text").unwrap().subscribe(subscription).await {
			Ok(subscription) => subscription.ordered(),
			Err(err) => return format!("subscribe {err}"),
		};
		match ordered.next_item().await {
			Ok(Some(Item::Group(group))) => format!("group {}", group.start),
			Ok(Some(Item::Gap { range, error, .. })) => format!("gap {range:?} {error}"),
			Ok(Some(_)) => "another item".to_string(),
			Ok(None) => "end".to_string(),
			Err(err) => format!("error {err}"),
		}
	})
	.await
	.unwrap_or_else(|_| "hang".to_string());
	drop(pairs);
	item
}

/// Local, one relay, and two relays read the same first item, warmed or not.
#[kio::test]
async fn a_created_first_group_reads_alike_everywhere() {
	let mut all = Vec::new();
	for links in [0, 1, 2] {
		for warm in [false, true] {
			all.push(format!("{links} links, warm {warm}: {}", first_item(links, warm).await));
		}
	}
	let want = format!("gap 0..{KEY} not found");
	let wrong: Vec<_> = all.iter().filter(|got| !got.ends_with(&want)).collect();
	assert!(wrong.is_empty(), "every case reads the same; differing: {wrong:#?}");
}

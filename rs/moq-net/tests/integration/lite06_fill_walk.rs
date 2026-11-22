#![cfg(feature = "async")]

//! A lite-06 relay copy of a timestamp-keyed track whose publisher never says which keys
//! between its group starts are empty, and resets its first groups as `Old` after their
//! headers went out: an ordered reader on the relay reaches the later groups, and the copy
//! asks upstream about the keys between group starts once per run, not once per key.

use crate::support;

use std::time::Duration;

use moq_net::track::{Delay, Subscription};
use moq_net::{Error, Timestamp, Version, origin, track};
use support::harness::{MockConnectOptions, connect_mock};

/// One group per second of media, keyed by its start in microseconds.
const GROUP: u64 = 1_000_000;
const FRAME: u64 = 33_333;
const FRAMES: u64 = 30;
const GROUPS: u64 = 8;
/// The groups the publisher resets as `Old`, from the first.
const RESET: u64 = 3;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	origin.with_proxy(true)
}

/// The FETCHes the relay sent upstream: FETCH's lite stream type is 3, a one-byte varint.
fn fetches(pair: &support::harness::MockPair) -> usize {
	pair.client_transport
		.bidi_types()
		.into_iter()
		.filter(|kind| *kind == 3)
		.count()
}

#[kio::test]
async fn reset_groups_cost_no_fetch_per_key() {
	// The ordered reader alone, from the first group; then behind a live reader holding the
	// copy, joining once the resets are over.
	for (live, join) in [(false, 0), (true, RESET + 1)] {
		reset_groups(live, join).await;
	}
}

async fn reset_groups(live: bool, join: u64) {
	let publisher = origin(1);
	let broadcast = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(moq_net::Timescale::MICRO)
				.with_max_age(Some(Duration::from_secs(30))),
		)
		.unwrap();
	broadcast.announce().unwrap();

	let relay = origin(2);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	// Each FETCH costs a round trip, as on a network, so a walk shows within the run.
	options.latency = Duration::from_millis(5);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;
	let remote = relay
		.consume()
		.follow("bcast", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();

	let _live = match live {
		true => {
			let held = remote
				.track("video")
				.unwrap()
				.subscribe(Subscription::default())
				.await
				.unwrap();
			support::harness::subscribed(&track).await;
			Some(held)
		}
		false => None,
	};

	let mut finished = Vec::new();
	let mut read = None;
	for index in 0..GROUPS {
		if index == join {
			let subscription = Subscription::default().with_delay(Delay::default().with_max(Duration::from_secs(30)));
			let mut reader = remote
				.track("video")
				.unwrap()
				.subscribe(subscription)
				.await
				.unwrap()
				.ordered();
			if !live {
				support::harness::subscribed(&track).await;
			}
			read = Some(kio::clock::sim::spawn(async move {
				let mut keys = Vec::new();
				while let Ok(Ok(Some(mut group))) =
					kio::clock::timeout(Duration::from_secs(60), reader.next_group()).await
				{
					while let Ok(Some(_)) = group.read_frame().await {}
					keys.push(group.start);
					if group.start == (GROUPS - 1) * GROUP {
						break;
					}
				}
				keys
			}));
		}
		// Created rather than appended: like a publisher that cannot say, the track never
		// declares the keys between its group starts skipped.
		let start = index * GROUP;
		let mut group = track.create_group(moq_net::group::Info { start }).unwrap();
		for frame in 0..FRAMES {
			group
				.write_frame(Timestamp::from_micros(start + frame * FRAME).unwrap(), &b"frame"[..])
				.unwrap();
			kio::clock::sleep(Duration::from_micros(FRAME)).await;
		}
		match index < RESET {
			true => group.abort(Error::Old).unwrap(),
			false => finished.push(group.finish().unwrap()),
		}
	}

	let keys = kio::clock::timeout(Duration::from_secs(120), read.unwrap())
		.await
		.expect("the reader reaches the last group")
		.unwrap();
	assert_eq!(keys.last(), Some(&((GROUPS - 1) * GROUP)), "live {live}: read {keys:?}");
	// At most one request per group: never one per key between them.
	let fetched = fetches(&upstream);
	assert!(fetched <= GROUPS as usize, "live {live}: {fetched} fetches for {GROUPS} groups");
	drop((track, broadcast, upstream, publisher, relay, finished));
}

#![cfg(feature = "async")]

//! A lite-06 relay copy of a timestamp-keyed track whose publisher never says which keys
//! between its group starts are empty, and resets its first groups as `Old` after their
//! headers went out: an ordered reader on the relay reaches the later groups, and the copy
//! asks upstream about the keys between group starts once per run, not once per key, on each
//! hop of a relay chain too. A run still asked when the publisher ends the track is answered
//! by upstream before the copy finishes.

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
	start(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()))
}

/// An origin that holds its groups for good, where another's cache would expire them.
fn holding(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = moq_net::cache::Pool::unbounded();
	start(config)
}

fn start(mut config: origin::Config) -> origin::Producer {
	config.proxy = true;
	let (origin, driver) = origin::Producer::new(config);
	support::harness::spawn(driver);
	origin
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
	// copy, joining once the resets are over; then alone again, two relays from the publisher,
	// where the first answers each run's SUBSCRIBE from where its own copy resumes.
	for (live, join, hops) in [(false, 0, 1), (true, RESET + 1, 1), (false, 0, 2)] {
		reset_groups(live, join, hops).await;
	}
}

async fn reset_groups(live: bool, join: u64, hops: u64) {
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

	// Each relay copies the hop before it, the reader on the last.
	let mut links = Vec::new();
	let mut relays = Vec::new();
	let mut source = publisher.consume();
	for hop in 0..hops {
		let relay = origin(hop + 2);
		let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
		// Each FETCH costs a round trip, as on a network, so a walk shows within the run.
		options.latency = Duration::from_millis(5);
		options.server_publish = Some(source);
		options.client_subscribe = Some(relay.clone());
		links.push(connect_mock(options).await);
		source = relay.consume();
		relays.push(relay);
	}
	let relay = relays.last().unwrap().clone();
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
	assert_eq!(
		keys.last(),
		Some(&((GROUPS - 1) * GROUP)),
		"live {live}, hops {hops}: read {keys:?}"
	);
	// At most one request per group on each hop: never one per key between them.
	for (hop, link) in links.iter().enumerate() {
		let requests = fetches(link) + subscribes(link);
		assert!(
			requests <= 2 * GROUPS as usize,
			"live {live}, hop {hop} of {hops}: {requests} requests for {GROUPS} groups"
		);
	}
	drop((track, broadcast, links, publisher, relays, finished));
}

/// The SUBSCRIBEs the relay opened upstream: SUBSCRIBE's lite stream type is 2.
fn subscribes(pair: &support::harness::MockPair) -> usize {
	pair.client_transport
		.bidi_types()
		.into_iter()
		.filter(|kind| *kind == 2)
		.count()
}

/// The publisher ends the track while the copy still recovers a run of history it evicted,
/// and a FETCH joined that run's fill. The copy's own subscription FINs, but the run is still
/// asked of upstream, and the FETCH answered from what upstream sends, before the copy
/// finishes, even once the run's SUBSCRIBE FINs ahead of its group streams: nothing is given
/// up on the copy's say, and nothing hangs.
#[kio::test]
async fn a_recovery_outlives_the_finished_subscription() {
	const TIMEOUT: Duration = Duration::from_secs(10);
	// The publisher holds its groups for good; the copy's cache expires them.
	let publisher = holding(1);
	let broadcast = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	let mut finished = Vec::new();
	for start in 0..6 {
		let mut group = track.create_group(moq_net::group::Info { start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start * 1000).unwrap(), &b"x"[..])
			.unwrap();
		finished.push(group.finish().unwrap());
	}

	let relay = origin(2);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
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
	let history = |ranges: track::Range| {
		let track = remote.track("video").unwrap();
		async move {
			track
				.subscribe(
					Subscription::default()
						.with_delay(Delay::default().with_max(None))
						.with_ranges([ranges]),
				)
				.await
				.unwrap()
		}
	};

	// The live subscription delivers every group, which the copy then expires while a reader
	// keeps it live.
	let mut first = history(track::Range::default()).await;
	let mut got = Vec::new();
	while got.len() < 6 {
		let group = kio::clock::timeout(TIMEOUT, first.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		got.push(group.start);
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;

	// A reader reaching back misses groups 1..4, a run asked on a SUBSCRIBE of its own, held
	// from the publisher.
	upstream.client_transport.hold_bidis();
	let before = subscribes(&upstream);
	let mut second = history(support::keys(1..4)).await;
	let read = kio::clock::sim::spawn(async move {
		let mut keys = Vec::new();
		let end = loop {
			match second.recv_group().await {
				Ok(Some(group)) => keys.push(group.start),
				end => break end.map(|_| ()),
			}
		};
		keys.sort();
		(keys, end)
	});
	kio::clock::timeout(TIMEOUT, async {
		while subscribes(&upstream) == before {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("the run is recovered on a SUBSCRIBE of its own");

	// A FETCH of a group in the run joins its fill rather than going out on its own.
	let fetch = kio::clock::sim::spawn({
		let track = remote.track("video").unwrap();
		async move {
			let mut group = track.fetch_group(2, None).await?;
			let frame = group.read_frame().await?;
			Ok::<_, moq_net::Error>(frame.map(|frame| frame.payload.to_vec()))
		}
	});
	kio::clock::sleep(Duration::from_millis(100)).await;
	assert_eq!(fetches(&upstream), 0, "the FETCH joined the run's fill");

	// The publisher ends the track, so the copy's subscription FINs and its tail settles, well
	// past any grace, while the run is still unanswered.
	let _ended = track.finish().unwrap();
	kio::clock::sleep(Duration::from_secs(60)).await;
	assert!(
		!fetch.is_finished(),
		"the FETCH was answered before upstream answered its run"
	);
	assert!(
		!read.is_finished(),
		"the reader was answered before upstream answered its run"
	);

	// Upstream answers the run and, the track being over, FINs at once: its group streams
	// land only after that FIN, as QUIC may deliver them. The FETCH and the reader still get
	// the run's groups.
	upstream.server_transport.hold_unis();
	upstream.client_transport.release_bidis();
	kio::clock::sleep(Duration::from_secs(1)).await;
	assert!(!fetch.is_finished(), "the FETCH was answered before its group landed");
	upstream.server_transport.release_unis();
	let fetched = kio::clock::timeout(TIMEOUT, fetch)
		.await
		.expect("the FETCH is answered")
		.unwrap();
	assert_eq!(fetched.expect("the FETCH gets its group"), Some(b"x".to_vec()));
	assert_eq!(fetches(&upstream), 0, "the run answered the FETCH");
	let (keys, end) = kio::clock::timeout(TIMEOUT, read)
		.await
		.expect("the reader reaches the end of its range")
		.unwrap();
	assert_eq!(keys, [1, 2, 3]);
	assert!(end.is_ok(), "the range ends cleanly: {end:?}");
	drop((broadcast, upstream, publisher, relay, finished, first));
}

/// A relay copy recovers runs it evicted from a relay that still holds every group, so gave
/// none up. That relay starts each run's SUBSCRIBE at the run's first key, which names no
/// group, and the copy still asks each run once rather than one key at a time.
#[kio::test]
async fn a_relay_start_naming_no_group_costs_no_subscribe_per_key() {
	const TIMEOUT: Duration = Duration::from_secs(600);
	// The publisher and the first relay hold their groups for good; the last relay's cache
	// expires them.
	let publisher = holding(1);
	let broadcast = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(moq_net::Timescale::MICRO)
				.with_max_age(None),
		)
		.unwrap();
	broadcast.announce().unwrap();
	let mut finished = Vec::new();
	for index in 0..6 {
		let mut group = track
			.create_group(moq_net::group::Info { start: index * GROUP })
			.unwrap();
		group
			.write_frame(Timestamp::from_micros(index * GROUP).unwrap(), &b"x"[..])
			.unwrap();
		finished.push(group.finish().unwrap());
	}

	let middle = holding(2);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(middle.clone());
	let first_link = connect_mock(options).await;
	let relay = origin(3);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	// Each request costs a round trip, as on a network, so a walk shows within the run.
	options.latency = Duration::from_millis(5);
	options.server_publish = Some(middle.consume());
	options.client_subscribe = Some(relay.clone());
	let last_link = connect_mock(options).await;
	let remote = relay
		.consume()
		.follow("bcast", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let history = |ranges: track::Range| {
		let track = remote.track("video").unwrap();
		async move {
			track
				.subscribe(
					Subscription::default()
						.with_delay(Delay::default().with_max(None))
						.with_ranges([ranges]),
				)
				.await
				.unwrap()
		}
	};

	// The live subscription delivers every group, which the last relay then expires while a
	// reader keeps it live.
	let mut live = history(track::Range::default()).await;
	for _ in 0..6 {
		kio::clock::timeout(TIMEOUT, live.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;

	// A reader from just past the first group misses the run up to the fourth.
	let before = subscribes(&last_link);
	let mut second = history(support::keys(1..4 * GROUP)).await;
	let read = kio::clock::sim::spawn(async move {
		let mut keys = Vec::new();
		while let Some(group) = second.recv_group().await.unwrap() {
			keys.push(group.start);
		}
		keys.sort();
		keys
	});
	// The middle relay can't prove the keys between its groups empty, so the runs settle once
	// the publisher ends the track and the middle relay drops each key it never served below
	// the run's cap.
	kio::clock::sleep(Duration::from_secs(10)).await;
	let _ended = track.finish().unwrap();
	let keys = kio::clock::timeout(TIMEOUT, read)
		.await
		.expect("the reader reaches the end of its range")
		.unwrap();
	assert_eq!(keys, [GROUP, 2 * GROUP, 3 * GROUP]);
	let asked = subscribes(&last_link) - before;
	// Three groups and the four gaps around them: a run's cap at the group its START named
	// would walk the gaps a key at a time.
	assert!(asked <= 2 * 7, "{asked} SUBSCRIBEs for three groups and four gaps");
	assert_eq!(fetches(&last_link), 0, "nothing fetched");
	drop((
		broadcast, first_link, last_link, publisher, middle, relay, finished, live,
	));
}

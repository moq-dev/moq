#![cfg(feature = "async")]

//! A lite-06 relay copy of a timestamp-keyed track whose publisher never says which keys
//! between its group starts are empty, and resets its first groups as `Old` after their
//! headers went out: an ordered reader on the relay reaches the later groups, and the copy
//! asks upstream about the keys between group starts once per run, not once per key, on each
//! hop of a relay chain too. A run still asked when the publisher ends the track is answered
//! by upstream before the copy finishes.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::track::Subscription;
use moq_net::{Error, Timestamp, Version, group, origin, track};
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
			let subscription = Subscription::default().with_max(Duration::from_secs(30));
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
				.subscribe(Subscription::default().with_ranges([ranges]))
				.await
				.unwrap()
		}
	};

	// The live subscription delivers every group, which the copy then expires while a reader
	// keeps it live.
	let mut first = history(track::Range::ALL).await;
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
	// land only after that FIN, within the grace, as QUIC may deliver them. The FETCH and the reader still get
	// the run's groups.
	upstream.server_transport.hold_unis();
	upstream.client_transport.release_bidis();
	kio::clock::sleep(Duration::from_millis(500)).await;
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

/// Publisher → relay A, which by default holds every group for good → relay B, whose cache
/// expires them, over lite-06, with a reader's view of B.
struct Chain {
	track: track::Producer,
	remote: moq_net::broadcast::Consumer,
	/// B's link to A, where B's requests go.
	last_link: support::harness::MockPair,
	_held: (
		origin::Producer,
		origin::Producer,
		origin::Producer,
		moq_net::origin::Route,
		support::harness::MockPair,
	),
}

impl Chain {
	async fn new(info: track::Info) -> Self {
		Self::with_upstream(info, Duration::ZERO, holding(2)).await
	}

	/// A chain through `middle` as A, whose hop from the publisher has a one-way `latency`.
	async fn with_upstream(info: track::Info, latency: Duration, middle: origin::Producer) -> Self {
		let publisher = holding(1);
		let route = publisher.route("bcast", origin::RouteOptions::hidden()).unwrap();
		let track = route.broadcast().create_track("video", info).unwrap();
		route.announce().unwrap();
		let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
		options.latency = latency;
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
		Self {
			track,
			remote,
			last_link,
			_held: (publisher, middle, relay, route, first_link),
		}
	}

	/// A group at `key` holding one frame at `at`, finished.
	fn write(&self, key: u64, at: Timestamp) -> group::Finished {
		let mut group = self.track.create_group(group::Info { start: key }).unwrap();
		group.write_frame(at, &b"x"[..]).unwrap();
		group.finish().unwrap()
	}

	/// A read of B with no max delay over `ranges`.
	async fn read(&self, ranges: track::Range) -> track::Subscriber {
		self.remote
			.track("video")
			.unwrap()
			.subscribe(Subscription::default().with_ranges([ranges]))
			.await
			.unwrap()
	}
}

/// Every group `reader` gets until its range ends, sorted.
async fn keys(mut reader: track::Subscriber) -> Vec<u64> {
	let mut keys = Vec::new();
	while let Some(group) = reader.recv_group().await.unwrap() {
		keys.push(group.start);
	}
	keys.sort();
	keys
}

/// Relay B recovers runs it evicted from relay A, which still holds every group so gave none
/// up, and starts each run's SUBSCRIBE at the run's first key, which names no group. A asks its
/// own upstream about the keys it can't prove empty, so the reader's range ends while the track
/// is still live, and B asks each run once rather than one key at a time.
#[kio::test]
async fn a_relay_chain_settles_an_evicted_run_live() {
	const TIMEOUT: Duration = Duration::from_secs(60);
	let chain = Chain::new(
		track::Info::default()
			.with_timescale(moq_net::Timescale::MICRO)
			.with_max_age(None),
	)
	.await;
	let _written: Vec<_> = (0..6)
		.map(|index| chain.write(index * GROUP, Timestamp::from_micros(index * GROUP).unwrap()))
		.collect();

	// The live subscription delivers every group, which B then expires while a reader keeps it
	// live.
	let mut live = chain.read(track::Range::ALL).await;
	for _ in 0..6 {
		kio::clock::timeout(TIMEOUT, live.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;

	// A reader from just past the first group misses the run up to the fourth.
	let before = subscribes(&chain.last_link);
	let read = chain.read(support::keys(1..4 * GROUP)).await;
	let keys = kio::clock::timeout(TIMEOUT, keys(read))
		.await
		.expect("the reader reaches the end of its range while the track is live");
	assert_eq!(keys, [GROUP, 2 * GROUP, 3 * GROUP]);
	// Three groups and the four gaps around them: a run's cap at the group its START named
	// would walk the gaps a key at a time.
	let asked = subscribes(&chain.last_link) - before;
	assert!(asked <= 2 * 7, "{asked} SUBSCRIBEs for three groups and four gaps");
	assert_eq!(fetches(&chain.last_link), 0, "nothing fetched");
}

/// A sequence-keyed publisher skips groups 2 and 3 without saying so. A reader of relay B with
/// no max delay waits out the gap, which A can't prove empty either: A asks its own upstream,
/// so the reader's range ends while the track is still live.
#[kio::test]
async fn a_relay_chain_settles_a_small_undeclared_gap_live() {
	const TIMEOUT: Duration = Duration::from_secs(60);
	let chain = Chain::new(
		track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None),
	)
	.await;
	let read = chain.read(support::keys(0..6)).await;
	let _written: Vec<_> = [0, 1, 4, 5]
		.into_iter()
		.map(|key| chain.write(key, Timestamp::from_millis(key * 1000).unwrap()))
		.collect();
	let keys = kio::clock::timeout(TIMEOUT, keys(read))
		.await
		.expect("the reader reaches the end of its range while the track is live");
	assert_eq!(keys, [0, 1, 4, 5]);
}

/// Relay B recovers a run it evicted from relay A, which evicted it too, so A asks the
/// publisher over a slow hop before it can answer the keys below the first group it sends.
/// Each of A's answers lands within the grace of the last, so B waits on them rather than
/// taking A for a relay of the previous release and fetching the run a key at a time.
#[kio::test]
async fn a_slow_relay_still_answering_is_not_walked() {
	const TIMEOUT: Duration = Duration::from_secs(20);
	let chain = Chain::with_upstream(
		track::Info::default()
			.with_timescale(moq_net::Timescale::MICRO)
			.with_max_age(None),
		Duration::from_millis(300),
		origin(2),
	)
	.await;
	let _written: Vec<_> = (0..6)
		.map(|index| chain.write(index * GROUP, Timestamp::from_micros(index * GROUP).unwrap()))
		.collect();
	let mut live = chain.read(track::Range::ALL).await;
	for _ in 0..6 {
		kio::clock::timeout(TIMEOUT, live.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
	}
	kio::clock::sleep(moq_net::cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;

	let read = chain.read(support::keys(1..4 * GROUP)).await;
	let keys = kio::clock::timeout(TIMEOUT, keys(read))
		.await
		.expect("the reader reaches the end of its range");
	assert_eq!(keys, [GROUP, 2 * GROUP, 3 * GROUP]);
	assert_eq!(fetches(&chain.last_link), 0, "nothing fetched");
}

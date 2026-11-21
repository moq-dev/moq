#![cfg(feature = "async")]

//! A live join stating a Min Age starts that far behind the live edge, through relays that
//! hold nothing yet, and through a relay copy that holds less than it reaches back for. A
//! range with no start and no Min Age reads from the oldest group instead.

use std::time::Duration;

use moq_net::{Timestamp, Version, cache, origin, track};

use crate::support::harness::{MockPair, peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = cache::Pool::unbounded();
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin.with_proxy(true)
}

/// Append the 500 ms group starting at `start`, presenting there.
fn append(source: &track::Producer, start: u64) {
	let start = Timestamp::from_millis(start).unwrap();
	let mut group = source.append_group(start).unwrap();
	group.write_frame(start, &b"x"[..]).unwrap();
	group.finish().unwrap();
}

/// A viewer joining through two relays that hold nothing gets the four groups behind the
/// live group its 2 s Min Age asks for, oldest first, then plays on to live with no gap.
#[kio::test]
async fn a_cold_relay_chain_reaches_back_by_min_age() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = origin(1);
	let edge = origin(2);
	let inner = origin(3);
	let viewer = origin(4);
	let _a = peer(version, &publisher, &inner).await;
	let _b = peer(version, &inner, &edge).await;
	let _c = peer(version, &edge, &viewer).await;

	let broadcast = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let source = broadcast.broadcast().create_track("video", info).unwrap();
	for start in (0..=5000).step_by(500) {
		append(&source, start);
	}
	broadcast.announce().unwrap();

	let joining = track::Subscription::default()
		.with_order(track::Order::Ascending)
		.with_delay(
			track::Delay::default()
				.with_min(Duration::from_secs(2))
				.with_max(Duration::from_secs(30)),
		);
	kio::clock::timeout(TIMEOUT, async {
		let remote = viewer.consume().follow("live", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		let mut subscriber = remote.track("video").unwrap().subscribe(joining).await.unwrap();
		let mut starts = Vec::new();
		while starts.len() < 5 {
			starts.push(subscriber.recv_group().await.unwrap().expect("a group").start);
		}
		assert_eq!(starts, [3000, 3500, 4000, 4500, 5000]);
		append(&source, 5500);
		assert_eq!(subscriber.recv_group().await.unwrap().expect("a group").start, 5500);
	})
	.await
	.expect("the viewer read its reach back");
}

/// A publisher with groups every 500 ms from 0 through 5 s, one relay peered from it over
/// lite-07, and the publisher's own track to watch what it is asked for.
struct Chain {
	publisher: origin::Producer,
	relay: origin::Producer,
	source: track::Producer,
	_route: origin::Route,
	_peer: MockPair,
}

impl Chain {
	async fn new() -> Self {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let publisher = origin(1);
		let relay = origin(2);
		let peer = peer(version, &publisher, &relay).await;
		let route = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
		let info = track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None);
		let source = route.broadcast().create_track("video", info).unwrap();
		for start in (0..=5000).step_by(500) {
			append(&source, start);
		}
		route.announce().unwrap();
		Self {
			publisher,
			relay,
			source,
			_route: route,
			_peer: peer,
		}
	}

	/// A reader of the relay's copy.
	async fn relay(&self, subscription: track::Subscription) -> track::Ordered {
		let remote = self.relay.consume().follow("live", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap()
			.ordered()
	}

	/// A reader of the publisher's own track.
	async fn origin(&self, subscription: track::Subscription) -> track::Ordered {
		let remote = self.publisher.consume().request("live", None).await;
		let remote = remote.expect("the broadcast resolves");
		remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap()
			.ordered()
	}

	/// Wait until the publisher is asked for a Min Age of `min`.
	async fn asked(&self, min: Option<Duration>) {
		let mut source = self.source.clone();
		while source
			.subscription()
			.is_none_or(|subscription| subscription.delay.min != min)
		{
			source.subscription_changed().await.unwrap();
		}
	}
}

/// Reading with Min Age `min`, unstated for `None`, from a range with no start.
fn joining(min: Option<Duration>) -> track::Subscription {
	track::Subscription::default()
		.with_order(track::Order::Ascending)
		.with_delay(track::Delay::default().with_min(min).with_max(Duration::from_secs(30)))
}

/// The first group `reader` reads in key order.
async fn first(reader: &mut track::Ordered) -> u64 {
	reader.next_group().await.unwrap().expect("a group").start
}

/// A live join whose Min Age reaches past what a relay copy holds reads the same groups from
/// the copy as from the origin: the copy, live since a reader with a Min Age of zero, holds only
/// the newest group, so it asks the publisher again with the larger Min Age and waits for its
/// answer instead of starting at what it happens to hold.
#[kio::test]
async fn a_join_past_what_a_relay_copy_holds_reads_as_the_origin_does() {
	let chain = Chain::new().await;
	kio::clock::timeout(TIMEOUT, async {
		let mut live = chain.relay(joining(Some(Duration::ZERO))).await;
		assert_eq!(
			first(&mut live).await,
			5000,
			"the live reader starts at the latest group"
		);
		chain.asked(Some(Duration::ZERO)).await;

		let mut relayed = chain.relay(joining(Some(Duration::from_secs(2)))).await;
		chain.asked(Some(Duration::from_secs(2))).await;
		let relayed = first(&mut relayed).await;

		let mut direct = chain.origin(joining(Some(Duration::from_secs(2)))).await;
		let direct = first(&mut direct).await;
		assert_eq!(relayed, direct, "the copy starts the join where the origin does");
		assert_eq!(direct, 3000, "2 s behind the live edge at 5 s");
	})
	.await
	.expect("both joins read their start");
}

/// An oldest reader, a range with no start and no Min Age, and a live reader with a Min Age of
/// zero on one relay each get their own start, whichever subscribes first: the oldest group
/// held and the latest one. The live reader doesn't cut the oldest reader's backlog off.
#[kio::test]
async fn an_oldest_and_a_live_reader_on_one_relay_each_get_their_own_start() {
	for oldest_first in [true, false] {
		let chain = Chain::new().await;
		kio::clock::timeout(TIMEOUT, async {
			let (mut oldest, mut live) = match oldest_first {
				true => {
					let oldest = chain.relay(joining(None)).await;
					(oldest, chain.relay(joining(Some(Duration::ZERO))).await)
				}
				false => {
					let live = chain.relay(joining(Some(Duration::ZERO))).await;
					(chain.relay(joining(None)).await, live)
				}
			};
			assert_eq!(
				first(&mut oldest).await,
				0,
				"oldest_first={oldest_first}: the oldest reader"
			);
			assert_eq!(
				first(&mut live).await,
				5000,
				"oldest_first={oldest_first}: the live reader"
			);
		})
		.await
		.expect("both readers read their start");
	}
}

/// A live join whose range ends before the group it joins at reads nothing and ends, through a
/// relay copy as at the origin: the publisher answers the copy's range empty, which says the
/// join starts past its end.
#[kio::test]
async fn a_join_ending_before_its_start_ends_empty_through_a_relay() {
	let chain = Chain::new().await;
	let bounded = || {
		joining(Some(Duration::from_secs(1))).with_ranges([track::Range {
			start: None,
			end: Some(1000),
		}])
	};
	kio::clock::timeout(TIMEOUT, async {
		let mut direct = chain.origin(bounded()).await;
		assert!(
			direct.next_group().await.unwrap().is_none(),
			"the origin delivers nothing"
		);
		let mut relayed = chain.relay(bounded()).await;
		assert!(relayed.next_group().await.unwrap().is_none(), "nor does the copy");

		// That empty answer places no reader whose range reaches past its end: one unbounded,
		// with the same Min Age, joins where the origin does.
		let mut open = chain.relay(joining(Some(Duration::from_secs(1)))).await;
		assert_eq!(first(&mut open).await, 4000, "1 s behind the live edge at 5 s");
		drop(relayed);
	})
	.await
	.expect("both bounded joins end");
}

/// A live join on a relay copy reopened while idle reaches back by its Min Age, as at the
/// origin: reopening asks the publisher for the join, not just the newest group the copy kept.
#[kio::test]
async fn a_join_on_an_idle_copy_reaches_back_by_its_min_age() {
	let chain = Chain::new().await;
	kio::clock::timeout(TIMEOUT, async {
		let mut live = chain.relay(joining(Some(Duration::ZERO))).await;
		assert_eq!(first(&mut live).await, 5000);
		drop(live);
		// The copy goes idle once its upstream subscription ends, keeping what it cached.
		let mut source = chain.source.clone();
		while source.subscription().is_some() {
			source.subscription_changed().await.unwrap();
		}
		let mut relayed = chain.relay(joining(Some(Duration::from_secs(2)))).await;
		assert_eq!(first(&mut relayed).await, 3000, "2 s behind the live edge at 5 s");
	})
	.await
	.expect("the join read its start");
}

/// A live join on a relay copy reopened while idle reaches back by its Min Age when it opens
/// together with a reader that leaves its own unstated, on a track that writes nothing more:
/// the unstated reader hides the join's Min Age from the aggregate, which must not reopen the
/// copy from just the newest group it kept.
#[kio::test]
async fn a_join_on_an_idle_copy_opened_with_an_unstated_reader_reaches_back() {
	let chain = Chain::new().await;
	kio::clock::timeout(TIMEOUT, async {
		let mut live = chain.relay(joining(Some(Duration::ZERO))).await;
		assert_eq!(first(&mut live).await, 5000);
		drop(live);
		let mut source = chain.source.clone();
		while source.subscription().is_some() {
			source.subscription_changed().await.unwrap();
		}
		// Both subscribe before the session reads the demand, so one SUBSCRIBE opens for both.
		let remote = chain.relay.consume().follow("live", None).unwrap().available().await;
		let remote = remote.unwrap().expect("the broadcast resolves");
		let track = remote.track("video").unwrap();
		let (oldest, joined) = futures::join!(
			track.subscribe(joining(None)),
			track.subscribe(joining(Some(Duration::from_secs(2))))
		);
		let (mut oldest, mut joined) = (oldest.unwrap().ordered(), joined.unwrap().ordered());
		assert_eq!(first(&mut joined).await, 3000, "2 s behind the live edge at 5 s");
		assert_eq!(first(&mut oldest).await, 0, "the unstated reader reads from the oldest");
	})
	.await
	.expect("both readers read their start");
}

/// A live join on a relay copy that went idle measures the live edge from its publisher's
/// answer, not from the groups the ended feed left: the publisher moved on meanwhile, and the
/// groups the copy missed would leave a hole after its stale ones.
#[kio::test]
async fn a_join_on_an_idle_copy_waits_for_the_live_edge() {
	let chain = Chain::new().await;
	kio::clock::timeout(TIMEOUT, async {
		let mut oldest = chain.relay(joining(None)).await;
		for start in (0..=5000).step_by(500) {
			assert_eq!(first(&mut oldest).await, start);
		}
		drop(oldest);
		let mut source = chain.source.clone();
		while source.subscription().is_some() {
			source.subscription_changed().await.unwrap();
		}
		for start in (5500..=8000).step_by(500) {
			append(&chain.source, start);
		}
		let mut relayed = chain.relay(joining(Some(Duration::from_secs(2)))).await;
		for start in (6000..=8000).step_by(500) {
			assert_eq!(first(&mut relayed).await, start, "2 s behind the live edge at 8 s");
		}
	})
	.await
	.expect("the join read on to live");
}

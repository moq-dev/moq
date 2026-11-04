#![cfg(feature = "async")]

//! A copy fills a miss of more than one group over moq-lite-07 through a subscription rather
//! than FETCH, and recovers on a SUBSCRIBE of its own what its live subscription will never
//! send again.

use crate::support;

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::track::{Delay, Info, Range, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version, cache, kio};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// SUBSCRIBE's and FETCH's lite stream types, each one varint byte.
const SUBSCRIBE: u8 = 2;
const FETCH: u8 = 3;

fn origin(hop: u64, pool: Option<cache::Pool>) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(Hop::new(hop).unwrap());
	if let Some(pool) = pool {
		config.pool = pool;
	}
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

/// The publisher's groups, one per second, with no publisher max age.
struct Fixture {
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	pair: MockPair,
	_broadcast: moq_net::origin::Publication,
	_origins: (moq_net::origin::Producer, moq_net::origin::Producer),
}

impl Fixture {
	/// A lite-07 publisher holding `groups` for good, and a copy whose cache expires them.
	async fn new(groups: u64) -> Self {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track(
				"video",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
			)
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		for sequence in 0..groups {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(sequence * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let copy = origin(2, None);
		let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(copy.clone());
		let pair = connect_mock(options).await;
		let remote = copy.consume().follow("bcast", None).unwrap().available().await.unwrap();
		Self {
			track,
			remote,
			pair,
			_broadcast: broadcast,
			_origins: (publisher, copy),
		}
	}

	/// How many streams of `kind` the copy opened upstream.
	fn opened(&self, kind: u8) -> usize {
		self.pair
			.client_transport
			.bidi_types()
			.into_iter()
			.filter(|opened| *opened == kind)
			.count()
	}

	/// A reader with no budget, of `ranges`.
	async fn subscribe(&self, ranges: impl IntoIterator<Item = Range>) -> moq_net::track::Subscriber {
		self.remote
			.track("video")
			.unwrap()
			.subscribe(
				Subscription::default()
					.with_delay(Delay::default().with_max(None))
					.with_ranges(ranges),
			)
			.await
			.unwrap()
	}
}

/// The sequences of the next `count` groups `reader` receives.
async fn receive(reader: &mut moq_net::track::Subscriber, count: usize) -> BTreeSet<u64> {
	let mut got = BTreeSet::new();
	while got.len() < count {
		let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		got.insert(group.sequence);
	}
	got
}

/// History the live subscription already delivered, and the copy's cache has since evicted,
/// is never sent again on that subscription. A reader reaching back for it gets it from one
/// SUBSCRIBE of its own, not a FETCH per group.
#[kio::test]
async fn evicted_history_is_recovered_once_on_its_own_subscription() {
	kio::clock::timeout(TIMEOUT + cache::DEFAULT_EXPIRY * 6, async {
		let fixture = Fixture::new(6).await;
		let mut first = fixture.subscribe([Range::oldest()]).await;
		assert_eq!(receive(&mut first, 6).await, (0..6).collect());

		// Superseded, the copy expires them, while the reader keeps the subscription live.
		kio::clock::sleep(cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
		let subscribes = fixture.opened(SUBSCRIBE);
		let mut second = fixture.subscribe([Range::groups(1..4)]).await;
		assert_eq!(receive(&mut second, 3).await, (1..4).collect());
		assert_eq!(fixture.opened(SUBSCRIBE), subscribes + 1, "recovered on one SUBSCRIBE");
		assert_eq!(fixture.opened(FETCH), 0, "nothing fetched");

		// The live subscription goes on.
		let mut group = fixture
			.track
			.create_group(moq_net::group::Info { sequence: 6 })
			.unwrap();
		group
			.write_frame(Timestamp::from_millis(6000).unwrap(), b"x".as_slice())
			.unwrap();
		group.finish().unwrap();
		assert_eq!(receive(&mut first, 1).await, [6].into());
		assert_eq!(fixture.opened(SUBSCRIBE), subscribes + 1, "recovered once");

		// Expired again, the history is recovered again for a later reader.
		drop(second);
		kio::clock::sleep(cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;
		let mut third = fixture.subscribe([Range::groups(1..4)]).await;
		assert_eq!(receive(&mut third, 3).await, (1..4).collect());
		assert_eq!(
			fixture.opened(SUBSCRIBE),
			subscribes + 2,
			"recovered for the later reader"
		);
	})
	.await
	.expect("timed out");
}

/// One group, or a slice of one, is what lite-07 FETCH carries, so a miss of one group still
/// fetches it.
#[kio::test]
async fn a_one_group_miss_still_fetches() {
	kio::clock::timeout(TIMEOUT, async {
		let fixture = Fixture::new(3).await;
		let mut group = kio::clock::timeout(TIMEOUT, fixture.remote.track("video").unwrap().fetch_group(1, None))
			.await
			.expect("fetch timeout")
			.expect("fetched");
		let frame = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(frame.payload.as_ref(), b"x");
		assert_eq!(fixture.opened(FETCH), 1);
		assert_eq!(fixture.opened(SUBSCRIBE), 0, "no subscription for one group");
	})
	.await
	.expect("timed out");
}

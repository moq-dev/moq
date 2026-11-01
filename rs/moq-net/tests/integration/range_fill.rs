#![cfg(feature = "async")]

//! A copy fills a group its cache misses over moq-lite-07 by subscribing to the missing
//! range, never with FETCH, which older versions still use.

use crate::support;

use std::time::Duration;

use moq_net::stats::{Message, Registry, Tier};
use moq_net::track::{Info, Range, Subscription};
use moq_net::{Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

const LITE07: &str = "moq-lite-07-wip";

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	group
		.write_frame(
			Timestamp::from_millis(sequence * 1000).unwrap(),
			format!("{sequence}").into_bytes(),
		)
		.unwrap();
	group.finish().unwrap();
}

struct Fixture {
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	registry: Registry,
	tier: Tier,
	_broadcast: moq_net::origin::Publication,
	_pair: support::harness::MockPair,
	_origins: (moq_net::origin::Producer, moq_net::origin::Producer),
}

impl Fixture {
	/// A publisher on `version` holding `groups`, and the subscriber's view of it, whose
	/// session counts its control bytes.
	async fn new(version: &str, groups: &[u64]) -> Self {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast
			.create_track("video", Info::default().with_timescale(Timescale::MILLI))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		for sequence in groups {
			write_group(&track, *sequence);
		}

		let subscriber = produce_origin(2);
		let registry = Registry::new(Default::default());
		let tier = Tier::new(moq_net::Path::from_static("internal"));
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		options.server_stats = registry.tier(tier.clone()).session(moq_net::Path::from_static(""));
		let pair = connect_mock(options).await;
		let remote = moq_clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		Self {
			track,
			remote,
			registry,
			tier,
			_broadcast: broadcast,
			_pair: pair,
			_origins: (publisher, subscriber),
		}
	}

	/// The control bytes this session sent for `class`.
	fn sent(&self, class: Message) -> u64 {
		self.registry
			.snapshot()
			.control()
			.iter()
			.filter(|(tier, row, _)| *tier == self.tier && *row == class)
			.map(|(_, _, bytes)| bytes.tx)
			.sum()
	}

	/// Fetch group `sequence` and read its one frame.
	async fn fetch(&self, sequence: u64) -> moq_net::Result<String> {
		let fetching = self.remote.track("video").unwrap().fetch_group(sequence, None);
		let mut group = moq_clock::timeout(TIMEOUT, fetching).await.expect("fetch timed out")?;
		let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read timed out")?
			.expect("a frame");
		Ok(String::from_utf8(frame.payload.to_vec()).unwrap())
	}
}

/// With no subscription open, a fetch of an uncached group subscribes to just that group,
/// and the publisher ends the subscription once it covers it. Lite-06 fetches it instead.
#[moq_clock::test]
async fn a_miss_subscribes_to_its_group() {
	for version in ["moq-lite-06", LITE07] {
		let fixture = Fixture::new(version, &[0, 1, 2]).await;
		assert_eq!(fixture.fetch(1).await.expect("fetched"), "1", "{version}");

		let (fetch, subscribe) = (fixture.sent(Message::Fetch), fixture.sent(Message::Subscribe));
		if version != LITE07 {
			assert!(fetch > 0, "{version}: fetched");
			continue;
		}
		assert_eq!(fetch, 0, "{version}: no FETCH");
		assert!(subscribe > 0, "{version}: subscribed");
		moq_clock::timeout(TIMEOUT, support::unwanted(fixture.track.demand()))
			.await
			.expect("the covered subscription ends")
			.unwrap();

		// Cached now, so a second fetch asks nothing more.
		assert_eq!(fixture.fetch(1).await.expect("cached"), "1");
		assert_eq!(fixture.sent(Message::Subscribe), subscribe, "served from the cache");
	}
}

/// A miss beside a live subscription rides it as one more range, and the live feed goes on.
#[moq_clock::test]
async fn a_miss_rides_the_live_subscription() {
	let fixture = Fixture::new(LITE07, &[0, 1, 2]).await;
	let mut live = fixture
		.remote
		.track("video")
		.unwrap()
		.subscribe(Subscription::default().with_ranges([Range::live()]))
		.await
		.expect("subscribe");
	let first = moq_clock::timeout(TIMEOUT, live.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	assert_eq!(first.sequence, 2);

	assert_eq!(fixture.fetch(0).await.expect("fetched"), "0");
	assert_eq!(fixture.sent(Message::Fetch), 0, "no FETCH");

	write_group(&fixture.track, 3);
	let next = moq_clock::timeout(TIMEOUT, live.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	assert_eq!(next.sequence, 3, "the live feed goes on");
	let subscription = fixture.track.subscription().expect("still subscribed");
	assert_eq!(subscription.ranges, [Range::live()], "the filled range is retired");
}

/// A group the publisher never had is a miss, named by the subscription it was asked on
/// once the publisher's feed gives up on it, past the budget's grace.
#[moq_clock::test]
async fn a_group_the_publisher_never_had_is_a_miss() {
	let fixture = Fixture::new(LITE07, &[0, 2, 3]).await;
	let fetching = fixture.remote.track("video").unwrap().fetch_group(
		1,
		moq_net::group::Fetch::default().with_max_delay(Duration::from_secs(1)),
	);
	let err = match moq_clock::timeout(Duration::from_secs(120), fetching)
		.await
		.expect("fetch timed out")
	{
		Ok(_) => panic!("group 1 was served"),
		Err(err) => err,
	};
	assert!(matches!(err, moq_net::Error::NotFound), "{err}");
	assert_eq!(fixture.sent(Message::Fetch), 0, "no FETCH");
}

/// A group whose stream an earlier subscription cut is filled whole by the next one,
/// rather than judged missing from what the earlier one left behind.
#[moq_clock::test]
async fn a_cut_group_is_filled_whole() {
	let fixture = Fixture::new(LITE07, &[]).await;
	let mut open = fixture
		.track
		.create_group(moq_net::group::Info { sequence: 0 })
		.unwrap();
	open.write_frame(Timestamp::ZERO, b"head".as_slice()).unwrap();

	let mut live = fixture
		.remote
		.track("video")
		.unwrap()
		.subscribe(Subscription::default().with_ranges([Range::live()]))
		.await
		.expect("subscribe");
	let mut group = moq_clock::timeout(TIMEOUT, live.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read timed out")
		.unwrap()
		.expect("a frame");
	assert_eq!(frame.payload.as_ref(), b"head");
	// The only reader leaves mid-group, so the subscription goes and cuts the copy.
	drop((group, live));
	moq_clock::timeout(TIMEOUT, support::unwanted(fixture.track.demand()))
		.await
		.expect("the subscription ends")
		.unwrap();

	open.write_frame(Timestamp::from_millis(1).unwrap(), b"tail".as_slice())
		.unwrap();
	open.finish().unwrap();
	let mut fetched = moq_clock::timeout(TIMEOUT, fixture.remote.track("video").unwrap().fetch_group(0, None))
		.await
		.expect("fetch timed out")
		.expect("fetched");
	let mut frames = Vec::new();
	while let Some(frame) = moq_clock::timeout(TIMEOUT, fetched.read_frame())
		.await
		.expect("read timed out")
		.expect("read")
	{
		frames.push(frame.payload.to_vec());
	}
	assert_eq!(frames, [b"head".to_vec(), b"tail".to_vec()]);
	assert_eq!(fixture.sent(Message::Fetch), 0, "no FETCH");
}

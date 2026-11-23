#![cfg(feature = "async")]

//! A lite-07 publisher, a relay, and two viewers, the way the interop replacement round
//! drives them: the publisher writes group 66, a viewer fetches group 67, which the publisher
//! serves only from a fill, and a viewer then subscribes from time 66.

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::track::{Item, Order, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

/// The group only a fill serves.
const FILLED: u64 = 67;
/// The group the time start resolves to, written live.
const LIVE: u64 = 66;

fn origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

async fn link(from: &moq_net::origin::Producer, to: &moq_net::origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

fn write(mut group: moq_net::group::Producer) {
	let key = group.start;
	group
		.write_frame(Timestamp::unscaled(key), key.to_string().into_bytes())
		.unwrap();
	group.finish().unwrap();
}

struct Fixture {
	track: moq_net::track::Producer,
	viewers: [moq_net::broadcast::Consumer; 2],
	serving: kio::clock::sim::JoinHandle<()>,
	_broadcast: moq_net::origin::Route,
	_links: Vec<MockPair>,
	_origins: Vec<moq_net::origin::Producer>,
}

impl Fixture {
	/// A publisher whose scale-less track holds nothing, serving [`FILLED`] from a fill and
	/// dropping every other fill, a relay, and two viewers of it.
	async fn new() -> Self {
		let publisher = origin(1);
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track("value", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		broadcast.announce().unwrap();
		let requests = track.requests();
		let serving = kio::clock::sim::spawn(async move {
			while let Ok(fill) = requests.requested_fill().await {
				if (fill.start().group..fill.end()).contains(&FILLED)
					&& let Ok(Some(group)) = fill.accept(FILLED, None)
				{
					write(group);
				}
			}
		});

		let relay = origin(2);
		let mut links = vec![link(&publisher, &relay).await];
		let mut viewers = Vec::new();
		let mut origins = vec![publisher, relay.clone()];
		for hop in 3..5 {
			let viewer = origin(hop);
			links.push(link(&relay, &viewer).await);
			let remote = kio::clock::timeout(TIMEOUT, viewer.consume().follow("bcast", None).unwrap().available())
				.await
				.expect("resolve timeout")
				.expect("broadcast resolves")
				.unwrap();
			viewers.push(remote);
			origins.push(viewer);
		}
		Self {
			track,
			viewers: viewers.try_into().map_err(drop).unwrap(),
			serving,
			_broadcast: broadcast,
			_links: links,
			_origins: origins,
		}
	}

	/// Fetch [`FILLED`] on viewer `index` and read it whole.
	async fn fetch(&self, index: usize) {
		let mut fetched = kio::clock::timeout(
			TIMEOUT,
			self.viewers[index].track("value").unwrap().fetch_group(FILLED, None),
		)
		.await
		.expect("fetch timeout")
		.unwrap();
		let frame = kio::clock::timeout(TIMEOUT, fetched.read_frame())
			.await
			.expect("read timeout")
			.unwrap()
			.expect("a frame");
		assert_eq!(&frame.payload[..], FILLED.to_string().as_bytes());
	}

	/// An ordered reader on viewer `index` from time [`LIVE`], with no budget.
	async fn subscribe(&self, index: usize) -> moq_net::track::Ordered {
		let subscription = Subscription::default()
			.with_max(None)
			.with_order(Order::Ascending)
			.with_ranges([Range {
				start: moq_net::track::Start::Time(LIVE),
				end: None,
			}]);
		kio::clock::timeout(
			TIMEOUT,
			self.viewers[index].track("value").unwrap().subscribe(subscription),
		)
		.await
		.expect("subscribe timeout")
		.unwrap()
		.ordered()
	}

	/// Write [`LIVE`] at the publisher.
	fn write_live(&self) {
		write(self.track.create_group(moq_net::group::Info { start: LIVE }).unwrap());
	}
}

/// The keys of the next `count` items `reader` returns, a gap as its range.
async fn read(reader: &mut moq_net::track::Ordered, count: usize) -> Vec<Result<u64, std::ops::Range<u64>>> {
	let mut items = Vec::new();
	while items.len() < count {
		let item = kio::clock::timeout(TIMEOUT, reader.next_item())
			.await
			.unwrap_or_else(|_| panic!("timed out after {items:?}"))
			.unwrap()
			.expect("an item");
		items.push(match item {
			Item::Group(group) => Ok(group.start),
			Item::Gap { range, .. } => Err(range),
		});
	}
	items
}

/// The starts of the next `count` groups `reader` returns, past any gap.
async fn read_groups(reader: &mut moq_net::track::Ordered, count: usize) -> Vec<u64> {
	let mut groups = Vec::new();
	while groups.len() < count {
		if let Ok(group) = read(reader, 1).await.remove(0) {
			groups.push(group);
		}
	}
	groups
}

/// A viewer that holds [`FILLED`] from its own fetch reads the group its time start resolves
/// to first, though its copy can't place the start until the publisher answers.
#[kio::test]
async fn a_waiting_time_start_reads_its_group_before_a_held_later_one() {
	let fixture = Fixture::new().await;
	fixture.write_live();
	fixture.fetch(0).await;
	let mut reader = fixture.subscribe(0).await;
	assert_eq!(read(&mut reader, 2).await, [Ok(LIVE), Ok(FILLED)]);
	fixture.serving.abort();
}

/// A group the publisher accepted from another viewer's fetch, past the live edge, reaches a
/// subscription covering it.
#[kio::test]
async fn a_filled_group_past_the_live_edge_reaches_a_subscription() {
	let fixture = Fixture::new().await;
	fixture.write_live();
	fixture.fetch(1).await;
	let mut reader = fixture.subscribe(0).await;
	assert_eq!(read(&mut reader, 2).await, [Ok(LIVE), Ok(FILLED)]);
	fixture.serving.abort();
}

/// An unlimited reader asking from a time before every group through a time before the only
/// one, and from that group on, reads that group, as it would at the publisher.
#[kio::test]
async fn a_range_before_every_group_reads_past_it() {
	let fixture = Fixture::new().await;
	fixture.write_live();
	let subscription = Subscription::default()
		.with_max(None)
		.with_order(Order::Ascending)
		.with_ranges([
			Range {
				start: moq_net::track::Start::Time(10),
				end: Some(20),
			},
			Range {
				start: moq_net::track::Start::Time(LIVE),
				end: None,
			},
		]);
	let mut direct = fixture.track.subscribe(subscription.clone()).ordered();
	let direct = read(&mut direct, 1).await;
	let mut reader = kio::clock::timeout(
		TIMEOUT,
		fixture.viewers[0].track("value").unwrap().subscribe(subscription),
	)
	.await
	.expect("subscribe timeout")
	.unwrap()
	.ordered();
	assert_eq!(read(&mut reader, 1).await, direct);
	fixture.serving.abort();
}

/// A copy holding the groups on either side of a time, from fetches, says nothing of a group
/// between them: a reader asking from that time starts where the publisher would.
#[kio::test]
async fn a_time_between_held_groups_starts_where_the_publisher_would() {
	let publisher = origin(1);
	let route = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None);
	let track = route.broadcast().create_track("value", info).unwrap();
	for start in [10, 15, 20] {
		let start = Timestamp::from_millis(start).unwrap();
		let mut group = track.append_group(start).unwrap();
		group.write_frame(start, &b"x"[..]).unwrap();
		group.finish().unwrap();
	}
	route.announce().unwrap();

	let (relay, viewer) = (origin(2), origin(3));
	let links = [link(&publisher, &relay).await, link(&relay, &viewer).await];
	let remote = kio::clock::timeout(TIMEOUT, viewer.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	for key in [10, 20] {
		let fetched = remote.track("value").unwrap().fetch_group(key, None);
		let mut fetched = kio::clock::timeout(TIMEOUT, fetched)
			.await
			.expect("fetch timeout")
			.unwrap();
		kio::clock::timeout(TIMEOUT, fetched.read_frame())
			.await
			.expect("read timeout")
			.unwrap()
			.expect("a frame");
	}

	let from = Subscription::default()
		.with_max(None)
		.with_order(Order::Ascending)
		.with_ranges([Range {
			start: moq_net::track::Start::Time(18),
			end: None,
		}]);
	let mut direct = track.subscribe(from.clone()).ordered();
	assert_eq!(read_groups(&mut direct, 2).await, [15, 20]);
	let subscribing = remote.track("value").unwrap().subscribe(from);
	let mut reader = kio::clock::timeout(TIMEOUT, subscribing)
		.await
		.expect("subscribe timeout")
		.unwrap()
		.ordered();
	assert_eq!(read_groups(&mut reader, 2).await, [15, 20]);
	drop((links, viewer, relay, route, publisher));
}

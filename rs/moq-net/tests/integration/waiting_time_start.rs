#![cfg(feature = "async")]

//! A lite-07 publisher, a relay, and two viewers, the way the interop replacement round
//! drives them: the publisher writes group 66, a viewer fetches group 67, which the publisher
//! serves only from a fill, and a viewer then subscribes from time 66.

use crate::support;

use std::time::Duration;

use moq_net::track::{Anchor, Delay, Item, Order, Range, Subscription};
use moq_net::{Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

/// The group only a fill serves.
const FILLED: u64 = 67;
/// The group the time start resolves to, written live.
const LIVE: u64 = 66;

fn origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
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
	/// dropping every other fill and lookup, a relay, and two viewers of it.
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
				let moq_net::track::Fill::Groups(fill) = fill else {
					continue;
				};
				if (fill.start().group..fill.end()).contains(&FILLED)
					&& let Ok(group) = fill.accept(FILLED, None)
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
			.with_delay(Delay::default().with_max(None))
			.with_order(Order::Ascending)
			.with_ranges([Range {
				start: Anchor::Time(LIVE),
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
		let item = kio::clock::timeout(TIMEOUT, reader.next_group())
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

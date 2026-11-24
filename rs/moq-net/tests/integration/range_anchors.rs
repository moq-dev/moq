#![cfg(feature = "async")]

//! A range anchored at a presentation time resolves where the track is held, and on a copy
//! received over lite-07 the same way. Every wire carries every range, a time as the group ID
//! with that key, but only lite-07 resolves a time start to the group containing it: an older
//! wire's publisher starts at the first group at or after it. A range with no start is placed
//! by the Min Age: a live join that far behind the live edge where lite-07 carries it, the
//! oldest group when it is not stated.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Order, Range, Subscription};
use moq_net::{Error, Timescale, Timestamp, Version};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A budget no group outlives, so only the range decides what is read.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-04",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-18",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn millis(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

/// Groups at 0 to 4 seconds, each keyed by its start in milliseconds, then what a subscriber
/// to `range` reads oldest first: through the publisher's own origin with no `version`,
/// otherwise over one session on it.
async fn read(version: Option<&str>, range: Range) -> Result<Vec<u64>, Error> {
	read_as(version, range, Read::default()).await.map(|read| read.groups)
}

/// How [`read_as`] publishes and reads.
#[derive(Clone, Copy, Default)]
struct Read {
	/// The publisher appends each group, saying it will not go back below it.
	append: bool,
	/// The subscriber reads in key order.
	ordered: bool,
	/// The subscriber's Min Age, which places a range with no start.
	min: Option<Duration>,
}

/// What [`read_as`] read.
#[derive(Debug, PartialEq)]
struct Groups {
	groups: Vec<u64>,
	/// The read ended, rather than waited out [`TIMEOUT`].
	ended: bool,
}

/// [`read`], published and read as `how` says.
async fn read_as(version: Option<&str>, range: Range, how: Read) -> Result<Groups, Error> {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("room", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"data",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce().unwrap();
	for key in (0..5).map(|second| second * 1000) {
		let mut group = match how.append {
			true => track.append_group(millis(key)).unwrap(),
			false => track.create_group(moq_net::group::Info { start: key }).unwrap(),
		};
		group.write_frame(millis(key), &b"x"[..]).unwrap();
		group.finish().unwrap();
	}

	let subscriber = produce_origin(2);
	let _pair;
	let remote = match version {
		// Through the origin, as a reader of a local route.
		None => kio::clock::timeout(TIMEOUT, publisher.consume().request("room", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves"),
		Some(version) => {
			_pair = peer(version.parse::<Version>().unwrap(), &publisher, &subscriber).await;
			let consumer = subscriber.consume();
			kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
				.await
				.expect("resolve timeout")
				.expect("broadcast resolves")
				.unwrap()
		}
	};

	// A range with no start joins live, `how.min` behind the live edge, when one is given.
	let range = match (range.start, how.min) {
		(moq_net::track::Start::Oldest, Some(min)) => Range {
			start: moq_net::track::Start::Live {
				min: Some(min),
				max: Some(FOREVER),
			},
			..range
		},
		_ => range,
	};
	let subscription = Subscription::default()
		.with_order(Order::Ascending)
		.with_ranges([range]);
	let reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")?;

	let mut read = Groups {
		groups: Vec::new(),
		ended: false,
	};
	let mut ordered = None;
	let mut arrival = None;
	match how.ordered {
		true => ordered = Some(reader.ordered()),
		false => arrival = Some(reader),
	}
	loop {
		let next = match (&mut ordered, &mut arrival) {
			(Some(ordered), _) => kio::clock::timeout(TIMEOUT, ordered.next_group()).await,
			(_, Some(arrival)) => kio::clock::timeout(TIMEOUT, arrival.recv_group()).await,
			_ => unreachable!(),
		};
		let Ok(group) = next else { break };
		match group? {
			Some(group) => read.groups.push(group.start),
			None => {
				read.ended = true;
				break;
			}
		}
	}
	// A read that ended passed its range: a group the publisher creates there now is late.
	if read.ended && !how.append {
		let mut late = track.create_group(moq_net::group::Info { start: 1500 }).unwrap();
		late.write_frame(millis(1500), &b"x"[..]).unwrap();
		late.finish().unwrap();
		let next = match (&mut ordered, &mut arrival) {
			(Some(ordered), _) => kio::clock::timeout(TIMEOUT, ordered.next_group()).await,
			(_, Some(arrival)) => kio::clock::timeout(TIMEOUT, arrival.recv_group()).await,
			_ => unreachable!(),
		};
		// An error or a wait fails the test; a group handed out shows in the groups read.
		if let Some(group) = next.expect("an ended read answers at once")? {
			read.groups.push(group.start);
		}
	}
	let _track = track.finish().unwrap();
	Ok(read)
}

fn range(start: Option<u64>, end: Option<u64>) -> Range {
	let start = match start {
		Some(start) => moq_net::track::Start::Time(start),
		None => moq_net::track::Start::Oldest,
	};
	Range { start, end }
}

/// Time ranges, with what a reader of the held track reads.
fn timed() -> [(Range, Vec<u64>); 3] {
	[
		(range(Some(2500), None), vec![2000, 3000, 4000]),
		(range(Some(0), Some(2500)), vec![0, 1000, 2000]),
		(range(Some(1500), Some(3500)), vec![1000, 2000, 3000]),
	]
}

/// Each wire carries a time range and resolves it as the held track does, except that an older
/// wire starts it at the first group at or after its start instead.
#[kio::test]
async fn each_wire_carries_a_time_range() {
	let mut failures = Vec::new();
	for version in std::iter::once(None).chain(VERSIONS.iter().copied().map(Some)) {
		// Lite-07 resolves a time start to the group containing it, as the held track does.
		let resolves = matches!(version, None | Some("moq-lite-07-wip"));
		// A moq-transport copy joins live, so the groups below arrive as fills, after the live one.
		let transport = matches!(version, Some(version) if version.starts_with("moq-transport"));
		for (range, mut held) in timed() {
			// An older wire's publisher starts a time start at the first group at or after it,
			// not the group containing it.
			if let (false, moq_net::track::Start::Time(start)) = (resolves, range.start) {
				held.retain(|&key| key >= start);
			}
			let mut result = read(version, range).await;
			if transport && let Ok(read) = &mut result {
				read.sort_unstable();
			}
			match &result {
				Ok(read) if *read == held => {}
				_ => failures.push(format!("{version:?}: {range:?} read {result:?}")),
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Every wire carries a range with no start, as the live start with a stated Min Age and the
/// oldest with none, which it spells with no floor. A wire whose copy starts at the live edge
/// reads oldest from there: no floor asks for the older groups.
#[kio::test]
async fn every_wire_carries_the_live_and_oldest_starts() {
	for version in VERSIONS {
		let live = Read {
			min: Some(Duration::ZERO),
			..Read::default()
		};
		let live = read_as(Some(version), Range::ALL, live).await;
		assert_eq!(
			live.unwrap().groups.last(),
			Some(&4000),
			"{version}: live reads the latest group"
		);
		let mut oldest = read(Some(version), Range::ALL).await.unwrap();
		oldest.sort_unstable();
		let want: &[u64] = match *version {
			"moq-lite-03" | "moq-lite-04" | "moq-lite-05" | "moq-transport-18" | "moq-transport-22" => &[4000],
			_ => &[0, 1000, 2000, 3000, 4000],
		};
		assert_eq!(oldest, want, "{version}: oldest reads every group its start reaches");
	}
}

/// A live join stating a Min Age starts at the group containing the live edge minus it, where
/// the track is held and over lite-07. Older wires carry no Min Age, so the join starts at their
/// live edge.
#[kio::test]
async fn a_live_join_reaches_back_by_its_min_age_where_the_wire_carries_it() {
	let join = Read {
		min: Some(Duration::from_secs(2)),
		..Read::default()
	};
	for version in std::iter::once(None).chain(VERSIONS.iter().copied().map(Some)) {
		let read = read_as(version, Range::ALL, join).await.unwrap().groups;
		match version {
			None | Some("moq-lite-07-wip") => assert_eq!(read, [2000, 3000, 4000], "{version:?}"),
			_ => assert_eq!(read.last(), Some(&4000), "{version:?}: joins at the live edge"),
		}
	}
}

/// A publisher starts a live join at its newest group, which may hold no frame yet and so counts
/// neither in the largest it reports nor where the copy would place the join. The copy keeps the
/// publisher's start instead, so a reader waiting on each group in key starts there rather than
/// waiting below it for groups that never arrive.
#[kio::test]
async fn an_ordered_reader_starts_at_the_publishers_start_over_a_frameless_newest_group() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("room", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast
		.broadcast()
		.create_track("data", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce().unwrap();
	for key in 0..5 {
		let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
		group
			.write_frame(moq_net::Timestamp::unscaled(group.start), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
	}
	let mut newest = track.create_group(moq_net::group::Info { start: 5 }).unwrap();

	let subscriber = produce_origin(2);
	let _pair = peer("moq-lite-07-wip".parse::<Version>().unwrap(), &publisher, &subscriber).await;
	let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	let subscription = Subscription::default().with_ranges([Range::from(moq_net::track::Start::Live {
		min: Some(Duration::ZERO),
		max: Some(FOREVER),
	})]);
	let mut reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.unwrap()
		.ordered();

	newest
		.write_frame(moq_net::Timestamp::unscaled(newest.start), &b"x"[..])
		.unwrap();
	newest.finish().unwrap();
	// Waiting on each group in key, the reader starts where the publisher resolved the start,
	// counting its frameless newest group, with nothing below it owed or given up.
	let item = kio::clock::timeout(TIMEOUT, reader.next_item())
		.await
		.expect("group timeout")
		.unwrap();
	let Some(moq_net::track::Item::Group(group)) = item else {
		panic!("expected group 5 first")
	};
	assert_eq!(group.start, 5);
	let _track = track.finish().unwrap();
}

/// A lite-07 copy holds only part of the track, so a live join starts where its publisher placed
/// it rather than over the groups the copy holds: a sparse request mixing a time range and a
/// live join ending at a time reads every group it names.
#[kio::test]
async fn a_sparse_request_places_its_live_join_as_the_publisher_did() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("room", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast
		.broadcast()
		.create_track(
			"data",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce().unwrap();
	for key in 0..10 {
		let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
		group.write_frame(millis(key), &b"x"[..]).unwrap();
		group.finish().unwrap();
	}

	let subscriber = produce_origin(2);
	let _pair = peer("moq-lite-07-wip".parse::<Version>().unwrap(), &publisher, &subscriber).await;
	let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	// The live edge is at 9 ms, so a 2 ms lag joins at group 7.
	let join = Range {
		start: moq_net::track::Start::Live {
			min: Some(Duration::from_millis(2)),
			max: Some(FOREVER),
		},
		end: Some(8),
	};
	let subscription = Subscription::default()
		.with_order(Order::Ascending)
		.with_ranges([range(Some(1), Some(2)), join]);
	let mut reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.unwrap();

	let mut read = Vec::new();
	for _ in 0..4 {
		let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
			.await
			.expect("group timeout")
			.unwrap();
		read.push(group.map(|group| group.start));
	}
	assert_eq!(read, [Some(1), Some(2), Some(7), Some(8)]);
	let end = kio::clock::timeout(TIMEOUT, reader.recv_group())
		.await
		.expect("end timeout")
		.unwrap();
	assert_eq!(end.map(|group| group.start), None, "the ranges are covered");
	let _track = track.finish().unwrap();
}

/// A time end between two groups ends an ordered read once the publisher created a group past
/// it: handing that group out would have passed the starts between. Through the publisher's own
/// origin that is its own state. A lite-07 copy ends there only on what its wire proves, the
/// starts between skipped, which an appending publisher's SUBSCRIBE_SKIP and GROUP_END say. A
/// publisher creating groups out of order says nothing, so the copy, and any arrival read of a
/// track created that way, waits for a group that may still come. A group created below the
/// end after an ordered read ended is late, as for any key the read passed.
#[kio::test]
async fn a_time_end_between_groups_ends_an_ordered_read_at_the_next() {
	let between = range(Some(1500), Some(1999));
	let ordered = |append| Read {
		append,
		ordered: true,
		min: None,
	};
	let arrival = |append| Read {
		append,
		ordered: false,
		min: None,
	};
	let lite07 = Some("moq-lite-07-wip");
	for (version, how, ended) in [
		(None, ordered(false), true),
		(None, ordered(true), true),
		(None, arrival(false), false),
		(None, arrival(true), true),
		(lite07, ordered(true), true),
		(lite07, arrival(true), true),
		(lite07, ordered(false), false),
	] {
		let read = read_as(version, between, how).await.unwrap();
		let label = (version, how.append, how.ordered);
		assert_eq!(
			read,
			Groups {
				groups: vec![1000],
				ended
			},
			"{label:?}"
		);
	}
}

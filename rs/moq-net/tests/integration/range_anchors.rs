#![cfg(feature = "async")]

//! A range anchored relative to the latest group or at a presentation time resolves where
//! the track is held, and on a copy received over a wire that tells it enough to resolve
//! the anchor the same way. Every wire carries a time anchor, as the group ID with that key,
//! but only lite-07 resolves a time start to the group containing it: an older wire's
//! publisher starts at the first group at or after it. A moq-transport copy counts a
//! relative anchor back by group ID from the Largest Object its live join reports. A copy
//! received over any
//! other wire refuses a relative anchor other than the live start instead of resolving it
//! against the part of the track it happens to hold.

use crate::support;
use moq_net::track::Delay;

use std::time::Duration;

use moq_net::track::{Anchor, Info, Order, Range, Subscription};
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
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
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

	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(FOREVER))
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

fn range(start: Anchor, end: Option<Anchor>) -> Range {
	Range { start, end }
}

/// Relative and time anchors at each end, with what a reader of the held track reads.
fn anchored() -> [(Range, Vec<u64>); 6] {
	let zero = Anchor::Time(0);
	[
		(range(Anchor::Relative(2), None), vec![2000, 3000, 4000]),
		(range(Anchor::Time(2500), None), vec![2000, 3000, 4000]),
		(range(zero, Some(Anchor::Relative(1))), vec![0, 1000, 2000, 3000]),
		(range(zero, Some(Anchor::Time(2500))), vec![0, 1000, 2000]),
		(range(Anchor::Relative(2), Some(Anchor::Relative(1))), vec![2000, 3000]),
		(
			range(Anchor::Time(1500), Some(Anchor::Time(3500))),
			vec![1000, 2000, 3000],
		),
	]
}

/// Each wire resolves the anchors it carries as the held track does and refuses the rest,
/// rather than resolve them against the part of the track its copy happens to hold. An older
/// wire starts a time anchor at the first group at or after it instead.
#[kio::test]
async fn each_wire_refuses_only_the_anchors_it_drops() {
	let mut failures = Vec::new();
	for version in std::iter::once(None).chain(VERSIONS.iter().copied().map(Some)) {
		// Lite-07 reports the largest and asks for everything the budget allows, so its copy
		// resolves every anchor where it is held.
		let resolves = matches!(version, None | Some("moq-lite-07-wip"));
		// A moq-transport copy joins live and resolves a relative anchor from the Largest
		// Object, so the groups below it arrive as fills, after the live one.
		let transport = matches!(version, Some(version) if version.starts_with("moq-transport"));
		for (range, mut held) in anchored() {
			// An older wire's publisher starts a time anchor at the first group at or after it,
			// not the group containing it.
			if let (false, Anchor::Time(start)) = (resolves, range.start) {
				held.retain(|&key| key >= start);
			}
			// A moq-transport copy counts a relative anchor back by group ID, as a joining FETCH
			// does, never over the groups it holds: these groups' IDs are their starts, a second
			// apart, so two back from 4000 is 3998.
			if transport {
				if let Anchor::Relative(back) = range.start {
					held.retain(|&key| key >= 4000 - back);
				}
				if let Some(Anchor::Relative(back)) = range.end {
					held.retain(|&key| key <= 4000 - back);
				}
			}
			let mut result = read(version, range).await;
			// Any other wire carries time anchors and the live start, and drops the rest.
			let wire = matches!(range.start, Anchor::Time(_) | Anchor::Oldest | Anchor::Relative(0))
				&& matches!(range.end, None | Some(Anchor::Time(_)));
			let carried = resolves || transport || wire;
			if transport && let Ok(read) = &mut result {
				read.sort_unstable();
			}
			match (&result, carried) {
				(Err(Error::Unsupported), false) => {}
				(Ok(read), true) if *read == held => {}
				_ => failures.push(format!("{version:?}: {range:?} read {result:?}")),
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Every wire carries the live and oldest starts, which it spells with no floor. A wire whose
/// copy starts at the live edge reads oldest from there: no floor asks for the older groups.
#[kio::test]
async fn every_wire_carries_the_live_and_oldest_starts() {
	for version in VERSIONS {
		let live = read(Some(version), Range::live()).await;
		assert_eq!(
			live.unwrap().last(),
			Some(&4000),
			"{version}: live reads the latest group"
		);
		let mut oldest = read(Some(version), Range::oldest()).await.unwrap();
		oldest.sort_unstable();
		let want: &[u64] = match *version {
			"moq-lite-03" | "moq-lite-04" | "moq-lite-05" | "moq-transport-18" | "moq-transport-22" => &[4000],
			_ => &[0, 1000, 2000, 3000, 4000],
		};
		assert_eq!(oldest, want, "{version}: oldest reads every group its start reaches");
	}
}

/// A publisher counts a relative start back from its newest group, which may hold no frame yet
/// and so counts neither in the largest it reports nor where the copy resolves the start. The
/// copy keeps the publisher's start instead, so a reader waiting on each group in key starts
/// there rather than waiting below it for groups that never arrive.
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
	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(FOREVER))
		.with_ranges([Range::live()]);
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

/// A lite-07 copy holds only part of the track, so a relative start counts back as its
/// publisher did rather than over the groups the copy holds: a sparse request mixing a time
/// range and a relative one reads every group it names. Mirrors the `@moq/net`
/// `integration: lite-07 serves sparse and relative ranges`.
#[kio::test]
async fn a_sparse_request_counts_its_relative_start_back_as_the_publisher_did() {
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
	let subscription = Subscription::default()
		.with_delay(Delay::default().with_max(FOREVER))
		.with_order(Order::Ascending)
		.with_ranges([
			range(Anchor::Time(1), Some(Anchor::Time(2))),
			range(Anchor::Relative(2), Some(Anchor::Time(8))),
		]);
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

/// A later reader's relative start on a lite-07 copy already holding part of the track counts
/// back as the publisher did, not over the groups the copy happens to hold: over a copy
/// holding groups 1 and 2 of 0 to 9, it reads only the groups its range names. Mirrors the
/// `@moq/net` `integration: lite-07 starts a later reader's relative range where the
/// publisher did`.
#[kio::test]
async fn a_later_relative_start_on_a_partial_copy_reads_only_its_groups() {
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
	let subscribe = |ranges: Vec<Range>| {
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(FOREVER))
			.with_order(Order::Ascending)
			.with_ranges(ranges);
		remote.track("data").unwrap().subscribe(subscription)
	};
	let read = async |reader: &mut moq_net::track::Subscriber, count: usize| {
		let mut read = Vec::new();
		for _ in 0..count {
			let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
				.await
				.expect("group timeout")
				.unwrap();
			read.push(group.map(|group| group.start));
		}
		read
	};

	let mut first = kio::clock::timeout(TIMEOUT, subscribe(vec![range(Anchor::Time(1), Some(Anchor::Time(2)))]))
		.await
		.expect("subscribe timeout")
		.unwrap();
	assert_eq!(read(&mut first, 3).await, [Some(1), Some(2), None]);

	// The publisher counts back past the end, so it has no group there to deliver.
	let mut empty = kio::clock::timeout(
		TIMEOUT,
		subscribe(vec![range(Anchor::Relative(2), Some(Anchor::Time(6)))]),
	)
	.await
	.expect("subscribe timeout")
	.unwrap();
	assert_eq!(read(&mut empty, 1).await, [None], "nothing to deliver");
	drop(empty);

	let mut second = kio::clock::timeout(
		TIMEOUT,
		subscribe(vec![range(Anchor::Relative(2), Some(Anchor::Time(8)))]),
	)
	.await
	.expect("subscribe timeout")
	.unwrap();
	assert_eq!(
		read(&mut second, 3).await,
		[Some(7), Some(8), None],
		"only the groups the publisher counts back to"
	);
	drop(first);
	let _track = track.finish().unwrap();
}

/// A later reader's relative start on a copy reading a track from its oldest group, which
/// already delivers every group the reader asks for, still asks the publisher where it starts,
/// and starts there: counting back past group 1, at group 1. Mirrors the `@moq/net`
/// `integration: lite-07 counts a later relative start back to where the oldest range
/// started`.
#[kio::test]
async fn a_later_relative_start_counts_back_to_where_the_oldest_range_started() {
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
	for key in 1..4 {
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
	let subscribe = |ranges: Vec<Range>| {
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(FOREVER))
			.with_order(Order::Ascending)
			.with_ranges(ranges);
		remote.track("data").unwrap().subscribe(subscription)
	};
	let read = async |reader: &mut moq_net::track::Subscriber, count: usize| {
		let mut read = Vec::new();
		for _ in 0..count {
			let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
				.await
				.expect("group timeout")
				.unwrap();
			read.push(group.map(|group| group.start));
		}
		read
	};

	let mut oldest = kio::clock::timeout(TIMEOUT, subscribe(vec![Range::oldest()]))
		.await
		.expect("subscribe timeout")
		.unwrap();
	assert_eq!(read(&mut oldest, 3).await, [Some(1), Some(2), Some(3)]);

	let mut later = kio::clock::timeout(TIMEOUT, subscribe(vec![range(Anchor::Relative(5), None)]))
		.await
		.expect("subscribe timeout")
		.unwrap();
	assert_eq!(
		read(&mut later, 3).await,
		[Some(1), Some(2), Some(3)],
		"counted back past group 1, as the publisher does"
	);
	drop(oldest);
	let _track = track.finish().unwrap();
}

/// A relative start on a lite-07 copy never resolves against the groups the copy holds: over a
/// copy holding groups 9 and 10, a reader counting 5 back starts where the publisher counts
/// to, not at 9. While the subscription that delivered them is live, the copy asks it for the
/// reader's range, and the publisher counts back from its latest group, 10, to 5. Once it is
/// gone, the copy subscribes again and the reader waits for that answer, counted back from the
/// Largest it reports: 11, written while the copy was idle, so 6. Mirrors the `@moq/net`
/// `integration: lite-07 never resolves a relative start against the groups a copy holds`.
#[kio::test]
async fn a_relative_start_never_resolves_against_the_groups_a_copy_holds() {
	for live in [true, false] {
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
		let write = |key: u64| {
			let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
			group.write_frame(millis(key), &b"x"[..]).unwrap();
			group.finish().unwrap();
		};
		for key in 0..=10 {
			write(key);
		}

		let subscriber = produce_origin(2);
		let _pair = peer("moq-lite-07-wip".parse::<Version>().unwrap(), &publisher, &subscriber).await;
		let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		let subscribe = |ranges: Vec<Range>| {
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(FOREVER))
				.with_order(Order::Ascending)
				.with_ranges(ranges);
			remote.track("data").unwrap().subscribe(subscription)
		};
		let read = async |reader: &mut moq_net::track::Subscriber| {
			let mut read = Vec::new();
			loop {
				let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
					.await
					.expect("group timeout")
					.unwrap();
				match group {
					Some(group) => read.push(group.start),
					None => break,
				}
			}
			read.sort_unstable();
			read
		};

		// The copy holds groups 9 and 10.
		let mut first = kio::clock::timeout(TIMEOUT, subscribe(vec![range(Anchor::Time(9), Some(Anchor::Time(10)))]))
			.await
			.expect("subscribe timeout")
			.unwrap();
		assert_eq!(read(&mut first).await, [9, 10]);
		let (expected, end) = match live {
			true => (vec![5, 6, 7, 8, 9, 10], 10),
			false => {
				// Its upstream subscription leaves with its last reader, and the track moves on.
				drop(first);
				kio::clock::sleep(Duration::from_secs(1)).await;
				write(11);
				(vec![6, 7, 8, 9, 10, 11], 11)
			}
		};
		let mut later = kio::clock::timeout(
			TIMEOUT,
			subscribe(vec![range(Anchor::Relative(5), Some(Anchor::Time(end)))]),
		)
		.await
		.expect("subscribe timeout")
		.unwrap();
		assert_eq!(read(&mut later).await, expected, "live: {live}");
		let _track = track.finish().unwrap();
	}
}

/// A finished copy has no upstream subscription left to carry a later reader's relative start,
/// so the copy asks again with a subscription of its own, and the reader still reads what the
/// publisher counts back to rather than nothing: the same groups a cold copy reads. Mirrors the
/// `@moq/net` `integration: lite-07 starts a relative start on a finished copy where the
/// publisher does`.
#[kio::test]
async fn a_relative_start_on_a_finished_copy_reads_the_publishers_groups() {
	// Whether the copy holds every group the reader counts back to or only some.
	for from in [0, 9] {
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
		for key in 0..=10 {
			let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
			group.write_frame(millis(key), &b"x"[..]).unwrap();
			group.finish().unwrap();
		}
		let _track = track.finish().unwrap();

		let subscriber = produce_origin(2);
		let _pair = peer("moq-lite-07-wip".parse::<Version>().unwrap(), &publisher, &subscriber).await;
		let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		let subscribe = |ranges: Vec<Range>| {
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(FOREVER))
				.with_order(Order::Ascending)
				.with_ranges(ranges);
			remote.track("data").unwrap().subscribe(subscription)
		};
		let read = async |reader: &mut moq_net::track::Subscriber| {
			let mut read = Vec::new();
			while let Some(group) = kio::clock::timeout(TIMEOUT, reader.recv_group())
				.await
				.expect("group timeout")
				.unwrap()
			{
				read.push(group.start);
			}
			read.sort_unstable();
			read
		};

		// The first reader holds the finished copy.
		let mut first = kio::clock::timeout(TIMEOUT, subscribe(vec![range(Anchor::Time(from), None)]))
			.await
			.expect("subscribe timeout")
			.unwrap();
		assert_eq!(read(&mut first).await, (from..=10).collect::<Vec<_>>());
		// Its subscription is over by now.
		kio::clock::sleep(Duration::from_secs(1)).await;
		let mut later = kio::clock::timeout(TIMEOUT, subscribe(vec![range(Anchor::Relative(5), None)]))
			.await
			.expect("subscribe timeout")
			.unwrap();
		assert_eq!(read(&mut later).await, [5, 6, 7, 8, 9, 10], "holding from {from}");
		drop(first);
	}
}

/// A time end between two groups ends an ordered read once the publisher created a group past
/// it: handing that group out would have passed the starts between. Through the publisher's own
/// origin that is its own state. A lite-07 copy ends there only on what its wire proves, the
/// starts between skipped, which a GROUP's Previous Start says of an appending publisher. A
/// publisher creating groups out of order says nothing, so the copy, and any arrival read of a
/// track created that way, waits for a group that may still come. A group created below the
/// end after an ordered read ended is late, as for any key the read passed.
#[kio::test]
async fn a_time_end_between_groups_ends_an_ordered_read_at_the_next() {
	let between = range(Anchor::Time(1500), Some(Anchor::Time(1999)));
	let ordered = |append| Read { append, ordered: true };
	let arrival = |append| Read { append, ordered: false };
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

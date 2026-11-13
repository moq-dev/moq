#![cfg(feature = "async")]

//! A range anchored relative to the latest group or at a presentation time resolves where
//! the track is held, and on a copy received over a wire that tells it enough to resolve
//! the anchor the same way. Every wire carries a time anchor, as the group ID with that key,
//! but only lite-07 resolves a time start to the group containing it: an older wire's
//! publisher starts at the first group at or after it. A moq-transport copy resolves a
//! relative anchor from the Largest Object its live join reports. A copy received over any
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
		let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
		group.write_frame(millis(key), &b"x"[..]).unwrap();
		group.close().unwrap();
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
	let mut reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")?;

	let mut read = Vec::new();
	while let Ok(group) = kio::clock::timeout(TIMEOUT, reader.recv_group()).await {
		match group? {
			Some(group) => read.push(group.start),
			None => break,
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
/// copy reports the publisher's start instead, where a reader waiting on each group in key
/// starts, rather than one below that never arrives.
#[kio::test]
async fn a_relative_start_reports_the_publishers_start_over_a_frameless_newest_group() {
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
		group.write_frame(None, &b"x"[..]).unwrap();
		group.close().unwrap();
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
		.unwrap();

	let start = kio::clock::timeout(TIMEOUT, reader.start())
		.await
		.expect("start timeout");
	assert_eq!(
		start,
		Some(5),
		"the publisher's resolved start, counting its frameless newest group"
	);

	newest.write_frame(None, &b"x"[..]).unwrap();
	newest.close().unwrap();
	let group = kio::clock::timeout(TIMEOUT, reader.recv_group())
		.await
		.expect("group timeout")
		.unwrap();
	assert_eq!(group.map(|group| group.start), Some(5));
	let _track = track.finish().unwrap();
}

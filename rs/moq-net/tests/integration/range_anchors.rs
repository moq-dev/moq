#![cfg(feature = "async")]

//! A range anchored relative to the latest group or at a presentation time resolves where
//! the track is held, and on a copy received over a wire that tells it enough to resolve
//! the anchor the same way. A moq-transport copy resolves a relative anchor from the
//! Largest Object its live join reports. A copy received over any other wire refuses the
//! anchor instead of resolving it against the part of the track it happens to hold.

use crate::support;

use std::time::Duration;

use moq_net::track::{Anchor, Info, Order, Range, Subscription};
use moq_net::{Error, Hop, Timescale, Timestamp, Version};
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
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn millis(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

/// Groups 0 to 4, group `n` presenting at `n` seconds, then what a subscriber to `range`
/// reads oldest first: through the publisher's own origin with no `version`, otherwise
/// over one session on it.
async fn read(version: Option<&str>, range: Range) -> Result<Vec<u64>, Error> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("room").unwrap();
	let track = broadcast
		.create_track(
			"data",
			Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER),
		)
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..5 {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		group.write_frame(millis(sequence * 1000), &b"x"[..]).unwrap();
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
		}
	};

	let subscription = Subscription::default()
		.with_max_delay(FOREVER)
		.with_order(Order::Ascending)
		.with_ranges([range]);
	let mut reader = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")?;

	let mut read = Vec::new();
	while let Ok(group) = kio::clock::timeout(TIMEOUT, reader.recv_group()).await {
		match group? {
			Some(group) => read.push(group.sequence),
			None => break,
		}
	}
	track.finish().unwrap();
	Ok(read)
}

fn range(start: Anchor, end: Option<Anchor>) -> Range {
	Range { start, end }
}

/// Relative and time anchors at each end, with what a reader of the held track reads.
fn anchored() -> [(Range, Vec<u64>); 6] {
	let zero = Anchor::Absolute(0);
	[
		(range(Anchor::Relative(2), None), vec![2, 3, 4]),
		(range(Anchor::Time(millis(2500)), None), vec![2, 3, 4]),
		(range(zero, Some(Anchor::Relative(1))), vec![0, 1, 2, 3]),
		(range(zero, Some(Anchor::Time(millis(2500)))), vec![0, 1, 2]),
		(range(Anchor::Relative(2), Some(Anchor::Relative(1))), vec![2, 3]),
		(
			range(Anchor::Time(millis(1500)), Some(Anchor::Time(millis(3500)))),
			vec![1, 2, 3],
		),
	]
}

/// Each wire resolves the anchors it carries as the held track does and refuses the rest,
/// rather than resolve them against the part of the track its copy happens to hold.
#[kio::test]
async fn each_wire_refuses_only_the_anchors_it_drops() {
	for version in std::iter::once(None).chain(VERSIONS.iter().copied().map(Some)) {
		// Lite-07 reports the largest and asks for everything the budget allows, so its copy
		// resolves every anchor where it is held.
		let carried = matches!(version, None | Some("moq-lite-07-wip"));
		// A wire without timestamps refuses a time anchor for that first.
		let timed = !matches!(version, Some("moq-lite-03" | "moq-lite-04"));
		for (range, held) in anchored() {
			let mut result = read(version, range).await;
			let time = matches!(range.start, Anchor::Time(_)) || matches!(range.end, Some(Anchor::Time(_)));
			// A moq-transport copy joins live and resolves a relative anchor from the Largest
			// Object, so the groups below it arrive as fills, after the live one.
			let transport = matches!(version, Some(version) if version.starts_with("moq-transport"));
			let carried = carried || (transport && !time);
			if transport && let Ok(read) = &mut result {
				read.sort_unstable();
			}
			match (&result, time && !timed, carried) {
				(Err(Error::TimestampMismatch), true, _) => {}
				(Err(Error::Unsupported), false, false) => {}
				(Ok(read), false, true) if *read == held => {}
				_ => panic!("{version:?}: {range:?} read {result:?}"),
			}
		}
	}
}

/// Every wire carries the live and oldest starts, which it spells with no floor. A wire whose
/// copy starts at the live edge reads oldest from there: no floor asks for the older groups.
#[kio::test]
async fn every_wire_carries_the_live_and_oldest_starts() {
	for version in VERSIONS {
		let live = read(Some(version), Range::live()).await;
		assert_eq!(live.unwrap().last(), Some(&4), "{version}: live reads the latest group");
		let mut oldest = read(Some(version), Range::oldest()).await.unwrap();
		oldest.sort_unstable();
		let want: &[u64] = match *version {
			"moq-lite-05" | "moq-transport-18" | "moq-transport-22" => &[4],
			_ => &[0, 1, 2, 3, 4],
		};
		assert_eq!(oldest, want, "{version}: oldest reads every group its start reaches");
	}
}

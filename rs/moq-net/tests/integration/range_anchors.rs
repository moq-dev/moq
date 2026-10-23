#![cfg(feature = "async")]

//! A range starting relative to the latest group or at a presentation time resolves only
//! where the track is held. No wire carries such an anchor upstream yet, so a copy
//! received over a session refuses one instead of joining at the live edge.

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
	"moq-transport-14",
	"moq-transport-16",
	"moq-transport-17",
	"moq-transport-20",
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
		None => moq_clock::timeout(TIMEOUT, publisher.consume().request_broadcast("room", None))
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves"),
		Some(version) => {
			_pair = peer(version.parse::<Version>().unwrap(), &publisher, &subscriber).await;
			let consumer = subscriber.consume();
			moq_clock::timeout(TIMEOUT, consumer.routed("room"))
				.await
				.expect("announce timeout")
				.expect("routed");
			moq_clock::timeout(TIMEOUT, consumer.request_broadcast("room", None))
				.await
				.expect("resolve timeout")
				.expect("broadcast resolves")
		}
	};

	let subscription = Subscription::default()
		.with_max_delay(FOREVER)
		.with_order(Order::Ascending)
		.with_ranges([range]);
	let mut reader = moq_clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")?;

	let mut read = Vec::new();
	while let Ok(group) = moq_clock::timeout(TIMEOUT, reader.recv_group()).await {
		match group? {
			Some(group) => read.push(group.sequence),
			None => break,
		}
	}
	track.finish().unwrap();
	Ok(read)
}

fn from(start: Anchor) -> Range {
	Range { start, end: None }
}

/// Where the track is held, both anchors resolve against its groups.
#[moq_clock::test]
async fn a_held_track_resolves_relative_and_time_starts() {
	assert_eq!(read(None, from(Anchor::Relative(2))).await.unwrap(), vec![2, 3, 4]);
	assert_eq!(
		read(None, from(Anchor::Time(millis(2500)))).await.unwrap(),
		vec![2, 3, 4]
	);
}

/// Over a session, the anchors the wire cannot carry are refused, while the live and
/// oldest starts it does carry still read.
#[moq_clock::test]
async fn a_received_copy_refuses_what_its_wire_drops() {
	let refused = [
		from(Anchor::Relative(2)),
		from(Anchor::Time(millis(2500))),
		Range {
			start: Anchor::Absolute(moq_net::track::Position::group(0)),
			end: Some(Anchor::Relative(1)),
		},
		Range {
			start: Anchor::Absolute(moq_net::track::Position::group(0)),
			end: Some(Anchor::Time(millis(2500))),
		},
	];
	for version in VERSIONS {
		for range in refused {
			let result = read(Some(version), range).await;
			// A wire without timestamps refuses a time anchor for that first.
			let timed = !matches!(
				*version,
				"moq-lite-03" | "moq-lite-04" | "moq-lite-05" | "moq-lite-06" | "moq-transport-14" | "moq-transport-16"
			);
			let time = matches!(range.start, Anchor::Time(_)) || matches!(range.end, Some(Anchor::Time(_)));
			match (time && !timed, &result) {
				(true, Err(Error::TimestampMismatch)) | (false, Err(Error::Unsupported)) => {}
				_ => panic!("{version}: {range:?} read {result:?}"),
			}
		}
		let live = read(Some(version), Range::live()).await;
		assert_eq!(live.unwrap().last(), Some(&4), "{version}: live reads the latest group");
		let oldest = read(Some(version), Range::oldest()).await;
		assert_eq!(
			oldest.unwrap().last(),
			Some(&4),
			"{version}: oldest reads through the latest group"
		);
	}
}

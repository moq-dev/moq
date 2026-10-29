#![cfg(feature = "async")]

//! A track's tail: a group stream that reaches the subscriber after the publisher has
//! ended the subscription still belongs to the track.
//!
//! Publishers end a subscription only once every group stream they opened is finished,
//! but QUIC does not order streams, so the subscriber can read the end (moq-lite's
//! subscribe stream FIN, IETF's PUBLISH_DONE) before a group's header. The mock holds the
//! publisher's group streams back from the subscriber to make that ordering
//! deterministic, while acknowledging them to the publisher like a real transport would.
//!
//! Time is paused, so the one-second grace for a group that never arrives is free.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);
const PAYLOAD: &[u8] = b"frame";

/// How long a subscriber waits for a group it cannot account for, with no max delay set.
const GRACE: Duration = Duration::from_secs(1);

/// moq-lite drafts with and without SUBSCRIBE_END, and IETF drafts with a joining
/// FETCH (18) and with subscription fills (22).
const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-07-wip",
	"moq-transport-18",
	"moq-transport-22",
];

/// What becomes of the group streams held back past the subscription's end.
#[derive(Clone, Copy, Debug)]
enum Late {
	/// They arrive after the end.
	Delivered,
	/// They never arrive, like a stream reset before its header.
	Lost,
}

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

struct Outcome {
	frames: Vec<Vec<u8>>,
	err: Option<moq_net::Error>,
	/// How long after the held streams were released (or lost) the track ended.
	elapsed: Duration,
}

/// A publisher and subscriber over the mock, with the subscriber holding `bcast`.
struct Pair {
	pair: support::harness::MockPair,
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	_keep: (
		moq_net::origin::Publication,
		moq_net::origin::Producer,
		moq_net::origin::Producer,
	),
}

async fn connect(version: &str) -> Pair {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	Pair {
		pair,
		track,
		remote,
		_keep: (broadcast, publisher, subscriber),
	}
}

/// Publish a group below the declared end (or none for an empty track), hold its stream
/// back until the subscription ends, then deliver or lose it. The subscriber reads with
/// `max_delay`.
async fn round(version: &str, late: Late, final_sequence: u64, max_delay: Option<Duration>) -> Outcome {
	let Pair {
		pair,
		mut track,
		remote,
		_keep,
	} = connect(version).await;

	let reader = moq_clock::sim::spawn(async move {
		let subscription = moq_net::track::Subscription::default()
			.with_range(0..)
			.with_max_delay(max_delay);
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let mut frames = Vec::new();
		let err = loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => break None,
				Err(err) => break Some(err),
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => frames.push(frame.payload.to_vec()),
					Ok(None) => break,
					Err(err) => panic!("group failed: {err}"),
				}
			}
		};
		(frames, err, moq_clock::now())
	});

	moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
		.await
		.expect("no subscriber appeared");
	// Let the publisher answer while the track is still empty. Before draft 22, a group
	// written first sits below SUBSCRIBE_OK's Largest Object, so it would arrive as a
	// joining FETCH, which the track ending first refuses, not as the subscription's own
	// late stream. Paused time advances only once every task is idle.
	moq_clock::sleep(Duration::from_millis(1)).await;

	pair.server_transport.hold_unis();
	if final_sequence > 0 {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
		group.finish().unwrap();
	}
	track
		.finish_at(moq_net::track::Position::group(final_sequence))
		.unwrap();
	drop(track);

	if final_sequence > 0 {
		// Paused time advances only when every task is idle, after the publisher and
		// subscriber have processed the subscription's end.
		moq_clock::sleep(GRACE / 10).await;
		assert!(
			!reader.is_finished(),
			"{version}: the track ended before its group arrived"
		);
	}

	let released = moq_clock::now();
	match late {
		Late::Delivered => pair.server_transport.release_unis(),
		Late::Lost => pair.server_transport.drop_unis(),
	}

	let (frames, err, ended) = moq_clock::timeout(TIMEOUT, reader)
		.await
		.expect("the subscription never ended")
		.expect("reader panicked");
	drop((pair, _keep));
	Outcome {
		frames,
		err,
		elapsed: ended - released,
	}
}

/// A group whose header arrives after the subscription's end is delivered, then the track
/// ends cleanly. An unbounded max delay waits for it too: its grace never runs out.
#[moq_clock::test]
async fn a_group_after_the_end_is_delivered() {
	for version in VERSIONS {
		// moq-lite-03 declares no end, so an unbounded subscription there waits forever.
		let delays: &[Option<Duration>] = match *version {
			"moq-lite-03" => &[Some(Duration::ZERO)],
			_ => &[Some(Duration::ZERO), None],
		};
		for &max_delay in delays {
			let outcome = round(version, Late::Delivered, 1, max_delay).await;
			assert!(
				outcome.err.is_none() && outcome.frames == [PAYLOAD],
				"{version} max_delay={max_delay:?}: got {} frame(s), err={:?}",
				outcome.frames.len(),
				outcome.err,
			);

			// Drafts that say where the track ends, or how many streams the publisher
			// opened, end as soon as the last group arrives rather than waiting out the grace.
			if *version != "moq-lite-03" {
				assert!(
					outcome.elapsed < GRACE / 10,
					"{version} max_delay={max_delay:?}: ended after {:?}",
					outcome.elapsed
				);
			}
		}
	}
}

/// A group that never arrives is given up on after the grace, and the track still ends
/// cleanly without it.
#[moq_clock::test]
async fn a_lost_group_ends_the_track_after_the_grace() {
	for version in VERSIONS {
		let outcome = round(version, Late::Lost, 1, Some(Duration::ZERO)).await;
		assert!(
			outcome.err.is_none() && outcome.frames.is_empty(),
			"{version}: got {} frame(s), err={:?}",
			outcome.frames.len(),
			outcome.err,
		);
		assert!(
			outcome.elapsed >= GRACE / 2,
			"{version}: ended after {:?}",
			outcome.elapsed
		);
	}
}

/// Skipped sequences have no stream, so lite-05+ publishers name them with SUBSCRIBE_DROP.
#[moq_clock::test]
async fn skipped_groups_end_without_the_grace() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let outcome = round(version, Late::Delivered, 3, Some(Duration::ZERO)).await;
		assert!(outcome.err.is_none(), "{version}: {:?}", outcome.err);
		assert_eq!(outcome.frames, [PAYLOAD], "{version}");
		assert!(
			outcome.elapsed < GRACE / 10,
			"{version}: ended after {:?}",
			outcome.elapsed
		);
	}
}

/// An empty track leaves no tail to wait for.
#[moq_clock::test]
async fn lite07_empty_track_ends_without_the_grace() {
	let outcome = round("moq-lite-07-wip", Late::Delivered, 0, Some(Duration::ZERO)).await;
	assert!(outcome.err.is_none());
	assert!(outcome.frames.is_empty());
	assert!(outcome.elapsed < GRACE / 10, "ended after {:?}", outcome.elapsed);
}

/// IETF drafts with a joining FETCH (18) and with subscription fills (22).
const IETF: &[&str] = &["moq-transport-18", "moq-transport-22"];

/// A subscriber that leaves while END_OF_TRACK waits for stream credit ends the publisher's
/// request, instead of parking it until credit that may never come.
#[moq_clock::test]
async fn ietf_leaving_cancels_a_blocked_end_of_track() {
	for version in IETF {
		let Pair {
			pair,
			track,
			remote,
			_keep,
		} = connect(version).await;

		let subscription = moq_net::track::Subscription::default().with_range(0..);
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
			.await
			.expect("no subscriber appeared");

		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
		group.finish().unwrap();
		let mut group = moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		while group.read_frame().await.unwrap().is_some() {}

		// Out of stream credit, so the marker cannot open.
		pair.server_transport.withhold_unis();
		track.finish().unwrap();
		moq_clock::sleep(GRACE / 10).await;
		assert!(
			track.subscription().is_some(),
			"{version}: the publisher is still ending the request"
		);

		// The request task holds the publisher's subscription until it ends.
		drop((group, sub));
		moq_clock::timeout(TIMEOUT, async {
			while track.subscription().is_some() {
				moq_clock::sleep(GRACE / 100).await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: the publisher's request never ended"));
		drop((pair, _keep));
	}
}

/// A lost datagram is indistinguishable from a reordered stream header, so its hole
/// waits out the tail grace on every lite version.
#[moq_clock::test]
async fn a_lost_datagram_delays_the_end() {
	for version in ["moq-lite-05", "moq-lite-07-wip"] {
		let Pair {
			pair,
			mut track,
			remote,
			_keep,
		} = connect(version).await;

		let reader = moq_clock::sim::spawn(async move {
			let subscription = moq_net::track::Subscription::default().with_range(0..);
			let mut sub = remote
				.track("video")
				.unwrap()
				.subscribe(subscription)
				.await
				.expect("subscribe");
			let mut groups = Vec::new();
			while let Some(group) = sub.recv_group().await.expect("track aborted") {
				groups.push(group.sequence);
			}
			(groups, moq_clock::now())
		});
		moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
			.await
			.expect("no subscriber appeared");

		for datagram in [false, true, false] {
			if datagram {
				pair.server_transport.lose_datagrams();
				track.append_datagram(Timestamp::ZERO, PAYLOAD).unwrap();
			} else {
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
				group.finish().unwrap();
			}
			moq_clock::sleep(GRACE / 100).await;
		}
		let finished = moq_clock::now();
		track.finish().unwrap();

		let (groups, ended) = moq_clock::timeout(TIMEOUT, reader)
			.await
			.expect("the subscription never ended")
			.expect("reader panicked");
		assert_eq!(groups, [0, 2], "{version}");
		let elapsed = ended - finished;
		assert!(
			(GRACE / 2..GRACE * 2).contains(&elapsed),
			"{version}: ended after {elapsed:?}"
		);
		drop((pair, _keep));
	}
}

/// A publisher can declare the end, then open each group's stream before writing its first
/// frame. The subscriber withholds a group until that frame lands, and the end waits for
/// it: a reader that ended at the boundary would miss every group.
#[moq_clock::test]
async fn a_group_whose_first_frame_trails_its_header_is_delivered() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let Pair {
			pair,
			mut track,
			remote,
			_keep,
		} = connect(version).await;

		let reader = moq_clock::sim::spawn(async move {
			let subscription = moq_net::track::Subscription::default()
				.with_range(0..)
				.with_max_delay(TIMEOUT);
			let mut sub = remote
				.track("video")
				.unwrap()
				.subscribe(subscription)
				.await
				.expect("subscribe");
			let mut groups = Vec::new();
			while let Some(mut group) = sub.recv_group().await.expect("track aborted") {
				let frame = group.read_frame().await.expect("group aborted").expect("a frame");
				assert_eq!(&frame.payload[..], PAYLOAD);
				groups.push(group.sequence);
			}
			groups
		});
		moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
			.await
			.expect("no subscriber appeared");

		track.finish_at(moq_net::track::Position::group(2)).unwrap();
		let mut groups = [track.append_group().unwrap(), track.append_group().unwrap()];
		// Paused time advances only once every task is idle: both headers and the end
		// have reached the subscriber, ahead of any frame.
		moq_clock::sleep(GRACE / 10).await;
		assert!(
			!reader.is_finished(),
			"{version}: the track ended before its groups showed"
		);

		for group in &mut groups {
			group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
			group.finish().unwrap();
		}
		let mut groups = moq_clock::timeout(TIMEOUT, reader)
			.await
			.expect("the subscription never ended")
			.expect("reader panicked");
		// Arrival order: either group's frame may land first.
		groups.sort();
		assert_eq!(groups, [0, 1], "{version}");
		drop((track, pair, _keep));
	}
}

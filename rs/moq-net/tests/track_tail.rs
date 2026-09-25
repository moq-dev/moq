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

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const PAYLOAD: &[u8] = b"frame";

/// How long a subscriber waits for a group it cannot account for, with no max age set.
const GRACE: Duration = Duration::from_secs(1);

/// moq-lite drafts with and without SUBSCRIBE_END, and IETF drafts over the control stream
/// adapter (14), on their own streams (17), and with subscription fills (20+).
const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-20",
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
	tokio::spawn(support::harness::run(driver));
	producer
}

struct Outcome {
	frames: Vec<Vec<u8>>,
	err: Option<moq_net::Error>,
	/// How long after the held streams were released (or lost) the track ended.
	elapsed: Duration,
}

/// Publish a one-group track, end it while its group stream is held back, then deliver or
/// lose that stream, and return what the subscriber read.
async fn round(version: &str, late: Late) -> Outcome {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.clone());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let reader = tokio::spawn(async move {
		let subscription = moq_net::track::Subscription::default().with_start(moq_net::track::Position::group(0));
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
		(frames, err, tokio::time::Instant::now())
	});

	tokio::time::timeout(TIMEOUT, track.used())
		.await
		.expect("no subscriber appeared")
		.unwrap();

	pair.server_transport.hold_unis();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	drop(track);

	// Paused time only advances once every task is idle, so this runs the publisher to its
	// end of the subscription and the subscriber through reading it.
	tokio::time::sleep(GRACE / 10).await;
	assert!(
		!reader.is_finished(),
		"{version}: the track ended before its group arrived"
	);

	let released = tokio::time::Instant::now();
	match late {
		Late::Delivered => pair.server_transport.release_unis(),
		Late::Lost => pair.server_transport.drop_unis(),
	}

	let (frames, err, ended) = tokio::time::timeout(TIMEOUT, reader)
		.await
		.expect("the subscription never ended")
		.expect("reader panicked");
	drop((pair, broadcast, publisher, subscriber));
	Outcome {
		frames,
		err,
		elapsed: ended - released,
	}
}

/// A group whose header arrives after the subscription's end is delivered, then the track
/// ends cleanly.
#[tokio::test]
async fn a_group_after_the_end_is_delivered() {
	tokio::time::pause();
	for version in VERSIONS {
		let outcome = round(version, Late::Delivered).await;
		assert!(
			outcome.err.is_none() && outcome.frames == [PAYLOAD],
			"{version}: got {} frame(s), err={:?}",
			outcome.frames.len(),
			outcome.err,
		);

		// Drafts that say where the track ends, or how many streams the publisher opened,
		// end as soon as the last group arrives rather than waiting out the grace.
		if *version != "moq-lite-03" {
			assert!(
				outcome.elapsed < GRACE / 10,
				"{version}: ended after {:?}",
				outcome.elapsed
			);
		}
	}
}

/// A group that never arrives is given up on after the grace, and the track still ends
/// cleanly without it.
#[tokio::test]
async fn a_lost_group_ends_the_track_after_the_grace() {
	tokio::time::pause();
	for version in VERSIONS {
		let outcome = round(version, Late::Lost).await;
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

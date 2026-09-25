//! A subscription ends cleanly only when every group in its range has been accounted
//! for; a session closing with a group still in flight ends it with an error.
//!
//! moq-lite, Subscribe: "The publisher closes the stream (FIN) only once every group
//! from start to end has been accounted for, either via a Group Stream (completed or
//! reset) or a SUBSCRIBE_DROP message." And Routing: "when the serving session ends,
//! in-flight subscriptions end with it (a reset)". So `recv_group()` may return
//! `Ok(None)` only after that FIN; any other ending is an error.
//!
//! The publisher opens the final group and writes its first frame (the group stream
//! opens), finishes the track (SUBSCRIBE_END goes out), then writes the rest, finishes
//! the group and disconnects with no await in between, so the final group's tail and
//! its FIN never leave. The close lands before the subscriber reads on. The subscriber
//! must then either see every frame or end with an error; it must not end `Ok(None)`
//! short of the final group.
//!
//! Deterministic: paused time, single-threaded runtime, and every "let the session
//! send" step is an idle-advance rather than a race.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const HEAD: [&[u8]; 2] = [b"head-a", b"head-b"];
const TAIL: [&[u8]; 2] = [b"tail-a", b"tail-b"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Let the drivers run until nothing is runnable: under paused time an idle runtime
/// auto-advances the clock, so this returns once every queued write has been sent.
async fn settle() {
	tokio::time::sleep(Duration::from_millis(1)).await;
}

fn expected() -> Vec<Vec<u8>> {
	HEAD.iter().chain(TAIL.iter()).map(|p| p.to_vec()).collect()
}

/// Publish a two-group track over one mock session, cut the publisher's session with the
/// final group half sent (or keep it up), and return what the subscriber read: the frame
/// payloads and the error it ended with, if any.
async fn round(drop_session: bool) -> (Vec<Vec<u8>>, Option<moq_net::Error>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new("moq-lite-05".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.clone());
	options.client_subscribe = Some(subscriber.clone());
	let MockPair { client, server, .. } = connect_mock(options).await;

	let consumer = subscriber.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	// The publisher only learns of a subscription once the subscriber polls it, so the
	// reader runs concurrently. It reports the head group drained, then waits to be told
	// to read on, so the final group is still unread when the session goes away.
	let (drained_tx, drained_rx) = tokio::sync::oneshot::channel();
	let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();

	let reader = tokio::spawn(async move {
		let subscription = moq_net::track::Subscription::default().with_start(moq_net::track::Position::group(0));
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		let mut got = Vec::new();
		let mut drained = Some(drained_tx);
		let mut go = Some(go_rx);
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return (got, None),
				Err(err) => return (got, Some(err)),
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => got.push(frame.payload.to_vec()),
					Ok(None) => break,
					Err(err) => return (got, Some(err)),
				}
			}
			if let Some(tx) = drained.take() {
				let _ = tx.send(());
				let _ = go.take().unwrap().await;
			}
		}
	});

	tokio::time::timeout(TIMEOUT, track.used())
		.await
		.expect("no subscriber appeared")
		.unwrap();

	let mut head = track.append_group().unwrap();
	for payload in HEAD {
		head.write_frame(Timestamp::ZERO, payload).unwrap();
	}
	head.finish().unwrap();
	tokio::time::timeout(TIMEOUT, drained_rx)
		.await
		.expect("the head group never arrived")
		.expect("reader gone");

	// The final group opens and its first frame is sent, then the track's end is
	// declared: SUBSCRIBE_END reaches the subscriber with the group still open, as it
	// does whenever a finish races a group still in flight.
	let mut tail = track.append_group().unwrap();
	tail.write_frame(Timestamp::ZERO, TAIL[0]).unwrap();
	track.finish().unwrap();
	settle().await;

	// The rest of the group, its clean end, and the disconnect happen with no await in
	// between, so the session has sent none of it when it closes.
	tail.write_frame(Timestamp::ZERO, TAIL[1]).unwrap();
	tail.finish().unwrap();
	drop(track);
	let server = if drop_session {
		drop(server);
		None
	} else {
		Some(server)
	};

	// The close lands before the subscriber reads on, as it does for a subscriber that is
	// merely slower than the network.
	settle().await;
	let _ = go_tx.send(());
	let result = tokio::time::timeout(TIMEOUT, reader)
		.await
		.expect("the subscription never ended")
		.expect("reader panicked");
	drop((client, server, broadcast, publisher, subscriber));
	result
}

/// The session closes with the final group in flight: the subscription ends with an
/// error, or delivers every frame.
#[tokio::test]
async fn a_subscription_cut_by_the_session_close_does_not_end_clean() {
	tokio::time::pause();
	let (got, err) = round(true).await;
	assert!(
		err.is_some() || got == expected(),
		"the subscription ended Ok(None) with {}/{} frames, got {:?} \
		 (a subscription that did not deliver every group must end with an error, not as complete)",
		got.len(),
		expected().len(),
		got.iter()
			.map(|p| String::from_utf8_lossy(p).into_owned())
			.collect::<Vec<_>>(),
	);
}

/// With the session kept alive the same track arrives whole and ends `Ok(None)`.
#[tokio::test]
async fn a_finished_track_ends_clean_while_its_session_lives() {
	tokio::time::pause();
	let (got, err) = round(false).await;
	assert!(
		err.is_none() && got == expected(),
		"session kept alive: got {} frames, err={err:?}",
		got.len()
	);
}

/// Every wire the session-death rule covers: lite before and after the track and
/// subscribe-response streams, and IETF on its shared and per-request control streams.
const DEATH_VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

/// The error the publisher's session is aborted with, carried in the close code.
const DEATH: moq_net::SessionError = moq_net::SessionError::App(7);

/// Kill the publisher's session with a group still open, and return the error the
/// subscriber's `recv_group` ends with once it drains what arrived.
async fn killed(version: &str) -> Option<moq_net::Error> {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.clone());
	options.client_subscribe = Some(subscriber.clone());
	let MockPair { client, server, .. } = connect_mock(options).await;

	let consumer = subscriber.consume();
	consumer.routed("bcast").await.expect("routed");
	let remote = consumer.request_broadcast("bcast").await.expect("broadcast resolves");
	// Subscribing resolves only once the publisher serves the track, which it does
	// only after seeing the subscription, so the reader runs concurrently.
	let reader = tokio::spawn(async move {
		let subscription = moq_net::track::Subscription::default().with_start(moq_net::track::Position::group(0));
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return None,
				Err(err) => return Some(err),
			};
			// The open group ends however it ends; only the track's end is at stake here.
			while let Ok(Some(_)) = group.read_frame().await {}
		}
	});

	track.used().await.expect("no subscriber appeared");
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, HEAD[0]).unwrap();
	settle().await;

	server.abort(moq_net::Error::Session(DEATH));
	let err = reader.await.expect("reader panicked");
	drop((client, server, group, track, broadcast, publisher, subscriber));
	err
}

/// A session dying mid-track ends the subscriber's track with the session's own
/// error: not a clean end, and not a generic `Dropped` or `Cancel`.
#[tokio::test]
async fn a_session_death_ends_the_track_with_its_error() {
	tokio::time::pause();
	for version in DEATH_VERSIONS {
		let err = tokio::time::timeout(TIMEOUT, killed(version))
			.await
			.unwrap_or_else(|_| panic!("{version}: the track never ended"));
		assert!(
			matches!(&err, Some(moq_net::Error::Session(code)) if *code == DEATH),
			"{version}: the track ended with {err:?}, not the session's error"
		);
	}
}

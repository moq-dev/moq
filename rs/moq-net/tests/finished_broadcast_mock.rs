//! Regression: a publisher that finishes its track and then its broadcast, over one
//! direct session with no relay, has the subscription reset with
//! [`Error::Dropped`](moq_net::Error::Dropped) and the track's content lost.
//!
//! The track is complete before the broadcast ends, and the session stays open, so
//! the subscription must conclude normally. moq-lite, ANNOUNCE_END: "Retraction does
//! not disturb subscriptions already in flight, which conclude normally with
//! SUBSCRIBE_END." Instead the track is aborted when the broadcast ends, and
//! whatever the session had not sent yet is gone.
//!
//! Deterministic: time is paused, the runtime is single-threaded, and the publisher
//! writes the track and ends it with no await in between, so the session has sent
//! nothing when the broadcast ends.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const PAYLOAD: &[u8] = b"frame";

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Publish a one-frame track over one mock session, end it cleanly, and return what
/// the subscriber read: the frame payloads and the error it ended with, if any.
async fn round(finish_broadcast: bool) -> (Vec<Vec<u8>>, Option<moq_net::Error>) {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new("moq-lite-05".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.clone());
	options.client_subscribe = Some(subscriber.clone());
	let pair: MockPair = connect_mock(options).await;

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
		let mut got = Vec::new();
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
		}
	});

	tokio::time::timeout(TIMEOUT, track.used())
		.await
		.expect("no subscriber appeared")
		.unwrap();

	// One frame and a textbook clean end, innermost first, with no await in between:
	// nothing is served until the ending is done.
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, PAYLOAD).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	drop(track);
	if finish_broadcast {
		broadcast.close();
	}

	// The session and both origins stay up until the reader is done.
	let result = tokio::time::timeout(TIMEOUT, reader)
		.await
		.expect("the subscription never ended")
		.expect("reader panicked");
	drop((pair, broadcast, publisher, subscriber));
	result
}

fn assert_complete((got, err): (Vec<Vec<u8>>, Option<moq_net::Error>)) {
	assert!(
		err.is_none() && got == [PAYLOAD],
		"got {} frame(s), err={err:?} (a cleanly finished track must deliver its frame, then end with Ok(None))",
		got.len(),
	);
}

/// The bug: finishing the broadcast after its track loses the track.
#[tokio::test]
async fn a_finished_broadcast_delivers_its_finished_track() {
	tokio::time::pause();
	assert_complete(round(true).await);
}

/// Control: the same track with the broadcast left alive arrives whole.
#[tokio::test]
async fn a_finished_track_is_delivered_while_its_broadcast_lives() {
	tokio::time::pause();
	assert_complete(round(false).await);
}

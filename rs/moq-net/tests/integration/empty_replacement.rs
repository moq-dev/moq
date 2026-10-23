#![cfg(feature = "async")]

//! A dynamic handler serves a track, the relay's copy goes idle, and the next request gets a
//! replacement producer that finishes before writing anything. The relay still caches the
//! first producer's group, so the replacement's end must not fall below it: an end there
//! contradicts a received group, which the relay treats as a protocol violation and closes
//! the whole session.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, broadcast, track};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Subscribe to `name` through `remote`, answering the request with a new producer.
async fn serve(
	remote: &broadcast::Consumer,
	dynamic: &mut broadcast::Dynamic,
	name: &str,
) -> (track::Producer, track::Subscriber) {
	let answer = async {
		let request = moq_clock::timeout(TIMEOUT, dynamic.requested_track())
			.await
			.expect("the request never reached the publisher")
			.unwrap();
		request.accept(track::Info::default().with_timescale(moq_net::Timescale::MILLI))
	};
	let subscribing = moq_clock::timeout(TIMEOUT, remote.track(name).unwrap().subscribe(None));
	let (producer, subscriber) = futures::future::join(answer, subscribing).await;
	let subscriber = subscriber.expect("subscribe hung").expect("subscribe failed");
	(producer, subscriber)
}

async fn empty_replacement_ends_after_the_cached_group(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("room").unwrap();
	let mut dynamic = broadcast.dynamic();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let pair = connect_mock(options).await;

	let consumer = relay.consume();
	moq_clock::timeout(TIMEOUT, consumer.routed("room"))
		.await
		.unwrap()
		.unwrap();
	let remote = moq_clock::timeout(TIMEOUT, consumer.request_broadcast("room", None))
		.await
		.unwrap()
		.unwrap();

	// The first producer writes group 0, which the relay caches.
	let (first, mut subscriber) = serve(&remote, &mut dynamic, "feed").await;
	let mut group = first.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"first"[..]).unwrap();
	group.finish().unwrap();
	let group = moq_clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(group.sequence, 0, "{version:?}");
	drop((group, subscriber));

	// The relay's copy goes idle and cancels upstream; the handler finishes and lets go.
	moq_clock::timeout(TIMEOUT, support::unwanted(first.demand()))
		.await
		.unwrap()
		.unwrap();
	first.finish().unwrap();
	drop(first);

	// The idle copy, still holding group 0, subscribes again, and the replacement finishes
	// before writing anything.
	let (second, mut subscriber) = serve(&remote, &mut dynamic, "feed").await;
	second.finish().unwrap();
	assert_eq!(second.end(), Some(track::Position::group(1)), "{version:?}");

	// Its own readers end at once: the earlier producer's groups never arrive here.
	let mut local = second.subscribe(None);
	let ended = moq_clock::timeout(TIMEOUT, local.recv_group())
		.await
		.expect("a local reader of the replacement never ended");
	assert!(matches!(ended, Ok(None)), "{version:?}");

	// The relay's copy ends after the group it cached, and its session stays up.
	loop {
		match moq_clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("the replacement never ended")
		{
			Ok(Some(_)) => {}
			Ok(None) => break,
			Err(err) => panic!("{version:?}: the replacement's end failed the subscription: {err}"),
		}
	}
	if let Ok(err) = moq_clock::timeout(TIMEOUT, pair.client.closed()).await {
		panic!("{version:?}: the relay's session closed: {err}");
	}
}

#[moq_clock::test]
async fn empty_replacement_lite_06() {
	empty_replacement_ends_after_the_cached_group("moq-lite-06").await;
}

#[moq_clock::test]
async fn empty_replacement_lite_07() {
	empty_replacement_ends_after_the_cached_group("moq-lite-07-wip").await;
}

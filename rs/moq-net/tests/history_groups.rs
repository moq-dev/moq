//! A fresh subscription from group 0 receives a finished older group alongside the open
//! newer one, however their streams race into a relay.
//!
//! A relay caches groups in upstream arrival order, and QUIC does not order streams, so
//! the newer group can land first. Resolving the relayed subscription's start from it
//! would drop the older group for good. The mock holds the publisher's group streams and
//! releases them newest first to make that race deterministic.

mod support;

use std::time::Duration;

use moq_net::track::{Info, Position, Subscription};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A budget no group outlives, on both the publisher and the subscriber.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Publish a finished group 5 (three frames) then an open group 6 (two frames), optionally
/// through a relay and with their streams delivered newest first, and return each group's
/// sequence and the frames one subscriber read from it.
async fn round(version: &str, relay: bool, newest_first: bool) -> Vec<(u64, usize)> {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("history", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay_origin = produce_origin(2);
	let upstream = match relay {
		true => {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(relay_origin.clone());
			Some(connect_mock(options).await)
		}
		false => None,
	};

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(match relay {
		true => relay_origin.consume(),
		false => publisher.consume(),
	});
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
		let subscription = Subscription::default()
			.with_max_age(FOREVER)
			.with_start(Position::group(0));
		let mut sub = remote
			.track("history")
			.unwrap()
			.subscribe(subscription)
			.await
			.expect("subscribe");

		// Group 6 never ends, so read until every frame arrived or nothing more does.
		let mut got = Vec::new();
		while got.iter().map(|(_, frames)| frames).sum::<usize>() < 5 {
			let Ok(next) = tokio::time::timeout(TIMEOUT, sub.recv_group()).await else {
				break;
			};
			let Some(mut group) = next.expect("recv_group") else {
				break;
			};
			let mut frames = 0;
			while let Ok(frame) = tokio::time::timeout(TIMEOUT, group.read_frame()).await {
				match frame.expect("read_frame") {
					Some(_) => frames += 1,
					None => break,
				}
			}
			got.push((group.sequence, frames));
		}
		got.sort();
		got
	});

	tokio::time::timeout(TIMEOUT, track.demand().used())
		.await
		.expect("no subscriber appeared")
		.unwrap();

	// The hop the publisher's group streams cross first: into the relay, when there is one.
	let first_hop = upstream
		.as_ref()
		.map_or(&pair.server_transport, |up| &up.server_transport);
	if newest_first {
		first_hop.hold_unis();
	}

	let mut old = track.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
	for _ in 0..3 {
		old.write_frame(Timestamp::now(), &b"old"[..]).unwrap();
	}
	old.finish().unwrap();
	let mut live = track.create_group(moq_net::group::Info { sequence: 6 }).unwrap();
	for _ in 0..2 {
		live.write_frame(Timestamp::now(), &b"live"[..]).unwrap();
	}

	if newest_first {
		// Paused time advances only once every task is idle: both streams are open.
		tokio::time::sleep(Duration::from_millis(10)).await;
		first_hop.release_unis_reversed();
	}

	let got = tokio::time::timeout(TIMEOUT * 3, reader)
		.await
		.expect("reader hung")
		.expect("reader panicked");
	drop((
		live,
		track,
		pair,
		upstream,
		relay_origin,
		broadcast,
		publisher,
		subscriber,
	));
	got
}

#[tokio::test]
async fn a_fresh_subscriber_receives_the_finished_older_group() {
	tokio::time::pause();
	let mut failures = Vec::new();
	for version in VERSIONS {
		for relay in [false, true] {
			for newest_first in [false, true] {
				let got = round(version, relay, newest_first).await;
				if got != [(5, 3), (6, 2)] {
					failures.push(format!("{version} relay={relay} newest_first={newest_first}: {got:?}"));
				}
			}
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// Read known frame counts so the open live group never advances paused time to
/// the relay's linger deadline while waiting for a FIN that will not arrive.
async fn read_history(sub: &mut moq_net::track::Subscriber) {
	let mut sequences = Vec::new();
	for _ in 0..2 {
		let mut group = sub.recv_group().await.unwrap().unwrap();
		let frames = match group.sequence {
			5 => 3,
			6 => 2,
			sequence => panic!("unexpected group {sequence}"),
		};
		for _ in 0..frames {
			assert!(group.read_frame().await.unwrap().is_some());
		}
		sequences.push(group.sequence);
	}
	sequences.sort();
	assert_eq!(sequences, [5, 6]);
}

#[tokio::test]
async fn a_late_subscriber_receives_the_relays_cached_history() {
	tokio::time::pause();
	// Older drafts canonicalize group 0 to the live edge instead of replay.
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("history", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();
	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;
	let first = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(first.clone());
	let first_pair = connect_mock(options).await;
	let consumer = first.consume();
	consumer.routed("bcast").await.unwrap();
	let remote = consumer.request_broadcast("bcast").await.unwrap();
	let subscription = Subscription::default()
		.with_max_age(FOREVER)
		.with_start(Position::group(0));
	let first_subscription = subscription.clone();
	let first_reader = tokio::spawn(async move {
		let mut sub = remote
			.track("history")
			.unwrap()
			.subscribe(first_subscription)
			.await
			.unwrap();
		read_history(&mut sub).await;
		sub
	});
	track.demand().used().await.unwrap();
	upstream.server_transport.hold_unis();
	let mut old = track.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
	for _ in 0..3 {
		old.write_frame(Timestamp::now(), &b"old"[..]).unwrap();
	}
	old.finish().unwrap();
	let mut live = track.create_group(moq_net::group::Info { sequence: 6 }).unwrap();
	for _ in 0..2 {
		live.write_frame(Timestamp::now(), &b"live"[..]).unwrap();
	}
	// Both streams open before paused time advances; 10ms stays below linger.
	tokio::time::sleep(Duration::from_millis(10)).await;
	upstream.server_transport.release_unis_reversed();
	let first_sub = tokio::time::timeout(TIMEOUT, first_reader)
		.await
		.expect("first subscriber history")
		.unwrap();
	let late = produce_origin(4);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(late.clone());
	let late_pair = connect_mock(options).await;
	let consumer = late.consume();
	consumer.routed("bcast").await.unwrap();
	let remote = consumer.request_broadcast("bcast").await.unwrap();
	let mut late_sub = remote.track("history").unwrap().subscribe(subscription).await.unwrap();
	tokio::time::timeout(TIMEOUT, read_history(&mut late_sub))
		.await
		.unwrap_or_else(|_| panic!("{version}: late subscriber history"));
	drop((first_sub, late_sub, first_pair, late_pair, upstream, live));
}

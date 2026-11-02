#![cfg(feature = "async")]

//! A group the subscription's max delay gives up on before the publisher finishes it never
//! reaches the next hop as finished. The served stream resets with `Old`, so a relay does
//! not cache the frames it already has as the whole group.

use crate::support;

use std::time::Duration;

use moq_net::track::{Info, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version};
use support::harness::{peer, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

const BUDGET: Duration = Duration::from_millis(500);

/// IETF peers negotiate the same superseded-group budget as Lite peers.
const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Publish an unfinished group 0 that stalls past the budget after its only frame, and
/// return how a reader without a budget of its own ends group 0 at the downstream hop:
/// `None` when that hop holds no copy of it, otherwise the read's outcome.
async fn round(version: &str) -> Option<Result<Option<usize>, moq_net::Error>> {
	let publisher = produce_origin(1);
	let downstream = produce_origin(2);
	let _pair = peer(version.parse::<Version>().unwrap(), &publisher, &downstream).await;

	let broadcast = publisher.create_broadcast("room").unwrap();
	let track = broadcast
		.create_track("data", Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let consumer = downstream.consume();
	let remote = kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let mut live = kio::clock::timeout(
		TIMEOUT,
		remote
			.track("data")
			.unwrap()
			.subscribe(Subscription::default().with_max_delay(BUDGET)),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe");
	subscribed(&track).await;

	let mut stalled = track.append_group().unwrap();
	stalled.write_frame(Timestamp::ZERO, &b"head"[..]).unwrap();
	let mut group = kio::clock::timeout(TIMEOUT, live.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	assert_eq!(group.sequence, 0);
	assert!(group.read_frame().await.unwrap().is_some());

	// The live edge moves a full second past the stalled group's reach.
	kio::clock::sleep(Duration::from_secs(1)).await;
	for millis in [1000, 2000] {
		let mut next = track.append_group().unwrap();
		next.write_frame(Timestamp::from_millis(millis).unwrap(), &b"edge"[..])
			.unwrap();
		next.finish().unwrap();
	}
	let tail = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("the stalled group never gave up");
	assert!(tail.is_err(), "{version}: the live reader saw a clean end: {tail:?}");
	kio::clock::sleep(Duration::from_millis(100)).await;

	// A reader with no budget to apply sees only what the downstream hop cached.
	let mut cached = kio::clock::timeout(
		TIMEOUT,
		remote.track("data").unwrap().subscribe(
			Subscription::default()
				.with_max_delay(Duration::from_secs(3600))
				.with_range(0..),
		),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe");
	let first = kio::clock::timeout(TIMEOUT, cached.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("a group");
	if first.sequence != 0 {
		stalled.abort(moq_net::Error::Cancel).unwrap();
		return None;
	}
	let mut first = first;
	let mut frames = 0;
	let end = loop {
		match kio::clock::timeout(TIMEOUT, first.read_frame())
			.await
			.expect("read timeout")
		{
			Ok(Some(_)) => frames += 1,
			Ok(None) => break Ok(Some(frames)),
			Err(err) => break Err(err),
		}
	};
	stalled.abort(moq_net::Error::Cancel).unwrap();
	Some(end)
}

#[kio::test]
async fn a_stalled_group_is_never_cached_downstream_as_finished() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		let end = round(version).await;
		if let Some(Ok(_)) = end {
			failures.push(format!("{version}: the downstream hop finished group 0: {end:?}"));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

//! A track's end reaching a relay while an earlier group is still in flight upstream
//! never strands that group: the subscriber behind the relay reads it whole.
//!
//! The subscriber is reading the older group when a newer group and the track's end
//! arrive, and only then does the older group's tail follow. The relay hands out every
//! group up to the end, but must keep pulling the older group's tail for the reader
//! still on it rather than let its upstream subscription go.
//!
//! Deterministic: simulated time, and every "let the sessions send" step is an
//! idle-advance rather than a race.

mod support;

use std::time::Duration;

use moq_net::track::{Info, Position, Subscription};
use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A cache no group outlives on the publisher.
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);

/// The reader's budget, well inside the timeout: moq-lite-03 cannot say where a track
/// ends, so its reader waits this long for a group it cannot account for.
const BUDGET: Duration = Duration::from_secs(1);

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-17",
	"moq-transport-22",
];

const FRAMES: [&[u8]; 2] = [b"frame-a", b"frame-b"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Let the drivers run until nothing is runnable: simulated time advances only once
/// every task is idle, so this returns once every queued write has been sent.
async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(10)).await;
}

/// Publish two groups through a relay, the older one's tail written after the newer
/// group and the track's end. Returns each group's sequence and frame count as read behind
/// the relay and the error the read ended with, or `None` when the read hung.
async fn round(version: &str) -> Option<(Vec<(u64, usize)>, Option<moq_net::Error>)> {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("video", Info::default().with_max_age(FOREVER))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let subscriber = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(subscriber.clone());
	let downstream = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	let reader = moq_net_sim::spawn(async move {
		let subscription = Subscription::default()
			.with_max_delay(BUDGET)
			.with_start(Position::group(0));
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
				Ok(None) => break (got, None),
				Err(err) => break (got, Some(err)),
			};
			let mut frames = 0;
			let err = loop {
				match group.read_frame().await {
					Ok(Some(_)) => frames += 1,
					Ok(None) => break None,
					Err(err) => break Some(err),
				}
			};
			got.push((group.sequence, frames));
			if err.is_some() {
				break (got, err);
			}
		}
	});

	moq_net_sim::timeout(TIMEOUT, track.demand().used())
		.await
		.expect("no subscriber appeared")
		.unwrap();
	settle().await;

	// The older group's head reaches the reader.
	let mut old = track.append_group().unwrap();
	old.write_frame(Timestamp::now(), FRAMES[0]).unwrap();
	settle().await;

	// A newer group and the end arrive while the older group is still open.
	let mut new = track.append_group().unwrap();
	for frame in FRAMES {
		new.write_frame(Timestamp::now(), frame).unwrap();
	}
	new.finish().unwrap();
	track.finish().unwrap();
	settle().await;

	// Then the older group's tail.
	old.write_frame(Timestamp::now(), FRAMES[1]).unwrap();
	old.finish().unwrap();

	let result = moq_net_sim::timeout(TIMEOUT, reader)
		.await
		.ok()
		.map(|res| res.expect("reader panicked"));
	drop((
		old, new, track, upstream, downstream, relay, broadcast, publisher, subscriber,
	));
	result.map(|(mut got, err)| {
		got.sort();
		(got, err)
	})
}

#[moq_net_sim::test]
async fn an_end_never_strands_a_group_in_flight_through_a_relay() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		match round(version).await {
			Some((got, None)) if got == [(0, FRAMES.len()), (1, FRAMES.len())] => {}
			Some((got, err)) => failures.push(format!("{version}: got {got:?}, err {err:?}")),
			None => failures.push(format!("{version}: the read behind the relay hung")),
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

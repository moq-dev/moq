#![cfg(feature = "async")]

//! Fetching a group from a moq-transport publisher never subscribes to its track.
//!
//! The subscriber learns the track from TRACK_STATUS instead, so a finished track is still
//! fetchable, and a relay's fetch-only demand puts no live subscription upstream to race
//! the fetches that report where the track ends.

use crate::support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn resolve(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	kio::clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

/// Publish a finished track of three groups, and flag any subscription it ever gets.
fn publish_finished(origin: &moq_net::origin::Producer) -> (moq_net::origin::Publication, Arc<AtomicBool>) {
	let broadcast = origin.create_broadcast("bcast").unwrap();
	let mut track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..3u64 {
		let mut group = track.append_group().unwrap();
		for frame in 0..2 {
			group
				.write_frame(Timestamp::ZERO, format!("{sequence}-{frame}").into_bytes())
				.unwrap();
		}
		group.finish().unwrap();
	}
	track.finish().unwrap();

	let subscribed = Arc::new(AtomicBool::new(false));
	let flag = subscribed.clone();
	drop(kio::clock::sim::spawn(async move {
		while let Ok(subscription) = track.subscription_changed().await {
			if subscription.is_some() {
				flag.store(true, Ordering::SeqCst);
			}
		}
	}));
	(broadcast, subscribed)
}

async fn fetch(broadcast: &moq_net::broadcast::Consumer, sequence: u64) -> Result<Vec<Vec<u8>>, moq_net::Error> {
	let mut group = kio::clock::timeout(TIMEOUT, async {
		broadcast.track("video").unwrap().fetch_group(sequence, None).await
	})
	.await
	.expect("fetch timed out")?;
	let mut frames = Vec::new();
	while let Some(frame) = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read timed out")?
	{
		frames.push(frame.payload.to_vec());
	}
	Ok(frames)
}

fn frames(sequence: u64) -> Vec<Vec<u8>> {
	(0..2).map(|frame| format!("{sequence}-{frame}").into_bytes()).collect()
}

const VERSIONS: &[&str] = &["moq-transport-18", "moq-transport-22"];

async fn direct(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (broadcast, subscribed) = publish_finished(&publisher);

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let pair = connect_mock(options).await;

	let remote = resolve(&client).await;
	assert_eq!(fetch(&remote, 1).await.expect("fetch"), frames(1), "{version}");
	assert!(!subscribed.load(Ordering::SeqCst), "{version}: the fetch subscribed");

	drop((remote, pair, broadcast, client, publisher));
}

/// A finished track is fetched without a SUBSCRIBE, which it would refuse.
#[kio::test]
async fn a_fetch_learns_the_track_without_subscribing() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		if kio::clock::sim::spawn(direct(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

async fn relayed(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (broadcast, subscribed) = publish_finished(&publisher);

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let client = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let downstream = connect_mock(options).await;

	let remote = resolve(&client).await;
	for sequence in [2, 0] {
		assert_eq!(
			fetch(&remote, sequence).await.expect("fetch"),
			frames(sequence),
			"{version}"
		);
	}
	assert!(
		!subscribed.load(Ordering::SeqCst),
		"{version}: the relay subscribed upstream"
	);

	drop((remote, downstream, upstream, broadcast, client, relay, publisher));
}

/// A subscriber arriving at a copy learned from TRACK_STATUS still gets timestamped
/// frames: the copy is timed from TRACK_STATUS_OK, and the resumed SUBSCRIBE keeps it so.
#[kio::test]
async fn a_subscribe_after_track_status_keeps_timestamps() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TIMEOUT, async {
			let publisher = produce_origin(1);
			let broadcast = publisher.create_broadcast("bcast").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let client = produce_origin(2);
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(client.clone());
			let _pair = connect_mock(options).await;

			let remote = resolve(&client).await;
			let copy = remote.track("video").unwrap();
			let info = copy.info().await.expect("TRACK_STATUS resolves the track");
			assert!(info.timescale.is_some(), "{version}: the copy is untimed");
			assert!(track.subscription().is_none(), "{version}: the query subscribed");

			let mut sub = copy.subscribe(None).await.unwrap();
			crate::support::wanted(track.demand()).await.unwrap();
			// Far from any arrival time, so a frame stamped on arrival cannot pass.
			let stamp = Timestamp::from_millis(123_456).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(stamp, b"frame".as_ref()).unwrap();
			group.finish().unwrap();

			let mut group = sub.recv_group().await.unwrap().unwrap();
			let frame = group.read_frame().await.unwrap().unwrap();
			assert_eq!(
				frame.timestamp.map(Timestamp::as_millis),
				Some(stamp.as_millis()),
				"{version}"
			);
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A relay serves a downstream fetch with a fetch upstream, never a subscription.
#[kio::test]
async fn a_relay_fetches_without_subscribing_upstream() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		if kio::clock::sim::spawn(relayed(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

/// Subscribe to group 1 alone: history below the Largest Object a publisher of
/// [`publish_finished`] reports. Our moq-transport publishers answer only a one-group FETCH.
async fn subscribe_history(track: &moq_net::track::Consumer) -> moq_net::track::Subscriber {
	let subscription = moq_net::track::Subscription::default()
		.with_order(moq_net::track::Order::Ascending)
		.with_range(1..2);
	kio::clock::timeout(TIMEOUT, track.subscribe(subscription))
		.await
		.expect("subscribe timed out")
		.expect("subscribe")
}

/// The next group `sub` hands out, read whole.
async fn next_frames(sub: &mut moq_net::track::Subscriber) -> Option<Vec<Vec<u8>>> {
	let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("recv timed out")
		.expect("recv group")?;
	let mut frames = Vec::new();
	while let Some(frame) = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read timed out")
		.expect("read frame")
	{
		frames.push(frame.payload.to_vec());
	}
	Some(frames)
}

async fn bounded_direct(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (broadcast, subscribed) = publish_finished(&publisher);

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let pair = connect_mock(options).await;

	let remote = resolve(&client).await;
	let copy = remote.track("video").unwrap();
	copy.info().await.expect("TRACK_STATUS resolves the track");
	// A front's bounded read still parks past its end, as each copy's reader does, so this
	// reads the group and stops there.
	let mut sub = subscribe_history(&copy).await;
	assert_eq!(next_frames(&mut sub).await, Some(frames(1)), "{version}");
	assert!(
		!subscribed.load(Ordering::SeqCst),
		"{version}: the bounded read subscribed"
	);

	drop((sub, copy, remote, pair, broadcast, client, publisher));
}

/// A copy the session holds no subscription for is unfed: a bounded read of the history
/// below the publisher's Largest Object fetches its holes at once, never subscribing.
#[kio::test]
async fn a_bounded_read_of_history_fetches_without_subscribing() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		if kio::clock::sim::spawn(bounded_direct(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

async fn bounded_relayed(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (broadcast, subscribed) = publish_finished(&publisher);

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	let client = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let downstream = connect_mock(options).await;

	// A fetch leaves the relay holding a copy no subscription feeds.
	let remote = resolve(&client).await;
	assert_eq!(fetch(&remote, 2).await.expect("fetch"), frames(2), "{version}");

	let copy = remote.track("video").unwrap();
	let mut sub = subscribe_history(&copy).await;
	assert_eq!(next_frames(&mut sub).await, Some(frames(1)), "{version}");
	assert_eq!(next_frames(&mut sub).await, None, "{version}: the read ends");
	assert!(
		!subscribed.load(Ordering::SeqCst),
		"{version}: the relay subscribed upstream"
	);

	drop((
		sub, copy, remote, downstream, upstream, broadcast, client, relay, publisher,
	));
}

/// A relay serves a bounded subscription to its fetch-only copy from the fills it asks
/// upstream, never subscribing upstream.
#[kio::test]
async fn a_relay_serves_a_bounded_read_of_history_without_subscribing_upstream() {
	let mut failures = Vec::new();
	for version in VERSIONS {
		if kio::clock::sim::spawn(bounded_relayed(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

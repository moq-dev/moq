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
	let mut sub = subscribe_history(&copy).await;
	assert_eq!(next_frames(&mut sub).await, Some(frames(1)), "{version}");
	assert_eq!(next_frames(&mut sub).await, None, "{version}: the read ends");
	assert!(
		!subscribed.load(Ordering::SeqCst),
		"{version}: the bounded read subscribed"
	);

	drop((sub, copy, remote, pair, broadcast, client, publisher));
}

/// A bounded read learns the track from TRACK_STATUS, and of the history below the
/// publisher's Largest Object, which a finished track's SUBSCRIBE would refuse, fetches its
/// holes at once, never subscribing.
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

/// Write group `sequence` of [`frames`] to a live track.
fn write_group(track: &moq_net::track::Producer, sequence: u64) {
	let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
	for frame in frames(sequence) {
		group.write_frame(Timestamp::ZERO, frame).unwrap();
	}
	group.finish().unwrap();
}

/// Publish a live track of groups `0..groups`, which the publisher keeps writing.
fn publish_live(
	origin: &moq_net::origin::Producer,
	groups: u64,
) -> (moq_net::origin::Publication, moq_net::track::Producer) {
	let broadcast = origin.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	for sequence in 0..groups {
		write_group(&track, sequence);
	}
	(broadcast, track)
}

/// A relay of a live track holding a copy a fetch left unfed, and a fresh session to it.
struct FetchedCopy {
	/// The publisher's track, which has since moved past the edge the fetch learned.
	track: moq_net::track::Producer,
	/// The track through the fresh session, which knows no edge, so its reads SUBSCRIBE.
	fresh: moq_net::track::Consumer,
	/// The broadcast through the session that fetched, another reader of the relay's copy.
	fetched: moq_net::broadcast::Consumer,
	_held: Vec<Box<dyn std::any::Any>>,
}

/// The relay speaks `upstream_version` to the publisher and `version` downstream.
async fn fetched_copy(upstream_version: Version, version: Version) -> FetchedCopy {
	let publisher = produce_origin(1);
	let (broadcast, track) = publish_live(&publisher, 3);

	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(upstream_version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let upstream = connect_mock(options).await;

	// The fetch learns Largest Object (2, 1) from TRACK_STATUS, and subscribes nowhere.
	let fetcher = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(fetcher.clone());
	let fetching = connect_mock(options).await;
	let fetched = resolve(&fetcher).await;
	assert_eq!(fetch(&fetched, 2).await.expect("fetch"), frames(2), "{version}");
	assert!(track.subscription().is_none(), "{version}: the fetch subscribed");

	// The edge moves on, so an answer from the earlier report would be stale.
	write_group(&track, 3);

	let client = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(client.clone());
	let downstream = connect_mock(options).await;
	let fresh = resolve(&client).await.track("video").unwrap();

	FetchedCopy {
		track,
		fresh,
		fetched,
		_held: vec![
			Box::new(downstream),
			Box::new(client),
			Box::new(fetching),
			Box::new(fetcher),
			Box::new(upstream),
			Box::new(relay),
			Box::new(broadcast),
			Box::new(publisher),
		],
	}
}

async fn history_through_fetched_copy(upstream: Version, version: Version) {
	let copy = fetched_copy(upstream, version).await;
	// The fresh session asks TRACK_STATUS first, which the relay answers without an edge,
	// so the read SUBSCRIBEs once that answer arrives, after `subscribe` returns.
	let mut sub = subscribe_history(&copy.fresh).await;
	assert_eq!(next_frames(&mut sub).await, Some(frames(1)), "{upstream} to {version}");
	assert_eq!(
		next_frames(&mut sub).await,
		None,
		"{upstream} to {version}: the read ends"
	);
	assert!(
		copy.track.subscription().is_some(),
		"{upstream} to {version}: answered without subscribing upstream"
	);
	// The copy still serves its other reader.
	assert_eq!(
		fetch(&copy.fetched, 0).await.expect("fetch after the SUBSCRIBE"),
		frames(0),
		"{upstream} to {version}"
	);
}

/// A history SUBSCRIBE reaching a relay's copy kept after a fetch is answered from the
/// upstream's SUBSCRIBE_OK, never the edge the fetch learned, and reads its history. A
/// draft-18 upstream has no bounded join, so the relay joins it live and serves the history
/// from fills, keeping the copy for its other readers.
#[kio::test]
async fn a_relay_answers_a_history_subscribe_on_a_fetched_copy() {
	for (upstream, version) in [
		("moq-transport-22", "moq-transport-22"),
		("moq-transport-18", "moq-transport-22"),
		("moq-transport-18", "moq-transport-18"),
	] {
		let (upstream, version): (Version, Version) = (upstream.parse().unwrap(), version.parse().unwrap());
		kio::clock::timeout(TIMEOUT, history_through_fetched_copy(upstream, version))
			.await
			.unwrap_or_else(|_| panic!("{upstream} to {version}: timed out"));
	}
}

async fn live_through_fetched_copy(version: Version) {
	let copy = fetched_copy(version, version).await;
	let mut sub = kio::clock::timeout(TIMEOUT, copy.fresh.subscribe(None))
		.await
		.expect("subscribe timed out")
		.expect("subscribe");
	// The live join starts at the upstream's current group, not the one the fetch reported.
	assert_eq!(next_frames(&mut sub).await, Some(frames(3)), "{version}");
	write_group(&copy.track, 4);
	assert_eq!(next_frames(&mut sub).await, Some(frames(4)), "{version}");
}

/// A live SUBSCRIBE reaching a relay's copy kept after a fetch joins at the upstream's
/// current edge.
#[kio::test]
async fn a_relay_answers_a_live_subscribe_on_a_fetched_copy() {
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		kio::clock::timeout(TIMEOUT, live_through_fetched_copy(version))
			.await
			.unwrap_or_else(|_| panic!("{version}: timed out"));
	}
}

/// A copy kept after a fetch resumes a bounded subscription that ends at or below the
/// upstream's new Largest Object, and reads it, though no group past its end arrives.
/// A draft-18 join fills the missing run with one FETCH, which our publisher refuses
/// until hole markers are supported. That refusal ends the read instead of hanging.
#[kio::test]
async fn a_resumed_bounded_subscription_reads_below_the_answered_edge() {
	for version in ["moq-transport-18", "moq-transport-20", "moq-transport-22"] {
		kio::clock::timeout(TIMEOUT, async {
			let version: Version = version.parse().unwrap();
			let publisher = produce_origin(1);
			let (broadcast, track) = publish_live(&publisher, 2);

			let client = produce_origin(2);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(client.clone());
			let pair = connect_mock(options).await;

			// The fetch learns Largest Object (1, 1), and leaves the copy unfed.
			let remote = resolve(&client).await;
			assert_eq!(fetch(&remote, 0).await.expect("fetch"), frames(0));
			for sequence in 2..4 {
				write_group(&track, sequence);
			}

			// Past the reported edge, so it subscribes; the answer names group 3, which the
			// bounded feed never delivers.
			let copy = remote.track("video").unwrap();
			let subscription = moq_net::track::Subscription::default()
				.with_order(moq_net::track::Order::Ascending)
				.with_delay(moq_net::track::Delay::default().with_max(None))
				.with_range(1..3);
			let mut sub = kio::clock::timeout(TIMEOUT, copy.subscribe(subscription))
				.await
				.expect("subscribe timed out")
				.expect("subscribe");
			// Read in arrival order, whichever group lands first. A refused multi-group
			// FETCH gives up the whole run; it never falls back to fetching each group.
			let mut read = Vec::new();
			while let Some(group) = next_frames(&mut sub).await {
				read.push(group);
			}
			read.sort();
			if version == "moq-transport-18".parse().unwrap() {
				assert!(read.is_empty(), "the unsupported multi-group FETCH was refused");
			} else {
				assert_eq!(read, [frames(1), frames(2)]);
			}
			assert!(track.subscription().is_some(), "the read never subscribed");

			drop((sub, copy, remote, pair, track, broadcast, client, publisher));
		})
		.await
		.expect("timed out");
	}
}

/// A copy that went idle holding a group resumes a bounded subscription of that group after
/// the upstream moved on, and still reads it: the feed starts where it asked, not at the
/// answer's Largest. A live join also keeps the cached history for an unlimited read.
#[kio::test]
async fn a_resumed_bounded_subscription_keeps_its_cached_start() {
	let versions = moq_net::Versions::all();
	for version in versions.iter().copied().chain(["moq-lite-07-wip".parse().unwrap()]) {
		kio::clock::timeout(TIMEOUT, async {
			let publisher = produce_origin(1);
			let (broadcast, track) = publish_live(&publisher, 1);

			let client = produce_origin(2);
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(client.clone());
			let pair = connect_mock(options).await;

			// A live read caches group 0, then leaves, so the copy goes idle holding it.
			let remote = resolve(&client).await;
			let copy = remote.track("video").unwrap();
			let mut live = kio::clock::timeout(TIMEOUT, copy.subscribe(None))
				.await
				.expect("subscribe timed out")
				.expect("subscribe");
			assert_eq!(next_frames(&mut live).await, Some(frames(0)));
			drop(live);
			let mut watch = track.clone();
			while watch.subscription().is_some() {
				kio::clock::timeout(TIMEOUT, watch.subscription_changed())
					.await
					.expect("unsubscribe timed out")
					.unwrap();
			}
			for sequence in 1..4 {
				write_group(&track, sequence);
			}

			// Past the edge the live read learned, so it subscribes; the answer names group 3.
			let subscription = moq_net::track::Subscription::default()
				.with_delay(moq_net::track::Delay::default().with_max(None))
				.with_range(0..1);
			let mut sub = kio::clock::timeout(TIMEOUT, copy.subscribe(subscription))
				.await
				.expect("subscribe timed out")
				.expect("subscribe");
			assert_eq!(next_frames(&mut sub).await, Some(frames(0)), "{version}");

			drop((sub, copy, remote, pair, track, broadcast, client, publisher));
		})
		.await
		.expect("timed out");
	}
}

/// A relay's retained group cannot hide a missing earlier group in the same read.
#[kio::test]
async fn a_relay_accounts_for_every_group_in_multi_group_history() {
	for (upstream, downstream) in [
		("moq-transport-18", "moq-transport-22"),
		("moq-transport-22", "moq-transport-22"),
		("moq-transport-18", "moq-lite-07-wip"),
		("moq-transport-22", "moq-lite-07-wip"),
	] {
		let copy = fetched_copy(upstream.parse().unwrap(), downstream.parse().unwrap()).await;
		let subscription = moq_net::track::Subscription::default()
			.with_order(moq_net::track::Order::Ascending)
			.with_delay(moq_net::track::Delay::default().with_max(None))
			.with_range(1..3);
		let mut sub = copy.fresh.subscribe(subscription).await.unwrap();
		let mut groups = Vec::new();
		while let Some(group) = next_frames(&mut sub).await {
			groups.push(group);
		}
		groups.sort();
		assert_eq!(groups, [frames(1), frames(2)], "{upstream} to {downstream}");
	}
}

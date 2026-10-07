//! Fetching a group from a moq-transport publisher never subscribes to its track.
//!
//! The subscriber learns the track from TRACK_STATUS instead, so a finished track is still
//! fetchable, and a relay's fetch-only demand puts no live subscription upstream to race
//! the fetches that report where the track ends.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use moq_net::{Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn resolve(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

/// Publish a finished track of three groups, and flag any subscription it ever gets.
fn publish_finished(origin: &moq_net::origin::Producer) -> (moq_net::broadcast::Producer, Arc<AtomicBool>) {
	let broadcast = origin.create_broadcast("bcast").unwrap();
	let mut track = broadcast.create_track("video", None).unwrap();
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
	drop(moq_net_sim::spawn(async move {
		while let Ok(subscription) = track.subscription_changed().await {
			if subscription.is_some() {
				flag.store(true, Ordering::SeqCst);
			}
		}
	}));
	(broadcast, subscribed)
}

async fn fetch(broadcast: &moq_net::broadcast::Consumer, sequence: u64) -> Result<Vec<Vec<u8>>, moq_net::Error> {
	let mut group = moq_net_sim::timeout(TIMEOUT, async {
		broadcast.track("video").unwrap().fetch_group(sequence, None).await
	})
	.await
	.expect("fetch timed out")?;
	let mut frames = Vec::new();
	while let Some(frame) = moq_net_sim::timeout(TIMEOUT, group.read_frame())
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

/// Drafts whose FETCH we serve, and the newest, whose FETCH we refuse until draft-20
/// FETCH lands: it must still not subscribe.
const VERSIONS: &[(&str, bool)] = &[
	("moq-transport-14", true),
	("moq-transport-15", true),
	("moq-transport-16", true),
	("moq-transport-17", true),
	("moq-transport-18", true),
	("moq-transport-19", true),
	("moq-transport-22", false),
];

async fn direct(version: &str, served: bool) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let (broadcast, subscribed) = publish_finished(&publisher);

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let pair = connect_mock(options).await;

	let remote = resolve(&client).await;
	let fetched = fetch(&remote, 1).await;
	match served {
		true => assert_eq!(fetched.expect("fetch"), frames(1), "{version}"),
		false => assert!(fetched.is_err(), "{version}: draft-20 FETCH is refused"),
	}
	assert!(!subscribed.load(Ordering::SeqCst), "{version}: the fetch subscribed");

	drop((remote, pair, broadcast, client, publisher));
}

/// A finished track is fetched without a SUBSCRIBE, which it would refuse.
#[moq_net_sim::test]
async fn a_fetch_learns_the_track_without_subscribing() {
	let mut failures = Vec::new();
	for (version, served) in VERSIONS {
		if moq_net_sim::spawn(direct(version, *served)).await.is_err() {
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

/// A relay serves a downstream fetch with a fetch upstream, never a subscription.
#[moq_net_sim::test]
async fn a_relay_fetches_without_subscribing_upstream() {
	let mut failures = Vec::new();
	for (version, served) in VERSIONS {
		if *served && moq_net_sim::spawn(relayed(version)).await.is_err() {
			failures.push(*version);
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}

#![cfg(feature = "async")]

//! A broadcast nested under another's path is reached through its own claim, whichever
//! publisher's node id sorts first.
//!
//! A claim covers every path beneath its prefix, so the publisher of `src` also claims
//! `src/derived`, and refuses it. A stream answering a request for exactly `src/derived`
//! presents that covering claim at the request itself, beside the derivative's own claim.
//! A viewer ranks the two alike unless the presented claim says how much broader it is,
//! and the order the relay lists them in, by node id, then decides which one it asks, so the
//! same setup reaches the derivative on some runs and is refused on others.

use crate::support;
use support::AppendNext;
use support::AtStart;

use std::time::Duration;

use moq_net::{Prefix, Prefixes, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// Have `to` pull everything `from` publishes.
async fn link(from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(lite07());
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// Have `to` pull only what `from` publishes at or below `prefix`, as a viewer following
/// that one path asks.
async fn follow(from: &origin::Producer, to: &origin::Producer, prefix: &str) -> MockPair {
	let interest = Prefixes::from(Prefix::try_from(prefix).unwrap());
	let mut options = MockConnectOptions::new(lite07());
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.scope("", &interest).unwrap());
	connect_mock(options).await
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_millis(500)).await;
}

/// Publish `path` on `node` with a `video` track whose one frame is `tag`.
fn publish(node: &origin::Producer, path: &str, tag: &'static str) -> (origin::Route, track::Producer) {
	let route = node.route(path, origin::RouteOptions::hidden()).unwrap();
	let track = route
		.broadcast()
		.create_track(
			"video",
			track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let mut group = track.append_next().unwrap();
	group.write_frame(group.at_start(), tag.as_bytes().to_vec()).unwrap();
	group.finish().unwrap();
	route.announce().unwrap();
	(route, track)
}

/// The first frame of `video` at `path` through `viewer`.
async fn first(viewer: &origin::Producer, path: &str) -> String {
	let broadcast = viewer.consume().request(path, None).await.unwrap();
	let mut subscription = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
	let mut group = kio::clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.unwrap()
		.expect("the track ended before a group");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

/// A viewer behind a relay reaches the nested broadcast, not the broadcast above it, whether
/// the relay lists the claim above it first or last: equal claims rank the newest first, and
/// the relay lists them by node id.
#[kio::test]
async fn a_nested_broadcast_wins_over_the_claim_above_it() {
	for (source_hop, derived_hop) in [(10, 20), (20, 10)] {
		let relay = produce_origin(1);
		let source = produce_origin(source_hop);
		let derived = produce_origin(derived_hop);
		let viewer = produce_origin(2);

		let _published = [
			publish(&source, "src", "source"),
			publish(&derived, "src/derived", "derived"),
		];
		let _links = [
			link(&source, &relay).await,
			link(&derived, &relay).await,
			follow(&relay, &viewer, "src/derived").await,
		];
		settle().await;
		assert_eq!(
			first(&viewer, "src/derived").await,
			"derived",
			"source node {source_hop}, derivative node {derived_hop}"
		);
	}
}

/// A claim at the origin's root, exported through a handle rooted at `tenant`, covers all the
/// peer sees: at the peer's empty prefix it is no broader than the prefix is deep, so the peer
/// keeps the session.
#[kio::test]
async fn a_claim_above_a_rooted_export_keeps_the_session() {
	let relay = produce_origin(1);
	let source = produce_origin(10);
	let _published = publish(&source, "", "root");

	let mut options = MockConnectOptions::new(lite07());
	options.server_publish = Some(source.consume().scope("tenant", &Prefixes::all()).unwrap());
	options.client_subscribe = Some(relay.clone());
	let pair = connect_mock(options).await;
	settle().await;
	assert_eq!(pair.client_transport.close_reason(), None);
	assert_eq!(pair.server_transport.close_reason(), None);
}

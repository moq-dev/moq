#![cfg(feature = "async")]

//! Metadata-only subscriptions: a reader asking for `Subscription::meta` gets each frame's
//! size and timestamp without its payload, through relays, and asking for the payload again
//! serves the groups it saw as metadata with their bytes.

use crate::support;
use support::AppendNext;
use support::AtStart;

use std::time::Duration;

use moq_net::track::Subscription;
use moq_net::{Error, Version, origin, track};
use support::harness::subscribed;

use super::route_change::{link, payload, produce_origin, settle, timed};

const TIMEOUT: Duration = Duration::from_secs(10);

const LITE07: &str = "moq-lite-07-wip";

/// A payload big enough that carrying it by mistake shows in the byte counts.
const SIZE: usize = 4096;

/// A publisher, a relay, and a subscriber, each pulling the one before it.
struct Chain {
	publisher: origin::Producer,
	relay: origin::Producer,
	subscriber: origin::Producer,
	_links: Vec<support::harness::MockPair>,
}

impl Chain {
	async fn new(version: Version) -> Self {
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let subscriber = produce_origin(3);
		let links = vec![
			link(version, &publisher, &relay).await,
			link(version, &relay, &subscriber).await,
		];
		Self {
			publisher,
			relay,
			subscriber,
			_links: links,
		}
	}

	/// Publish the track `data` in the broadcast `room`.
	fn publish(&self) -> (origin::Route, track::Producer) {
		let route = self
			.publisher
			.route("room", origin::RouteOptions::hidden())
			.unwrap();
		let track = route.broadcast().create_track("data", timed()).unwrap();
		route.announce().unwrap();
		(route, track)
	}

	/// Subscribe to `data` at the subscriber's end of the chain.
	async fn subscribe(&self, subscription: Subscription) -> track::Subscriber {
		let remote = kio::clock::timeout(TIMEOUT, self.subscriber.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.expect("subscribe")
	}
}

/// Every group from 0, oldest first, asking for metadata only when `meta`. A max delay keeps
/// an older group arriving after a newer one readable.
fn from_start(meta: bool) -> Subscription {
	Subscription::default()
		.with_delay(track::Delay::default().with_max(Duration::from_secs(5)))
		.with_order(track::Order::Ascending)
		.with_ranges([support::keys(0..)])
		.with_meta(meta)
}

/// What the publisher's track is asked for once a subscription reaches it.
async fn asked(track: &track::Producer) -> Subscription {
	let mut track = track.clone();
	loop {
		if let Some(subscription) = kio::clock::timeout(TIMEOUT, track.subscription_changed())
			.await
			.expect("subscription timeout")
			.expect("track open")
		{
			return subscription;
		}
	}
}

/// A frame of `SIZE` bytes, starting with what [`payload`] names it.
fn big(group: u64, frame: u64) -> Vec<u8> {
	let mut bytes = payload(group, frame);
	bytes.resize(SIZE, b'.');
	bytes
}

/// The group `key`, skipping any other the reader gets first: groups arrive in any order.
async fn recv_key(sub: &mut track::Subscriber, key: u64) -> moq_net::group::Consumer {
	loop {
		let group = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		if group.start == key {
			return group;
		}
	}
}

/// Group `key`'s frame headers, which a metadata reader gets without the payload.
async fn infos(sub: &mut track::Subscriber, key: u64) -> Vec<moq_net::frame::Info> {
	let mut group = recv_key(sub, key).await;
	assert!(group.is_meta(), "a metadata reader got the payload");
	let mut infos = Vec::new();
	while let Some(info) = kio::clock::timeout(TIMEOUT, group.read_info())
		.await
		.expect("frame timeout")
		.unwrap()
	{
		infos.push(info);
	}
	infos
}

/// Group `key`'s payloads.
async fn frames(sub: &mut track::Subscriber, key: u64) -> Vec<Vec<u8>> {
	let mut group = recv_key(sub, key).await;
	assert!(!group.is_meta(), "a payload reader got a metadata copy");
	let mut frames = Vec::new();
	while let Some(frame) = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.unwrap()
	{
		frames.push(frame.payload.to_vec());
	}
	frames
}

/// Write group `key` with `frames` frames of [`big`] payloads, leaving it open.
fn open_group(track: &track::Producer, key: u64, frames: u64) -> moq_net::group::Producer {
	let mut group = track.append_next().unwrap();
	assert_eq!(group.start, key);
	for frame in 0..frames {
		let at = group.at_start();
		group.write_frame(at, big(key, frame)).unwrap();
	}
	group
}

/// Write group `key` with `frames` frames of [`big`] payloads, and close it.
fn write_group(track: &track::Producer, key: u64, frames: u64) {
	open_group(track, key, frames).close().unwrap();
}

#[kio::test]
async fn metadata_crosses_a_relay_without_payload() {
	let chain = Chain::new(LITE07.parse().unwrap()).await;
	let (_route, track) = chain.publish();
	let mut sub = chain.subscribe(from_start(true)).await;
	subscribed(&track).await;
	assert!(asked(&track).await.meta, "the relay asked its upstream for the payload");

	write_group(&track, 0, 3);
	let infos = infos(&mut sub, 0).await;
	assert_eq!(infos.len(), 3);
	assert!(infos.iter().all(|info| info.size == SIZE as u64), "{infos:?}");

	// An empty frame keeps its place.
	let mut group = track.append_next().unwrap();
	group.write_frame(group.at_start(), &b""[..]).unwrap();
	group.close().unwrap();
	let empty = self::infos(&mut sub, 1).await;
	assert_eq!(empty.iter().map(|info| info.size).collect::<Vec<_>>(), vec![0]);

	// The relay cached the headers alone: a metadata reader there reads the copy that
	// crossed the hop, which holds no payload.
	let remote = kio::clock::timeout(TIMEOUT, chain.relay.consume().follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();
	let mut cached = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(from_start(true)))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	assert!(recv_key(&mut cached, 0).await.is_meta(), "the relay cached the payload");
}

#[kio::test]
async fn a_metadata_group_refuses_a_payload_read() {
	let chain = Chain::new(LITE07.parse().unwrap()).await;
	let (_route, track) = chain.publish();
	let mut sub = chain.subscribe(from_start(true)).await;
	subscribed(&track).await;

	write_group(&track, 0, 1);
	let mut group = recv_key(&mut sub, 0).await;
	assert!(group.is_meta());
	assert!(matches!(group.read_frame().await, Err(Error::Unsupported)));
}

#[kio::test]
async fn mixed_demand_serves_each_reader_its_mode() {
	let chain = Chain::new(LITE07.parse().unwrap()).await;
	let (_route, track) = chain.publish();
	let mut meta = chain.subscribe(from_start(true)).await;
	let mut full = chain.subscribe(from_start(false)).await;
	subscribed(&track).await;
	settle().await;
	assert!(!asked(&track).await.meta, "a payload reader's demand asks for the payload");

	write_group(&track, 0, 2);
	assert_eq!(frames(&mut full, 0).await, vec![big(0, 0), big(0, 1)]);
	// The metadata reader reads the payload copy's headers.
	let mut group = recv_key(&mut meta, 0).await;
	let info = group.read_info().await.unwrap().expect("a frame");
	assert_eq!(info.size, SIZE as u64);
}

#[kio::test]
async fn a_payload_reader_joining_promotes_what_arrived_as_metadata() {
	let chain = Chain::new(LITE07.parse().unwrap()).await;
	let (_route, track) = chain.publish();
	let mut meta = chain.subscribe(from_start(true)).await;
	subscribed(&track).await;

	write_group(&track, 0, 2);
	let mut open = open_group(&track, 1, 1);
	assert_eq!(infos(&mut meta, 0).await.len(), 2);
	settle().await;

	// The payload is asked for again: every group seen as metadata arrives with its bytes.
	let mut full = chain.subscribe(from_start(false)).await;
	settle().await;
	assert!(!asked(&track).await.meta, "promotion reached the publisher");
	assert_eq!(frames(&mut full, 0).await, vec![big(0, 0), big(0, 1)]);

	open.write_frame(open.at_start(), big(1, 1)).unwrap();
	open.close().unwrap();
	assert_eq!(frames(&mut full, 1).await, vec![big(1, 0), big(1, 1)]);
}

#[kio::test]
async fn headers_keep_flowing_after_the_last_payload_reader_leaves() {
	let chain = Chain::new(LITE07.parse().unwrap()).await;
	let (_route, track) = chain.publish();
	let mut meta = chain.subscribe(from_start(true)).await;
	let mut full = chain.subscribe(from_start(false)).await;
	subscribed(&track).await;

	write_group(&track, 0, 1);
	assert_eq!(frames(&mut full, 0).await, vec![big(0, 0)]);
	drop(full);
	settle().await;
	assert!(asked(&track).await.meta, "the relay kept asking for the payload");

	// The metadata reader still gets every group, the newer ones without the payload.
	assert_eq!(recv_key(&mut meta, 0).await.start, 0);
	write_group(&track, 1, 2);
	let infos = infos(&mut meta, 1).await;
	assert_eq!(infos.len(), 2);
}

#[kio::test]
async fn older_versions_refuse_metadata_only() {
	for version in ["moq-lite-06", "moq-transport-18"] {
		let chain = Chain::new(version.parse().unwrap()).await;
		let (_route, _track) = chain.publish();
		let remote = kio::clock::timeout(TIMEOUT, chain.subscriber.consume().follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves")
			.unwrap();
		let res = kio::clock::timeout(TIMEOUT, async {
			let mut sub = remote.track("data").unwrap().subscribe(from_start(true)).await?;
			sub.recv_group().await.map(|_| ())
		})
		.await
		.unwrap_or_else(|_| panic!("{version}: neither refused nor served"));
		assert!(
			matches!(res, Err(Error::Unsupported)),
			"{version}: expected the mode refused, got {res:?}"
		);
	}
}

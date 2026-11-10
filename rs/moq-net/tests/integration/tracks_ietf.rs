#![cfg(feature = "async")]

//! We never list a moq-transport peer's tracks: a TRACKS listing over that session fails
//! as unsupported without sending SUBSCRIBE_TRACKS, and the session and its SUBSCRIBEs carry on.
//!
//! Deterministic: paused time and a single-threaded runtime over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::Version;
use moq_net::broadcast::TracksEvent;
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every supported moq-transport draft.
const DRAFTS: [&str; 3] = ["moq-transport-18", "moq-transport-20", "moq-transport-22"];

/// SUBSCRIBE (0x03), as the first byte of a request stream.
const SUBSCRIBE: u8 = 0x03;

/// SUBSCRIBE_TRACKS (0x51), as the first byte of a request stream.
const SUBSCRIBE_TRACKS: u8 = 0x51;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) =
		moq_net::origin::Producer::new(moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

/// The next event as `+name`, `-name`, `synced`, `end`, or `error: ..`.
async fn next(tracks: &mut moq_net::broadcast::Tracks) -> String {
	match kio::clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
	{
		Ok(Some(TracksEvent::Added { name, .. })) => format!("+{name}"),
		Ok(Some(TracksEvent::Removed { name })) => format!("-{name}"),
		Ok(Some(TracksEvent::Finished { name })) => format!("~{name}"),
		Ok(Some(TracksEvent::Synced)) => "synced".into(),
		Ok(None) => "end".into(),
		Err(err) => format!("error: {err}"),
	}
}

/// `publisher` served to a fresh origin over one session; returns that origin's copy.
async fn connect(publisher: &moq_net::origin::Producer, version: &str) -> (moq_net::origin::Producer, MockPair) {
	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;
	(subscriber, pair)
}

async fn resolve(origin: &moq_net::origin::Producer, path: &str) -> moq_net::broadcast::Consumer {
	kio::clock::timeout(TIMEOUT, origin.consume().follow(path, None).unwrap().available())
		.await
		.expect("announce timeout")
		.expect("routed")
		.unwrap()
}

/// How many request streams of `kind` the client has opened.
fn opened(pair: &MockPair, kind: u8) -> usize {
	pair.client_transport
		.bidi_types()
		.iter()
		.filter(|first| **first == kind)
		.count()
}

/// How many SUBSCRIBE request streams the client has opened.
fn subscribes(pair: &MockPair) -> usize {
	opened(pair, SUBSCRIBE)
}

/// Read one frame of the next group, or `None` once the subscription ends.
async fn read(sub: &mut moq_net::track::Subscriber) -> Option<(u64, Vec<u8>)> {
	let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timeout")
		.ok()??;
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.ok()??;
	Some((group.sequence, frame.payload.to_vec()))
}

/// A listing fails as unsupported rather than claim the peer lists nothing, sends no
/// SUBSCRIBE_TRACKS, and leaves the SUBSCRIBE delivering.
#[kio::test]
async fn a_listing_fails_alone() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let video = broadcast.broadcast().create_track("video", None).unwrap();
		broadcast.announce().unwrap();

		let (subscriber, pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut sub = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		kio::clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never subscribed")
			.unwrap();
		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"before"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((0, b"before".to_vec())), "{version}");

		let mut tracks = remote.tracks().unwrap();
		assert_eq!(
			next(&mut tracks).await,
			format!("error: {}", moq_net::Error::Unsupported),
			"{version}"
		);
		assert_eq!(opened(&pair, SUBSCRIBE_TRACKS), 0, "{version}");

		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"after"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((1, b"after".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 1, "{version}");
	}
}

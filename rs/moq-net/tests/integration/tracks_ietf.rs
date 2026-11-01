#![cfg(feature = "async")]

//! A TRACKS listing over a moq-transport session (draft-18+) sends SUBSCRIBE_TRACKS with
//! FORWARD=0, only on demand. Our own publisher refuses it, so a listing of our peer fails
//! alone: the session and its SUBSCRIBEs carry on. The listing's handling of a peer that
//! answers is covered against a scripted peer in `ietf::subscriber::tracks`.
//!
//! Deterministic: paused time and a single-threaded runtime over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::{Hop, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every draft that has SUBSCRIBE_TRACKS.
const DRAFTS: [&str; 3] = ["moq-transport-18", "moq-transport-20", "moq-transport-22"];

/// SUBSCRIBE (0x03), as the first byte of a request stream.
const SUBSCRIBE: u8 = 0x03;

/// SUBSCRIBE_TRACKS (0x51), as the first byte of a request stream.
const SUBSCRIBE_TRACKS: u8 = 0x51;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
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

/// SUBSCRIBE_TRACKS goes out only for a listing, since many peers do not answer it:
/// connecting, an announce, and a reader's SUBSCRIBE send none. Our own publisher refuses
/// it, which fails only that listing, and the SUBSCRIBE keeps delivering.
#[kio::test]
async fn a_listing_of_our_own_peer_fails_alone() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let video = broadcast.create_track("video", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

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
		assert_eq!(opened(&pair, SUBSCRIBE_TRACKS), 0, "{version}: listed unasked");

		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		let failed = next(&mut tracks).await;
		assert!(failed.starts_with("error:"), "{version}: {failed}");
		assert_eq!(opened(&pair, SUBSCRIBE_TRACKS), 1, "{version}");

		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"after"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((1, b"after".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 1, "{version}");
	}
}

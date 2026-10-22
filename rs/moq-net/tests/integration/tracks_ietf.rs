#![cfg(feature = "async")]

//! A TRACKS listing over a moq-transport session (draft-18+): the subscriber sends
//! SUBSCRIBE_TRACKS with FORWARD=0 and holds each PUBLISH paused as its track's entry, which
//! a reader resumes for data instead of subscribing again.
//!
//! Deterministic: paused time and a single-threaded runtime over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::{Hop, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every draft that has SUBSCRIBE_TRACKS.
const DRAFTS: [&str; 5] = [
	"moq-transport-18",
	"moq-transport-19",
	"moq-transport-20",
	"moq-transport-21",
	"moq-transport-22",
];

/// SUBSCRIBE (0x03), as the first byte of a request stream.
const SUBSCRIBE: u8 = 0x03;

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The next event as `+name`, `-name`, `synced`, `end`, or `error: ..`.
async fn next(tracks: &mut moq_net::broadcast::Tracks) -> String {
	match moq_clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
	{
		Ok(Some(TracksEvent::Added { name, .. })) => format!("+{name}"),
		Ok(Some(TracksEvent::Removed { name })) => format!("-{name}"),
		Ok(Some(TracksEvent::Synced)) => "synced".into(),
		Ok(None) => "end".into(),
		Err(err) => format!("error: {err}"),
	}
}

/// Whether the listing has nothing more to report for a while.
async fn quiet(tracks: &mut moq_net::broadcast::Tracks) -> bool {
	moq_clock::timeout(Duration::from_millis(500), tracks.next())
		.await
		.is_err()
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
	moq_clock::timeout(TIMEOUT, origin.consume().routed_broadcast(path))
		.await
		.expect("announce timeout")
		.expect("routed")
}

/// How many SUBSCRIBE request streams the client has opened.
fn subscribes(pair: &MockPair) -> usize {
	pair.client_transport
		.bidi_types()
		.iter()
		.filter(|first| **first == SUBSCRIBE)
		.count()
}

/// Read one frame of the next group, or `None` once the subscription ends.
async fn read(sub: &mut moq_net::track::Subscriber) -> Option<(u64, Vec<u8>)> {
	let mut group = moq_clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timeout")
		.ok()??;
	let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.ok()??;
	Some((group.sequence, frame.payload.to_vec()))
}

/// The listing syncs at once, then lists the peer's tracks and their changes: a track the
/// peer stops listing is removed, as its PUBLISH ends.
#[moq_clock::test]
async fn an_ietf_session_lists_its_peers_tracks() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let a = broadcast.create_track("a", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (subscriber, _pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		assert_eq!(next(&mut tracks).await, "+a", "{version}");

		let _b = broadcast.create_track("b", None).unwrap();
		assert_eq!(next(&mut tracks).await, "+b", "{version}");
		drop(a);
		assert_eq!(next(&mut tracks).await, "-a", "{version}");
		assert!(quiet(&mut tracks).await, "{version}");
	}
}

/// A listed track carries the info its PUBLISH declared.
#[moq_clock::test]
async fn a_listing_carries_track_info() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_priority(9);
	let _a = broadcast.create_track("a", info).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, "moq-transport-22").await;
	let mut tracks = resolve(&subscriber, "bcast").await.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "synced");
	let event = moq_clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
		.unwrap();
	let Some(TracksEvent::Added { name, info }) = event else {
		panic!("expected an add, got {event:?}");
	};
	assert_eq!(&*name, "a");
	assert_eq!(info.timescale, Some(moq_net::Timescale::MILLI));
	assert_eq!(info.priority, 9);
}

/// Before draft-18 there is no SUBSCRIBE_TRACKS, so the listing is refused rather than
/// answered empty.
#[moq_clock::test]
async fn before_draft_18_the_listing_is_refused() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let _a = broadcast.create_track("a", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, "moq-transport-17").await;
	let mut tracks = resolve(&subscriber, "bcast").await.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, format!("error: {}", moq_net::Error::Unsupported));
}

/// A reader of a listed track resumes its held PUBLISH, so its data arrives with no
/// SUBSCRIBE at all; draft-18 would refuse one as a second subscription to the track.
/// Once the reader leaves, the PUBLISH pauses and stays listed, and the next reader
/// resumes it again.
#[moq_clock::test]
async fn a_reader_resumes_the_held_publish() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let video = broadcast.create_track("video", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (subscriber, pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		assert_eq!(next(&mut tracks).await, "+video", "{version}");

		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never resumed")
			.unwrap();
		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"first"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((0, b"first".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 0, "{version}: subscribed beside the held PUBLISH");

		// The last reader leaving pauses the PUBLISH upstream, and the track stays listed.
		drop(sub);
		moq_clock::timeout(TIMEOUT, support::unwanted(video.demand()))
			.await
			.expect("never paused")
			.unwrap();
		assert!(quiet(&mut tracks).await, "{version}");

		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never resumed again")
			.unwrap();
		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"second"[..]).unwrap();
		group.finish().unwrap();
		let got = loop {
			match read(&mut sub).await {
				Some((sequence, payload)) if sequence < 1 => continue,
				got => break got,
			}
		};
		assert_eq!(got, Some((1, b"second".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 0, "{version}");
	}
}

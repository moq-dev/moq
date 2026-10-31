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
	match moq_clock::timeout(TIMEOUT, tracks.next())
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
	moq_clock::timeout(TIMEOUT, origin.consume().follow(path, None).unwrap().available())
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

/// The listing syncs at once, then lists the peer's tracks as they appear.
#[moq_clock::test]
async fn an_ietf_session_lists_its_peers_tracks() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let _a = broadcast.create_track("a", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (subscriber, _pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		assert_eq!(next(&mut tracks).await, "+a", "{version}");

		let _b = broadcast.create_track("b", None).unwrap();
		assert_eq!(next(&mut tracks).await, "+b", "{version}");
		assert!(quiet(&mut tracks).await, "{version}");
	}
}

/// SUBSCRIBE_TRACKS goes out only for a listing, since many peers do not answer it yet:
/// connecting, an announce, and a reader's SUBSCRIBE send none.
#[moq_clock::test]
async fn only_a_listing_sends_subscribe_tracks() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let video = broadcast.create_track("video", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (subscriber, pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never subscribed")
			.unwrap();
		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"frame"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((0, b"frame".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 1, "{version}");
		assert_eq!(opened(&pair, SUBSCRIBE_TRACKS), 0, "{version}: listed unasked");

		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		assert_eq!(next(&mut tracks).await, "+video", "{version}");
		assert_eq!(opened(&pair, SUBSCRIBE_TRACKS), 1, "{version}");
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
				Some((sequence, _)) if sequence < 1 => continue,
				got => break got,
			}
		};
		assert_eq!(got, Some((1, b"second".to_vec())), "{version}");
		assert_eq!(subscribes(&pair), 0, "{version}");
	}
}

/// A reader joining a listed track mid-group gets the objects already published in it: on
/// draft 18, whose filters deliver nothing already published, through a Joining
/// FETCH on the held PUBLISH's Request ID, and from draft-22 through the resume's fill.
#[moq_clock::test]
async fn a_join_gets_the_current_groups_head() {
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

		let mut group = video.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
		group.write_frame(None, &b"head"[..]).unwrap();

		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never resumed")
			.unwrap();
		moq_clock::sleep(Duration::from_millis(10)).await;
		group.write_frame(None, &b"tail"[..]).unwrap();
		group.finish().unwrap();

		let mut joined = moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("the joined group");
		assert_eq!(joined.sequence, 5, "{version}");
		let mut frames = Vec::new();
		while let Some(frame) = moq_clock::timeout(TIMEOUT, joined.read_frame())
			.await
			.expect("frame timeout")
			.unwrap()
		{
			frames.push(frame.payload.to_vec());
		}
		assert_eq!(frames, [b"head".to_vec(), b"tail".to_vec()], "{version}");
		assert_eq!(subscribes(&pair), 0, "{version}");
	}
}

/// A nested listing reuses the broader SUBSCRIBE_TRACKS and filters locally: the peer
/// would refuse an overlapping prefix.
#[moq_clock::test]
async fn a_nested_listing_reuses_the_broader_one() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let outer = publisher.create_broadcast("a").unwrap();
		let _x = outer.create_track("x", None).unwrap();
		outer.announce(Default::default()).unwrap();
		let inner = publisher.create_broadcast("a/b").unwrap();
		let _y = inner.create_track("y", None).unwrap();
		inner.announce(Default::default()).unwrap();

		let (subscriber, _pair) = connect(&publisher, version).await;
		let mut outer_tracks = resolve(&subscriber, "a").await.tracks().unwrap();
		assert_eq!(next(&mut outer_tracks).await, "synced", "{version}");
		assert_eq!(next(&mut outer_tracks).await, "+x", "{version}");
		// The broader request holds `a/b`'s tracks already, so they list before syncing.
		let mut inner_tracks = resolve(&subscriber, "a/b").await.tracks().unwrap();
		assert_eq!(next(&mut inner_tracks).await, "+y", "{version}");
		assert_eq!(next(&mut inner_tracks).await, "synced", "{version}");

		let _z = inner.create_track("z", None).unwrap();
		assert_eq!(next(&mut inner_tracks).await, "+z", "{version}");
		assert!(
			quiet(&mut outer_tracks).await,
			"{version}: a nested track listed in its parent"
		);
	}
}

/// A broader listing replaces the nested requests only once the peer has closed them, so it
/// is never refused as overlapping, and the nested listings keep what they listed.
#[moq_clock::test]
async fn a_broader_listing_replaces_the_nested_ones() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let mut held = Vec::new();
		let mut broadcasts = Vec::new();
		for path in ["a/b", "a/c", "a"] {
			let broadcast = publisher.create_broadcast(path).unwrap();
			held.push(broadcast.create_track("t", None).unwrap());
			broadcast.announce(Default::default()).unwrap();
			broadcasts.push(broadcast);
		}

		let (subscriber, _pair) = connect(&publisher, version).await;
		let mut nested = Vec::new();
		for path in ["a/b", "a/c"] {
			let mut tracks = resolve(&subscriber, path).await.tracks().unwrap();
			assert_eq!(next(&mut tracks).await, "synced", "{version}");
			assert_eq!(next(&mut tracks).await, "+t", "{version}: {path}");
			nested.push(tracks);
		}

		let mut outer = resolve(&subscriber, "a").await.tracks().unwrap();
		assert_eq!(next(&mut outer).await, "synced", "{version}");
		assert_eq!(next(&mut outer).await, "+t", "{version}");
		for tracks in &mut nested {
			assert!(quiet(tracks).await, "{version}: a nested listing changed");
		}

		// The broader request lists what follows in the nested broadcasts too.
		held.push(broadcasts[0].create_track("u", None).unwrap());
		assert_eq!(next(&mut nested[0]).await, "+u", "{version}");
	}
}

/// A PUBLISH for a track we already SUBSCRIBEd is redundant: the SUBSCRIBE lists the track
/// and keeps carrying its data. Draft-18 skips it instead, which lists it the same way.
#[moq_clock::test]
async fn a_subscribed_track_is_listed_through_its_subscribe() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let video = broadcast.create_track("video", None).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let (subscriber, pair) = connect(&publisher, version).await;
		let remote = resolve(&subscriber, "bcast").await;
		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		moq_clock::timeout(TIMEOUT, support::wanted(video.demand()))
			.await
			.expect("never subscribed")
			.unwrap();
		assert_eq!(subscribes(&pair), 1, "{version}");

		let mut tracks = remote.tracks().unwrap();
		assert_eq!(next(&mut tracks).await, "synced", "{version}");
		assert_eq!(next(&mut tracks).await, "+video", "{version}");

		let mut group = video.append_group().unwrap();
		group.write_frame(None, &b"live"[..]).unwrap();
		group.finish().unwrap();
		assert_eq!(read(&mut sub).await, Some((0, b"live".to_vec())), "{version}");
		assert!(quiet(&mut tracks).await, "{version}");
		assert_eq!(subscribes(&pair), 1, "{version}");
	}
}

/// Each held PUBLISH counts against the session's `announces` cap, as a lite listed track
/// does: past it, the PUBLISH is refused and the listing reset rather than left short, and
/// the session stays.
#[moq_clock::test]
async fn a_listing_past_the_announces_cap_is_reset() {
	for version in DRAFTS {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		const CAP: usize = 10;
		let _tracks: Vec<_> = (0..CAP)
			.map(|i| broadcast.create_track(format!("t{i}"), None).unwrap())
			.collect();
		broadcast.announce(Default::default()).unwrap();

		let mut limits = moq_net::session::Limits::default();
		limits.announces = CAP;
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		options.server_limits = Some(limits);
		let pair = connect_mock(options).await;

		let remote = resolve(&subscriber, "bcast").await;
		let mut listing = remote.tracks().unwrap();
		assert_eq!(next(&mut listing).await, "synced", "{version}");
		// The announcement holds a slot too, so not every track fits.
		let mut added = 0;
		let end = loop {
			match next(&mut listing).await {
				event if event.starts_with('+') => added += 1,
				event => break event,
			}
		};
		assert!(added < CAP, "{version}: listed {added} tracks under a cap of {CAP}");
		assert_eq!(end, format!("error: {}", moq_net::Error::TooManyRequests), "{version}");

		assert!(
			moq_clock::timeout(Duration::from_millis(100), pair.server.closed())
				.await
				.is_err(),
			"{version}: the session closed"
		);
	}
}

/// Only TRACK_ENDED removes a listed track, and our answerer sends it only for a track that
/// finished: that one leaves the listing quietly. One that left unfinished may go on
/// elsewhere, so its PUBLISH ends in error, which fails the listing loudly, and a reader
/// listing again sees what is still listed.
#[moq_clock::test]
async fn a_track_leaves_the_listing_as_it_ended() {
	for finish in [true, false] {
		for version in DRAFTS {
			let publisher = produce_origin(1);
			let broadcast = publisher.create_broadcast("bcast").unwrap();
			let a = broadcast.create_track("a", None).unwrap();
			let _b = broadcast.create_track("b", None).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let (subscriber, _pair) = connect(&publisher, version).await;
			let remote = resolve(&subscriber, "bcast").await;
			let mut tracks = remote.tracks().unwrap();
			assert_eq!(next(&mut tracks).await, "synced", "{version}");
			let mut listed = vec![next(&mut tracks).await, next(&mut tracks).await];
			listed.sort();
			assert_eq!(listed, ["+a", "+b"], "{version}");

			if finish {
				a.finish().unwrap();
				drop(a);
				assert_eq!(next(&mut tracks).await, "~a", "{version}");
				assert!(quiet(&mut tracks).await, "{version}: the listing stands");
				continue;
			}
			let _ = a.abort(moq_net::Error::Cancel);
			let failed = next(&mut tracks).await;
			assert!(failed.starts_with("error:"), "{version}: {failed}");

			let mut again = remote.tracks().unwrap();
			assert_eq!(next(&mut again).await, "synced", "{version}");
			assert_eq!(next(&mut again).await, "+b", "{version}");
			assert!(quiet(&mut again).await, "{version}");
		}
	}
}

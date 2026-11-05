#![cfg(feature = "async")]

//! A moq-lite-07 TRACKS listing over a session and through a relay: each hop syncs with
//! what it holds at once, and the upstream's tracks follow as live adds.

use crate::support;

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::{Hop, Version, session::Limits};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
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
async fn connect(
	publisher: &moq_net::origin::Producer,
	hop: u64,
	version: Version,
) -> (moq_net::origin::Producer, MockPair) {
	let subscriber = produce_origin(hop);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;
	(subscriber, pair)
}

async fn resolve(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	kio::clock::timeout(TIMEOUT, origin.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("announce timeout")
		.expect("routed")
}

/// A session syncs at once, then lists the peer's tracks and their changes until the
/// broadcast ends.
#[kio::test]
async fn a_session_lists_its_peers_tracks() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let a = broadcast.create_track("a", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let remote = resolve(&subscriber).await;
	let mut tracks = remote.tracks().unwrap();
	assert_eq!(
		next(&mut tracks).await,
		"synced",
		"synced with this hop, which holds nothing yet"
	);
	assert_eq!(next(&mut tracks).await, "+a");

	let b = broadcast.create_track("b", None).unwrap();
	assert_eq!(next(&mut tracks).await, "+b");
	drop(a);
	assert_eq!(next(&mut tracks).await, "-a");
	b.finish().unwrap();
	drop(b);
	assert_eq!(next(&mut tracks).await, "~b", "TRACKS_FINISH, not TRACKS_REMOVE");

	broadcast.close();
	assert_eq!(next(&mut tracks).await, "end");
}

/// TRACKS_ADD carries the track's whole TRACK_INFO, Refresh included.
#[kio::test]
async fn a_listing_carries_track_info() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(Duration::from_secs(3))
		.with_priority(9)
		.with_refresh(4)
		.with_compress(true);
	let _a = broadcast.create_track("a", info).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let mut tracks = resolve(&subscriber).await.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "synced");
	let event = kio::clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
		.unwrap();
	let Some(TracksEvent::Added { name, info }) = event else {
		panic!("expected an add, got {event:?}");
	};
	assert_eq!(&*name, "a");
	assert_eq!(info.timescale, Some(moq_net::Timescale::MILLI));
	assert_eq!(info.max_age, Some(Duration::from_secs(3)));
	assert_eq!(info.priority, 9);
	assert_eq!(info.refresh, Some(4));
	assert!(info.compress);
}

/// A listed track is a track like any other: a SUBSCRIBE from the listing reads the
/// group it already holds from the cache.
#[kio::test]
async fn a_listed_track_serves_its_cached_group() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let remote = resolve(&subscriber).await;
	let mut tracks = remote.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "synced");

	let late = broadcast.create_track("late", None).unwrap();
	let mut group = late.append_group().unwrap();
	group.write_frame(None, &b"first"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next(&mut tracks).await, "+late");

	let mut sub = kio::clock::timeout(TIMEOUT, remote.track("late").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("the group");
	assert_eq!(group.sequence, 0);
	assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"first");
}

/// Through a relay: the relay answers each downstream listing at once from the one
/// upstream listing it keeps, a cold relay's upstream tracks follow as live adds, and a
/// removal reaches each downstream as its upstream reported it.
#[kio::test]
async fn a_relay_shares_its_upstream_listing() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let a = broadcast.create_track("a", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (relay, _upstream) = connect(&publisher, 2, lite07()).await;
	let (first, _first) = connect(&relay, 3, lite07()).await;
	let (second, _second) = connect(&relay, 4, lite07()).await;

	let mut one = resolve(&first).await.tracks().unwrap();
	assert_eq!(next(&mut one).await, "synced");
	assert_eq!(next(&mut one).await, "+a");

	// Warm now: the relay lists what it holds before syncing.
	let mut two = resolve(&second).await.tracks().unwrap();
	assert_eq!(next(&mut two).await, "synced");
	assert_eq!(next(&mut two).await, "+a");

	let b = broadcast.create_track("b", None).unwrap();
	assert_eq!(next(&mut one).await, "+b");
	assert_eq!(next(&mut two).await, "+b");

	a.finish().unwrap();
	drop(a);
	assert_eq!(next(&mut one).await, "~a");
	assert_eq!(next(&mut two).await, "~a");
	drop(b);
	assert_eq!(next(&mut one).await, "-b");
	assert_eq!(next(&mut two).await, "-b");
}

/// A version without the Tracks Stream refuses the listing rather than answer it empty.
#[kio::test]
async fn lite06_cannot_list() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let _a = broadcast.create_track("a", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, "moq-lite-06".parse().unwrap()).await;
	let mut tracks = resolve(&subscriber).await.tracks().unwrap();
	assert_eq!(
		next(&mut tracks).await,
		format!("error: {}", moq_net::Error::Unsupported)
	);
}

/// Each listed track counts against the session's `announces` cap: past it, the listing
/// is reset rather than held, and the session stays.
#[kio::test]
async fn a_listing_past_the_announces_cap_is_reset() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	const CAP: usize = 10;
	let _tracks: Vec<_> = (0..CAP)
		.map(|i| broadcast.create_track(format!("t{i}"), None).unwrap())
		.collect();
	broadcast.announce(Default::default()).unwrap();

	let mut limits = Limits::default();
	limits.announces = CAP;
	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(lite07());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	options.server_limits = Some(limits);
	let pair = connect_mock(options).await;

	let remote = resolve(&subscriber).await;
	let mut listing = remote.tracks().unwrap();
	assert_eq!(next(&mut listing).await, "synced");
	// The announcement holds a slot too, so not every track fits.
	let mut added = 0;
	let end = loop {
		match next(&mut listing).await {
			event if event.starts_with('+') => added += 1,
			event => break event,
		}
	};
	assert!(added < CAP, "listed {added} tracks under a cap of {CAP}");
	assert_eq!(end, format!("error: {}", moq_net::Error::TooManyRequests));

	assert!(
		kio::clock::timeout(Duration::from_millis(100), pair.server.closed())
			.await
			.is_err(),
		"the session closed"
	);
}

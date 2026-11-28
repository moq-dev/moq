#![cfg(feature = "async")]

//! A moq-lite-07 TRACKS listing over a session and through a relay: each hop lists the
//! upstream's tracks as they arrive and syncs when the upstream does, so a track listed
//! after the sync is new at the source.

use crate::support;
use support::AppendNext;

use std::time::Duration;

use moq_net::broadcast::{Kind, Listed, TracksEvent};
use moq_net::{Version, session::Limits};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// The next event as `+name`, `-name`, `~name` (finished), `synced`, `end`, or `error: ..`, a
/// datagram track's name prefixed `d:`.
async fn next(tracks: &mut moq_net::broadcast::Tracks) -> String {
	let named = |kind: Kind, name: &str| match kind {
		Kind::Track => name.to_string(),
		Kind::Datagrams => format!("d:{name}"),
		kind => panic!("unexpected kind {kind:?}"),
	};
	match kio::clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
	{
		Ok(Some(TracksEvent::Added { name, info, .. })) => format!("+{}", named(info.kind(), &name)),
		Ok(Some(TracksEvent::Removed { kind, name, .. })) => format!("-{}", named(kind, &name)),
		Ok(Some(TracksEvent::Finished { kind, name, .. })) => format!("~{}", named(kind, &name)),
		Ok(Some(TracksEvent::Synced)) => "synced".into(),
		Ok(Some(event)) => panic!("unexpected event {event:?}"),
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
		.unwrap()
}

/// A session lists the peer's tracks, syncs when the peer does, then lists their
/// changes until the broadcast ends.
#[kio::test]
async fn a_session_lists_its_peers_tracks() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let a = broadcast.broadcast().create_track("a", None).unwrap();
	broadcast.announce().unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let remote = resolve(&subscriber).await;
	let mut tracks = remote.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "+a");
	assert_eq!(next(&mut tracks).await, "synced", "synced with the peer");

	let b = broadcast.broadcast().create_track("b", None).unwrap();
	assert_eq!(next(&mut tracks).await, "+b");
	drop(a);
	assert_eq!(next(&mut tracks).await, "-a");
	let b = b.finish().unwrap();
	drop(b);
	assert_eq!(next(&mut tracks).await, "~b", "TRACKS_FINISH, not TRACKS_REMOVE");

	broadcast.broadcast().close();
	assert_eq!(next(&mut tracks).await, "end");
}

/// TRACKS_ADD carries the track's whole TRACK_INFO, Refresh included.
#[kio::test]
async fn a_listing_carries_track_info() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(Duration::from_secs(3))
		.with_priority(9)
		.with_refresh(moq_net::track::Refresh::default().with_ratio(4))
		.with_compression(moq_net::track::Compression::Deflate);
	let _a = broadcast.broadcast().create_track("a", info).unwrap();
	broadcast.announce().unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let mut tracks = resolve(&subscriber).await.tracks().unwrap();
	let event = kio::clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
		.unwrap();
	let Some(TracksEvent::Added {
		name,
		info: Listed::Track(info),
		..
	}) = event
	else {
		panic!("expected an add, got {event:?}");
	};
	assert_eq!(&*name, "a");
	assert_eq!(info.timescale, Some(moq_net::Timescale::MILLI));
	assert_eq!(info.max_age, Some(Duration::from_secs(3)));
	assert_eq!(info.priority, 9);
	assert_eq!(info.refresh, moq_net::track::Refresh::default().with_ratio(4));
	assert_eq!(info.compression, moq_net::track::Compression::Deflate);
}

/// A datagram track is listed apart from the track of its name, with its DATAGRAMS_INFO, and
/// the listed name reads its datagrams.
#[kio::test]
async fn a_session_lists_its_peers_datagram_tracks() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast.broadcast().create_track("a", None).unwrap();
	let mut datagrams = broadcast
		.broadcast()
		.create_datagrams(
			"a",
			moq_net::datagram::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_priority(-2),
		)
		.unwrap();
	broadcast.announce().unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let remote = resolve(&subscriber).await;
	let mut tracks = remote.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "+a");
	let event = kio::clock::timeout(TIMEOUT, tracks.next())
		.await
		.expect("listing stalled")
		.unwrap();
	let Some(TracksEvent::Added {
		name,
		info: Listed::Datagrams(info),
		..
	}) = event
	else {
		panic!("expected the datagram track, got {event:?}");
	};
	assert_eq!(&*name, "a");
	assert_eq!(info.timescale, Some(moq_net::Timescale::MILLI));
	assert_eq!(info.priority, -2);
	assert_eq!(next(&mut tracks).await, "synced");

	let mut reader = remote.datagrams(&name).unwrap();
	kio::clock::timeout(TIMEOUT, reader.info()).await.unwrap().unwrap();
	// Sent until one arrives: a datagram is best-effort.
	let received = kio::clock::timeout(TIMEOUT, async {
		loop {
			datagrams
				.append(moq_net::Timestamp::from_millis(0).unwrap(), b"x".as_ref())
				.unwrap();
			if let Some(Ok(datagram)) = futures::FutureExt::now_or_never(reader.recv()) {
				break datagram;
			}
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("no datagram arrived");
	assert_eq!(received.expect("a datagram").payload, &b"x"[..]);

	datagrams.finish();
	assert_eq!(next(&mut tracks).await, "~d:a", "TRACKS_FINISH of the datagram track");
	drop(track);
	assert_eq!(next(&mut tracks).await, "-a");
}

/// A listed track is a track like any other: a SUBSCRIBE from the listing reads the
/// group it already holds from the cache.
#[kio::test]
async fn a_listed_track_serves_its_cached_group() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	broadcast.announce().unwrap();

	let (subscriber, _pair) = connect(&publisher, 2, lite07()).await;
	let remote = resolve(&subscriber).await;
	let mut tracks = remote.tracks().unwrap();
	assert_eq!(next(&mut tracks).await, "synced");

	let late = broadcast.broadcast().create_track("late", None).unwrap();
	let mut group = late.append_next().unwrap();
	group
		.write_frame(moq_net::Timestamp::unscaled(group.start), &b"first"[..])
		.unwrap();
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
	assert_eq!(group.start, 0);
	assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"first");
}

/// Through a relay: the relay answers each downstream listing from the one upstream
/// listing it keeps, a cold relay syncs only once its upstream has, and a removal
/// reaches each downstream as its upstream reported it.
#[kio::test]
async fn a_relay_shares_its_upstream_listing() {
	let publisher = produce_origin(1);
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let a = broadcast.broadcast().create_track("a", None).unwrap();
	broadcast.announce().unwrap();

	let (relay, _upstream) = connect(&publisher, 2, lite07()).await;
	let (first, _first) = connect(&relay, 3, lite07()).await;
	let (second, _second) = connect(&relay, 4, lite07()).await;

	// Cold: the source's track comes before the sync, as it would in-process.
	let mut one = resolve(&first).await.tracks().unwrap();
	assert_eq!(next(&mut one).await, "+a");
	assert_eq!(next(&mut one).await, "synced");

	// Warm now: the relay lists what it holds, then syncs.
	let mut two = resolve(&second).await.tracks().unwrap();
	assert_eq!(next(&mut two).await, "+a");
	assert_eq!(next(&mut two).await, "synced");

	let b = broadcast.broadcast().create_track("b", None).unwrap();
	assert_eq!(next(&mut one).await, "+b");
	assert_eq!(next(&mut two).await, "+b");

	let a = a.finish().unwrap();
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
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let _a = broadcast.broadcast().create_track("a", None).unwrap();
	broadcast.announce().unwrap();

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
	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	const CAP: usize = 10;
	let _tracks: Vec<_> = (0..CAP)
		.map(|i| broadcast.broadcast().create_track(format!("t{i}"), None).unwrap())
		.collect();
	broadcast.announce().unwrap();

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
	// The announcement holds a slot too, so not every track fits, and the peer never syncs.
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

/// A relay's listing follows its broadcast onto another route of the same identity when
/// the serving one dies: a track listed afterwards still reaches the downstream reader.
///
/// `P` is pulled by `A` directly and through `B`; `D` lists through `A`. `A`'s session
/// to `P` dies, so `A` moves to the route through `B`, and `P` then lists a new track.
#[kio::test]
async fn a_listing_follows_its_broadcast_onto_another_route() {
	use crate::route_change::{link, priced_link, settle};

	let publisher = produce_origin(1);
	let relay_a = produce_origin(2);
	let relay_b = produce_origin(3);
	let dash = produce_origin(4);

	let broadcast = publisher
		.route("bcast", moq_net::origin::RouteOptions::hidden())
		.unwrap();
	let _a = broadcast.broadcast().create_track("a", None).unwrap();
	broadcast.announce().unwrap();

	let p_to_a = link(lite07(), &publisher, &relay_a).await;
	let _p_to_b = link(lite07(), &publisher, &relay_b).await;
	let _b_to_a = priced_link(lite07(), &relay_b, &relay_a, Some(1_000)).await;
	let _a_to_d = link(lite07(), &relay_a, &dash).await;
	settle().await;

	let mut listing = resolve(&dash).await.tracks().unwrap();
	assert_eq!(next(&mut listing).await, "+a");
	assert_eq!(next(&mut listing).await, "synced");

	p_to_a.server.abort(moq_net::Error::Cancel);
	p_to_a.client.abort(moq_net::Error::Cancel);
	settle().await;

	let _b = broadcast.broadcast().create_track("b", None).unwrap();
	assert_eq!(next(&mut listing).await, "+b");
}

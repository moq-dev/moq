#![cfg(feature = "async")]

//! Lite-07 removes FETCH (bidi 0x3) and serves all single-group fetches, frame slices,
//! and misses via bounded SUBSCRIBE (one-shot SUBSCRIBE + immediate FIN, or deduplicated
//! onto an active SUBSCRIBE).

use crate::WithMax as _;
use crate::support;

use std::time::Duration;

use moq_net::group;
use moq_net::track::{Info, Range, Start, Subscription};
use moq_net::{Timescale, Timestamp, Version, cache};
use support::harness::{MockConnectOptions, connect_mock};
use support::mock::MockSession;

const TIMEOUT: Duration = Duration::from_secs(5);
const LITE07: &str = "moq-lite-07-wip";
const SUBSCRIBE: u8 = 2;
const FETCH: u8 = 3;

fn origin(hop: u64, pool: Option<cache::Pool>) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	if let Some(pool) = pool {
		config.pool = pool;
	}
	config.proxy = true;
	let (producer, driver) = moq_net::origin::Producer::new(config);
	support::harness::spawn(driver);
	producer
}

fn count_bidi(transport: &MockSession, kind: u8) -> usize {
	transport.bidi_types().into_iter().filter(|t| *t == kind).count()
}

/// Anchor::Position(G, 0) matches only the exact group G (including G == 0 without underflow),
/// never falling back to an earlier group whose span contains G; Anchor::Time still resolves
/// by time containment.
#[kio::test]
async fn position_zero_and_exact_group_match() {
	kio::clock::timeout(TIMEOUT, async {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track(
				"video",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
			)
			.unwrap();
		broadcast.announce().unwrap();

		// Group 0 covers [0, 1000); group 1000 covers [1000, 2000).
		let g0 = track
			.append_group(Timestamp::from_millis(0).unwrap(), b"g0".as_slice())
			.unwrap();
		g0.finish_at(1000).unwrap();

		let g1 = track
			.append_group(Timestamp::from_millis(1000).unwrap(), b"g1".as_slice())
			.unwrap();
		g1.finish_at(2000).unwrap();

		let viewer = origin(2, None);
		let mut options = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("bcast", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();

		// Position(0, 0)..Position(1, 0) delivers group 0 without underflow.
		let mut fetched0 = remote.track("video").unwrap().fetch_group(0, None).await.unwrap();
		let f0 = fetched0.read_frame().await.unwrap().expect("group 0 frame");
		assert_eq!(f0.payload.as_ref(), b"g0");
		assert!(fetched0.read_frame().await.unwrap().is_none());
		assert_eq!(fetched0.end(), Some(1000), "whole group carries GROUP_END");

		// Position(500, 0)..Position(501, 0) does NOT fall back to group 0 even though 500 is in [0, 1000).
		let miss = remote.track("video").unwrap().fetch_group(500, None).await;
		assert!(miss.is_err(), "exact Position(500, 0) must not fall back to group 0");

		// Time(500) DOES resolve by time containment to group 0.
		let mut sub = remote
			.track("video")
			.unwrap()
			.subscribe(
				Subscription::default()
					.with_ranges([Range {
						start: Start::Time(500),
						end: Some(500),
					}])
					.with_max(None),
			)
			.await
			.unwrap();
		let resolved = sub.recv_group().await.unwrap().expect("group 0 via Time(500)");
		assert_eq!(resolved.start, 0);

		assert_eq!(
			count_bidi(&pair.client_transport, FETCH),
			0,
			"lite-07 never sends FETCH"
		);
		assert!(count_bidi(&pair.client_transport, SUBSCRIBE) >= 2);
		drop((resolved, sub, fetched0, pair, broadcast, publisher, viewer));
	})
	.await
	.expect("timed out");
}

/// Standalone `fetch_group` on lite-07 uses one-shot bounded SUBSCRIBE
/// `[Position(G, 0), Position(G + 1, 0))` + immediate FIN (zero FETCH streams)
/// and delivers every frame through GROUP_END without creating a live subscription.
#[kio::test]
async fn standalone_fetch_uses_one_shot_subscribe_with_group_end() {
	kio::clock::timeout(TIMEOUT, async {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track(
				"video",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
			)
			.unwrap();
		broadcast.announce().unwrap();

		let mut g0 = track.create_group(Timestamp::ZERO, b"f0".as_slice()).unwrap();
		for i in 1u64..5 {
			g0.write_frame(Timestamp::from_millis(i * 100).unwrap(), format!("f{i}").into_bytes())
				.unwrap();
		}
		g0.finish_at(500).unwrap();

		let viewer = origin(2, None);
		let mut options = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("bcast", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();

		let mut opts = group::Fetch::default();
		opts.priority = 5;
		let mut fetched = remote.track("video").unwrap().fetch_group(0, opts).await.unwrap();
		let mut got = Vec::new();
		while let Some(frame) = fetched.read_frame().await.unwrap() {
			got.push(String::from_utf8(frame.payload.to_vec()).unwrap());
		}
		assert_eq!(got, vec!["f0", "f1", "f2", "f3", "f4"]);
		assert_eq!(fetched.end(), Some(500));
		assert!(
			track.subscription().is_none(),
			"one-shot fetch creates no live subscription"
		);
		assert_eq!(
			count_bidi(&pair.client_transport, FETCH),
			0,
			"lite-07 never sends FETCH"
		);
		assert_eq!(count_bidi(&pair.client_transport, SUBSCRIBE), 1);

		drop((fetched, pair, broadcast, publisher, viewer));
	})
	.await
	.expect("timed out");
}

/// Cancelling a pending one-shot SUBSCRIBE + immediate FIN sends STOP_SENDING and cancels
/// the publisher's fill request promptly.
#[kio::test]
async fn one_shot_subscribe_fin_cancel_stops_pending_fill() {
	kio::clock::timeout(TIMEOUT, async {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", Info::default().with_max_age(None))
			.unwrap();
		broadcast.announce().unwrap();
		let requests = track.requests();

		let viewer = origin(2, None);
		let mut options = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(viewer.clone());
		let pair = connect_mock(options).await;

		let remote = viewer
			.consume()
			.follow("bcast", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();

		let fetching =
			kio::clock::sim::spawn(async move { remote.track("video").unwrap().fetch_group(42, None).await });
		let fill = requests.requested_fill().await.expect("fill arrives");
		assert_eq!(fill.start().group, 42);
		assert_eq!(fill.end(), 43);

		// Dropping the caller sends STOP_SENDING on the one-shot SUBSCRIBE stream.
		let server_resets = pair.server_transport.resets().len();
		fetching.abort();
		kio::clock::sleep(Duration::from_millis(100)).await;
		assert!(
			pair.server_transport.resets().len() > server_resets,
			"STOP_SENDING cancels the publisher's one-shot SUBSCRIBE"
		);
		assert_eq!(
			count_bidi(&pair.client_transport, FETCH),
			0,
			"lite-07 never sends FETCH"
		);
		drop((fill, pair, track, broadcast, publisher, viewer));
	})
	.await
	.expect("timed out");
}

/// N-to-1 deduplication (D2): while a relay has an active live SUBSCRIBE covering group N,
/// a viewer calling `fetch_group(N)` shares that upstream SUBSCRIBE (at most 1 wire copy,
/// zero FETCH streams). Once evicted from the relay cache, `fetch_group(N)` uses a one-shot
/// bounded SUBSCRIBE + immediate FIN (still zero FETCH streams).
#[kio::test]
async fn relay_deduplicates_fetch_onto_active_subscribe_and_recovers_after_eviction() {
	kio::clock::timeout(cache::DEFAULT_EXPIRY * 3 + TIMEOUT, async {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track(
				"video",
				Info::default().with_timescale(Timescale::MILLI).with_max_age(None),
			)
			.unwrap();
		broadcast.announce().unwrap();

		for i in 0u64..3 {
			let g = track
				.create_group(Timestamp::from_millis(i * 1000).unwrap(), format!("g{i}").into_bytes())
				.unwrap();
			g.finish().unwrap();
		}

		// Relay cache expires idle groups; publisher holds everything forever.
		let relay = origin(2, None);
		let mut up_opts = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		up_opts.server_publish = Some(publisher.consume());
		up_opts.client_subscribe = Some(relay.clone());
		let upstream = connect_mock(up_opts).await;

		let viewer = origin(3, None);
		let mut down_opts = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		down_opts.server_publish = Some(relay.consume());
		down_opts.client_subscribe = Some(viewer.clone());
		let downstream = connect_mock(down_opts).await;

		let remote = kio::clock::timeout(TIMEOUT, viewer.consume().follow("bcast", None).unwrap().available())
			.await
			.expect("follow timeout")
			.unwrap()
			.unwrap();

		// Active live subscription on the relay covers groups 0..=2 and future groups.
		let mut live = kio::clock::timeout(
			TIMEOUT,
			remote
				.track("video")
				.unwrap()
				.subscribe(Subscription::default().with_ranges([Range::ALL]).with_max(None)),
		)
		.await
		.expect("subscribe timeout")
		.unwrap();
		for i in 0..3 {
			let _ = kio::clock::timeout(TIMEOUT, live.recv_group())
				.await
				.unwrap_or_else(|_| panic!("initial group {i} timeout"))
				.unwrap()
				.expect("initial group");
		}
		assert_eq!(count_bidi(&upstream.client_transport, SUBSCRIBE), 1);
		assert_eq!(count_bidi(&upstream.client_transport, FETCH), 0);

		// Group 3 starts streaming live (still open!) while the live SUBSCRIBE is active.
		// A concurrent fetch_group(3000) before g3 finishes deduplicates onto that active
		// SUBSCRIBE (no extra upstream SUBSCRIBE, no FETCH).
		let mut g3 = track
			.create_group(Timestamp::from_millis(3000).unwrap(), b"g3-f0".as_slice())
			.unwrap();

		let mut live3 = kio::clock::timeout(TIMEOUT, live.recv_group())
			.await
			.expect("live g3 timeout")
			.unwrap()
			.expect("live g3");
		assert_eq!(live3.start, 3000);
		let live3_f0 = kio::clock::timeout(TIMEOUT, live3.read_frame())
			.await
			.expect("live g3 f0 timeout")
			.unwrap()
			.expect("g3 f0");
		assert_eq!(live3_f0.payload.as_ref(), b"g3-f0");

		let mut fetched3 = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().fetch_group(3000, None))
			.await
			.expect("fetch_group(3000) timeout")
			.unwrap();
		g3.write_frame(Timestamp::from_millis(3100).unwrap(), b"g3-f1".as_slice())
			.unwrap();
		g3.finish().unwrap();

		let f3_0 = kio::clock::timeout(TIMEOUT, fetched3.read_frame())
			.await
			.expect("read f3_0 timeout")
			.unwrap()
			.expect("g3 f0");
		assert_eq!(f3_0.payload.as_ref(), b"g3-f0");
		let f3_1 = kio::clock::timeout(TIMEOUT, fetched3.read_frame())
			.await
			.expect("read f3_1 timeout")
			.unwrap()
			.expect("g3 f1");
		assert_eq!(f3_1.payload.as_ref(), b"g3-f1");
		assert_eq!(
			count_bidi(&upstream.client_transport, SUBSCRIBE),
			1,
			"in-flight live group fetch deduplicates onto the active upstream SUBSCRIBE"
		);
		assert_eq!(count_bidi(&upstream.client_transport, FETCH), 0);
		drop((fetched3, live3));

		// Advance past cache expiry so the relay evicts group 0 while the live SUBSCRIBE stays open.
		kio::clock::sleep(cache::DEFAULT_EXPIRY * 2 + Duration::from_secs(2)).await;

		// Fetching evicted group 0 now uses a one-shot bounded SUBSCRIBE + FIN (still 0 FETCH).
		let mut fetched0 = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().fetch_group(0, None))
			.await
			.expect("fetch_group(0) timeout")
			.unwrap();
		let f0 = kio::clock::timeout(TIMEOUT, fetched0.read_frame())
			.await
			.expect("read f0 timeout")
			.unwrap()
			.expect("g0 frame");
		assert_eq!(f0.payload.as_ref(), b"g0");
		assert_eq!(
			count_bidi(&upstream.client_transport, FETCH),
			0,
			"lite-07 never sends FETCH"
		);
		assert_eq!(
			count_bidi(&upstream.client_transport, SUBSCRIBE),
			2,
			"evicted group recovered via one-shot bounded SUBSCRIBE"
		);

		drop((
			fetched0, live, downstream, upstream, track, broadcast, publisher, relay, viewer,
		));
	})
	.await
	.expect("timed out");
}

/// Through a relay with a live subscription at group 66, fetching unproduced group 67 via
/// one-shot SUBSCRIBE reaches the publisher's fill handler (exact Position never falls back
/// to group 66 on the relay's resume copy).
#[kio::test]
async fn relay_exact_position_fetch_past_live_edge_reaches_fill_handler() {
	kio::clock::timeout(TIMEOUT, async {
		let publisher = origin(1, Some(cache::Pool::unbounded()));
		let broadcast = publisher
			.route("bcast", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = broadcast
			.broadcast()
			.create_track("value", Info::default().with_max_age(None))
			.unwrap();
		broadcast.announce().unwrap();
		let requests = track.requests();

		let g66 = track.create_group(Timestamp::unscaled(66), b"66".as_slice()).unwrap();
		g66.finish().unwrap();

		let relay = origin(2, Some(cache::Pool::unbounded()));
		let mut up_opts = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		up_opts.server_publish = Some(publisher.consume());
		up_opts.client_subscribe = Some(relay.clone());
		let upstream = connect_mock(up_opts).await;

		let viewer = origin(3, None);
		let mut down_opts = MockConnectOptions::new(LITE07.parse::<Version>().unwrap());
		down_opts.server_publish = Some(relay.consume());
		down_opts.client_subscribe = Some(viewer.clone());
		let downstream = connect_mock(down_opts).await;

		let remote = viewer
			.consume()
			.follow("bcast", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();

		let mut live = remote
			.track("value")
			.unwrap()
			.subscribe(
				Subscription::default()
					.with_ranges([Range {
						start: Start::Time(66),
						end: None,
					}])
					.with_max(None),
			)
			.await
			.unwrap();
		let g = live.recv_group().await.unwrap().expect("group 66");
		assert_eq!(g.start, 66);

		let serve = kio::clock::sim::spawn(async move {
			while let Ok(fill) = requests.requested_fill().await {
				if fill.start().group == 67 {
					assert_eq!(fill.end(), 68);
					if let Some(mut g67) = fill.accept(67, None).unwrap() {
						g67.write_frame(Timestamp::unscaled(67), b"67".as_slice()).unwrap();
						g67.finish().unwrap();
					}
					break;
				}
			}
		});

		let mut fetched = remote.track("value").unwrap().fetch_group(67, None).await.unwrap();
		assert_eq!(fetched.start, 67);
		let f = fetched.read_frame().await.unwrap().expect("frame 67");
		assert_eq!(f.payload.as_ref(), b"67");
		serve.await.unwrap();
		assert_eq!(count_bidi(&upstream.client_transport, FETCH), 0);
		assert_eq!(count_bidi(&downstream.client_transport, FETCH), 0);

		drop((
			fetched, g, live, downstream, upstream, track, broadcast, publisher, relay, viewer,
		));
	})
	.await
	.expect("timed out");
}

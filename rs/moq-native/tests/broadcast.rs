//! Integration test: verify that announcing a broadcast and subscribing to a
//! track works end-to-end for every supported protocol version.
//!
//! The server publishes a broadcast containing a track with known data.
//! The client connects, receives the announcement, subscribes to the track,
//! and verifies it receives the correct payload.
//!
//! This covers raw QUIC (moqt://) and WebTransport (https://) transports,
//! exercising every protocol version the library supports.

use moq_native::moq_net::{self, Origin};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Publish a broadcast on the server, subscribe on the client, and verify
/// the data arrives correctly for the given URL scheme and version configuration.
///
/// `client_version` and `server_version` can differ to test version negotiation.
/// `None` means "support all versions" (empty version vec).
async fn broadcast_test(scheme: &str, client_version: Option<&str>, server_version: Option<&str>) {
	let client_version: Option<moq_net::Version> = client_version.map(|v| v.parse().expect("invalid client version"));
	let server_version: Option<moq_net::Version> = server_version.map(|v| v.parse().expect("invalid server version"));

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");

	// Write a group containing a single frame.
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	if let Some(v) = server_version {
		server_config.version = vec![v];
	}

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	if let Some(v) = client_version {
		client_config.version = vec![v];
	}

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(&pub_origin).ok().await?;

		// Keep producers alive so the subscriber can read data.
		let _broadcast = broadcast;
		let _track = track;

		// Block until the client disconnects.
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("consume_track failed");

	// Read one group.
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");

	// Read one frame and verify the payload.
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");

	assert_eq!(&*frame, b"hello");

	// Tear down: dropping the session closes the QUIC connection.
	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// Lite05 publisher↔subscriber round-trip exercising the per-frame timestamp
/// delta encoding, including negative deltas (B-frame ordering).
async fn lite05_timestamp_roundtrip(scheme: &str) {
	use moq_native::moq_net::{Timescale, Timestamp};

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");

	// Track with an explicit microsecond timescale (the default is milliseconds).
	let mut track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(Timescale::MICRO),
		)
		.expect("failed to create track");

	// Three frames where the middle PTS goes backwards (B-frame decode order) so the
	// zigzag timestamp delta carries a negative value.
	let frames = [10_000u64, 30_000, 20_000];
	let mut group = track.append_group().expect("failed to append group");
	for &us in &frames {
		let payload = format!("frame@{us}").into_bytes();
		let frame = moq_native::moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp: Timestamp::new(us, Timescale::MICRO).unwrap(),
		};
		let mut writer = group.create_frame(frame).expect("failed to create frame");
		writer
			.write(bytes::Bytes::from(payload))
			.expect("failed to write frame");
		writer.finish().expect("failed to finish frame");
	}
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05".parse().unwrap()];
	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05".parse().unwrap()];
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(&pub_origin).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("consume_track failed");

	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");

	for &expected_us in &frames {
		let mut frame_sub = tokio::time::timeout(TIMEOUT, group_sub.next_frame())
			.await
			.expect("next_frame timed out")
			.expect("next_frame failed")
			.expect("group closed prematurely");

		let ts = frame_sub.timestamp;
		assert_eq!(ts.scale(), Timescale::MICRO);
		assert_eq!(ts.value(), expected_us);

		// Drain the payload so the stream advances to the next frame.
		let _ = frame_sub.read_all().await;
	}

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_05_timestamps_webtransport() {
	lite05_timestamp_roundtrip("https").await;
}

/// Lite05 FETCH round-trip: retrieve a past group by sequence without holding a
/// subscription, exercising the bare-FRAME fetch response and per-frame timestamp
/// decoding on the fetch stream. The track is also `compress`-hinted, so the
/// fetched frames are Deflate-compressed (matching TRACK_INFO) and inflated by the
/// subscriber, exercising fetch/TRACK_INFO codec consistency.
async fn lite05_fetch_roundtrip(scheme: &str) {
	use moq_native::moq_net::{Timescale, Timestamp};

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(Timescale::MICRO),
		)
		.expect("failed to create track");

	// A group with a few timestamped frames (middle PTS goes backwards, so the fetch
	// stream carries a negative zigzag delta too).
	let frames = [10_000u64, 30_000, 20_000];
	let mut group = track.append_group().expect("failed to append group"); // seq 0
	for &us in &frames {
		let payload = format!("frame@{us}").into_bytes();
		let frame = moq_native::moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp: Timestamp::new(us, Timescale::MICRO).unwrap(),
		};
		let mut writer = group.create_frame(frame).expect("failed to create frame");
		writer
			.write(bytes::Bytes::from(payload))
			.expect("failed to write frame");
		writer.finish().expect("failed to finish frame");
	}
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05".parse().unwrap()];
	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05".parse().unwrap()];
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(&pub_origin).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	// Fetch group 0 directly, without subscribing. No live producer holds the group
	// on the client, so this issues a wire FETCH upstream.
	let mut group_sub = tokio::time::timeout(TIMEOUT, async { bc.track("video").unwrap().fetch_group(0, None).await })
		.await
		.expect("fetch timed out")
		.expect("fetch failed");
	assert_eq!(group_sub.sequence, 0);

	for &expected_us in &frames {
		let mut frame_sub = tokio::time::timeout(TIMEOUT, group_sub.next_frame())
			.await
			.expect("next_frame timed out")
			.expect("next_frame failed")
			.expect("group closed prematurely");

		let ts = frame_sub.timestamp;
		assert_eq!(ts.scale(), Timescale::MICRO);
		assert_eq!(ts.value(), expected_us);

		let payload = frame_sub.read_all().await.expect("failed to read frame");
		assert_eq!(payload, bytes::Bytes::from(format!("frame@{expected_us}")));
	}

	// The fetched group ends cleanly (stream FIN → no more frames).
	let end = tokio::time::timeout(TIMEOUT, group_sub.next_frame())
		.await
		.expect("next_frame timed out")
		.expect("next_frame failed");
	assert!(end.is_none(), "group should finish after its frames");

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_05_fetch_webtransport() {
	// Exercises the WebTransport path; lite-05 is forced via config on both ends.
	// The raw-QUIC ALPN path is covered by broadcast_race_quic_wins.
	lite05_fetch_roundtrip("https").await;
}

/// A fetch must be served while a live subscription is active on the same track.
/// The relay subscribes starting at the latest group, so an older group isn't
/// cached and the fetch has to issue a wire FETCH concurrently with the
/// subscription. Older relays served a subscription OR a fetch, never both, so
/// this fetch would have hung.
async fn lite05_fetch_during_subscribe(scheme: &str) {
	use moq_native::moq_net::{Timescale, Timestamp};

	fn timestamped_frame(us: u64, payload: &str) -> moq_net::frame::Info {
		moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp: Timestamp::new(us, Timescale::MICRO).unwrap(),
		}
	}

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(Timescale::MICRO),
		)
		.expect("failed to create track");

	// Group 0 is the "past" group only reachable via FETCH; group 1 is the latest,
	// delivered live over the subscription.
	let mut group0 = track.append_group().expect("append group 0"); // seq 0
	let mut w = group0.create_frame(timestamped_frame(10_000, "old")).expect("frame 0");
	w.write(bytes::Bytes::from_static(b"old")).expect("write 0");
	w.finish().expect("finish frame 0");
	group0.finish().expect("finish group 0");

	let mut group1 = track.append_group().expect("append group 1"); // seq 1
	let mut w = group1.create_frame(timestamped_frame(20_000, "new")).expect("frame 1");
	w.write(bytes::Bytes::from_static(b"new")).expect("write 1");
	w.finish().expect("finish frame 1");
	group1.finish().expect("finish group 1");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05".parse().unwrap()];
	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05".parse().unwrap()];
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(&pub_origin).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	// Subscribe (starts at the latest group) and read the live group, which
	// establishes the upstream subscription and leaves it active.
	let mut track_sub = tokio::time::timeout(TIMEOUT, async { bc.track("video").unwrap().subscribe(None).await })
		.await
		.expect("subscribe timed out")
		.expect("subscribe failed");
	let mut live = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	assert_eq!(live.sequence, 1);
	let frame = tokio::time::timeout(TIMEOUT, live.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&*frame, b"new");

	// While the subscription is still held and active, fetch the older group. The
	// relay doesn't have it cached (subscription started at the latest), so this
	// must issue a wire FETCH concurrently with the live subscription.
	let mut fetched = tokio::time::timeout(TIMEOUT, async { bc.track("video").unwrap().fetch_group(0, None).await })
		.await
		.expect("fetch timed out")
		.expect("fetch failed");
	assert_eq!(fetched.sequence, 0);
	let frame = tokio::time::timeout(TIMEOUT, fetched.read_frame())
		.await
		.expect("fetch read_frame timed out")
		.expect("fetch read_frame failed")
		.expect("fetched group closed prematurely");
	assert_eq!(&*frame, b"old");

	// The live subscription is unaffected: a freshly published group still arrives.
	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_05_fetch_during_subscribe_webtransport() {
	lite05_fetch_during_subscribe("https").await;
}

/// On Lite05 timestamps are mandatory: a publisher that doesn't set a timescale gets
/// the default (milliseconds), and frames written without an explicit timestamp are
/// stamped with wall-clock time. The subscriber receives `Some(ts)` at that scale.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_05_default_timescale() {
	use moq_native::moq_net::Timescale;

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05".parse().unwrap()];
	let mut server = server_config.init().expect("init server");
	let addr = server.local_addr().expect("local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05".parse().unwrap()];
	let client = client_config.init().expect("init client");
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let session = request.with_publisher(&pub_origin).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timeout")
		.expect("connect failed");

	let (_, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timeout")
		.expect("origin closed");
	let bc = bc.broadcast().expect("expected announce");

	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("consume_track failed");

	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed");

	let frame_sub = tokio::time::timeout(TIMEOUT, group_sub.next_frame())
		.await
		.expect("next_frame timeout")
		.expect("next_frame failed")
		.expect("group closed");

	let ts = frame_sub.timestamp;
	assert_eq!(ts.scale(), Timescale::MILLI, "default timescale is milliseconds");

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

// ── Raw QUIC (moqt://) – same version on both sides ─────────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_01() {
	broadcast_test("moqt", Some("moq-lite-01"), Some("moq-lite-01")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_02() {
	broadcast_test("moqt", Some("moq-lite-02"), Some("moq-lite-02")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_03() {
	broadcast_test("moqt", Some("moq-lite-03"), Some("moq-lite-03")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_14() {
	broadcast_test("moqt", Some("moq-transport-14"), Some("moq-transport-14")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_15() {
	broadcast_test("moqt", Some("moq-transport-15"), Some("moq-transport-15")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_16() {
	broadcast_test("moqt", Some("moq-transport-16"), Some("moq-transport-16")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_17() {
	broadcast_test("moqt", Some("moq-transport-17"), Some("moq-transport-17")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_18() {
	broadcast_test("moqt", Some("moq-transport-18"), Some("moq-transport-18")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_transport_19() {
	broadcast_test("moqt", Some("moq-transport-19"), Some("moq-transport-19")).await;
}

// ── Raw QUIC – server supports all versions, client pins one ─────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_lite_01() {
	broadcast_test("moqt", Some("moq-lite-01"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_lite_02() {
	broadcast_test("moqt", Some("moq-lite-02"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_lite_03() {
	broadcast_test("moqt", Some("moq-lite-03"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_14() {
	broadcast_test("moqt", Some("moq-transport-14"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_15() {
	broadcast_test("moqt", Some("moq-transport-15"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_16() {
	broadcast_test("moqt", Some("moq-transport-16"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_17() {
	broadcast_test("moqt", Some("moq-transport-17"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_18() {
	broadcast_test("moqt", Some("moq-transport-18"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_server_all_client_transport_19() {
	broadcast_test("moqt", Some("moq-transport-19"), None).await;
}

// ── Raw QUIC – client supports all versions, server pins one ─────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_lite_01() {
	broadcast_test("moqt", None, Some("moq-lite-01")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_lite_02() {
	broadcast_test("moqt", None, Some("moq-lite-02")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_lite_03() {
	broadcast_test("moqt", None, Some("moq-lite-03")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_14() {
	broadcast_test("moqt", None, Some("moq-transport-14")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_15() {
	broadcast_test("moqt", None, Some("moq-transport-15")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_16() {
	broadcast_test("moqt", None, Some("moq-transport-16")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_17() {
	broadcast_test("moqt", None, Some("moq-transport-17")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_18() {
	broadcast_test("moqt", None, Some("moq-transport-18")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_negotiate_client_all_server_transport_19() {
	broadcast_test("moqt", None, Some("moq-transport-19")).await;
}

// ── WebTransport (https://) – same version on both sides ────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport() {
	broadcast_test("https", None, None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_lite_01() {
	broadcast_test("https", Some("moq-lite-01"), Some("moq-lite-01")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_lite_02() {
	broadcast_test("https", Some("moq-lite-02"), Some("moq-lite-02")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_lite_03() {
	broadcast_test("https", Some("moq-lite-03"), Some("moq-lite-03")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_14() {
	broadcast_test("https", Some("moq-transport-14"), Some("moq-transport-14")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_15() {
	broadcast_test("https", Some("moq-transport-15"), Some("moq-transport-15")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_16() {
	broadcast_test("https", Some("moq-transport-16"), Some("moq-transport-16")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_17() {
	broadcast_test("https", Some("moq-transport-17"), Some("moq-transport-17")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_18() {
	broadcast_test("https", Some("moq-transport-18"), Some("moq-transport-18")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_moq_transport_19() {
	broadcast_test("https", Some("moq-transport-19"), Some("moq-transport-19")).await;
}

// ── WebTransport – server supports all, client pins one ─────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_lite_01() {
	broadcast_test("https", Some("moq-lite-01"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_lite_02() {
	broadcast_test("https", Some("moq-lite-02"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_lite_03() {
	broadcast_test("https", Some("moq-lite-03"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_14() {
	broadcast_test("https", Some("moq-transport-14"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_15() {
	broadcast_test("https", Some("moq-transport-15"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_16() {
	broadcast_test("https", Some("moq-transport-16"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_17() {
	broadcast_test("https", Some("moq-transport-17"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_18() {
	broadcast_test("https", Some("moq-transport-18"), None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_server_all_client_transport_19() {
	broadcast_test("https", Some("moq-transport-19"), None).await;
}

// ── WebTransport – client supports all, server pins one ─────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_lite_01() {
	broadcast_test("https", None, Some("moq-lite-01")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_lite_02() {
	broadcast_test("https", None, Some("moq-lite-02")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_lite_03() {
	broadcast_test("https", None, Some("moq-lite-03")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_14() {
	broadcast_test("https", None, Some("moq-transport-14")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_15() {
	broadcast_test("https", None, Some("moq-transport-15")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_16() {
	broadcast_test("https", None, Some("moq-transport-16")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_17() {
	broadcast_test("https", None, Some("moq-transport-17")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_18() {
	broadcast_test("https", None, Some("moq-transport-18")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_webtransport_negotiate_client_all_server_transport_19() {
	broadcast_test("https", None, Some("moq-transport-19")).await;
}

// ── WebSocket (ws://) ───────────────────────────────────────────────

/// Test WebSocket transport end-to-end.
///
/// The server binds a WebSocket TCP listener on a separate port.
/// The client connects directly via ws://, bypassing QUIC entirely.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket() {
	use moq_native::moq_net::Origin;

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Server with both QUIC (required) and WebSocket listeners.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::websocket::Listener::bind("[::]:0".parse().unwrap())
		.await
		.expect("failed to bind WebSocket listener");
	let ws_addr = ws_listener.local_addr().expect("failed to get ws addr");

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_websocket(Some(ws_listener));

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	// Disable WebSocket delay so client connects immediately via ws://
	client_config.websocket.delay = None;

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("ws://localhost:{}", ws_addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		assert_eq!(request.transport(), "websocket");
		let session = request.with_publisher(&pub_origin).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("consume_track failed");

	// Read one group.
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");

	// Read one frame and verify the payload.
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");

	assert_eq!(&*frame, b"hello");

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// Test WebSocket fallback when QUIC is unavailable.
///
/// The client connects via `http://` to the WebSocket port. QUIC tries to
/// reach that port over UDP and fails (no QUIC listener there). The WebSocket
/// fallback converts `http://` → `ws://` and connects over TCP, succeeding.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket_fallback() {
	use moq_native::moq_net::Origin;

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// QUIC binds on its own port; WebSocket on a different port.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::websocket::Listener::bind("[::]:0".parse().unwrap())
		.await
		.expect("failed to bind WebSocket listener");
	let ws_addr = ws_listener.local_addr().expect("failed to get ws addr");

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_websocket(Some(ws_listener));

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	// No delay. Race QUIC and WebSocket simultaneously.
	client_config.websocket.delay = None;

	let client = client_config.init().expect("failed to init client");

	// Connect via http:// to the WebSocket port.
	// QUIC will try UDP on this port and fail; WebSocket will try ws:// and succeed.
	let url: url::Url = format!("http://localhost:{}", ws_addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		assert_eq!(request.transport(), "websocket");
		let session = request.with_publisher(&pub_origin).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("consume_track failed");

	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");

	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");

	assert_eq!(&*frame, b"hello");

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

// ── ALPN regression guards ──────────────────────────────────────────

/// The newest moq-lite version both sides advertise by default.
///
/// Bump this whenever [`moq_net::Versions::all`] gains a newer Lite variant
/// so the regression tests below keep tracking "the newest", not a frozen value.
const NEWEST_LITE: &str = "moq-lite-05";

/// Regression guard for the WebSocket ALPN path. Lite02 over WebSocket means
/// the qmux subprotocol negotiation produced a bare `moql` (or no match)
/// instead of `moq-lite-04`, which falls through to legacy SETUP negotiation
/// and picks Lite02. This test fails immediately if that happens.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket_uses_newest_version() {
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::websocket::Listener::bind("[::]:0".parse().unwrap())
		.await
		.expect("failed to bind WebSocket listener");
	let ws_addr = ws_listener.local_addr().expect("failed to get ws addr");

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_websocket(Some(ws_listener));

	let sub_origin = Origin::random().produce();
	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.websocket.delay = None;

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("ws://localhost:{}", ws_addr.port()).parse().unwrap();

	let expected_version: moq_net::Version = NEWEST_LITE.parse().expect("invalid version");

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		assert_eq!(request.transport(), "websocket");
		let session = request.with_publisher(&pub_origin).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	assert_eq!(cs.version(), expected_version, "client negotiated stale version");

	drop(cs);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// Regression guard for the QUIC vs WebSocket race. With both transports
/// reachable at the same URL, QUIC must win, since it's lower-latency and
/// has direct ALPN negotiation. A WebSocket win here means QUIC silently
/// regressed (and would also tend to drag the version down to Lite02 on
/// older relays). We bind WebSocket TCP and QUIC UDP to the same port,
/// then disable the head start so the race is genuine.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_race_quic_wins() {
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Bind WebSocket TCP first to pick a random port, then bind QUIC UDP to
	// the same port. UDP and TCP live in separate kernel namespaces, so this
	// works on every supported platform.
	let ws_listener = moq_native::websocket::Listener::bind("[::]:0".parse().unwrap())
		.await
		.expect("failed to bind WebSocket listener");
	let port = ws_listener.local_addr().expect("failed to get ws addr").port();

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some(format!("[::]:{port}"));
	server_config.tls.generate = vec!["localhost".into()];

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_websocket(Some(ws_listener));

	let sub_origin = Origin::random().produce();
	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	// Zero head start: QUIC has to win on its own merit, not by penalising WS.
	client_config.websocket.delay = None;

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("https://localhost:{port}").parse().unwrap();

	let expected_version: moq_net::Version = NEWEST_LITE.parse().expect("invalid version");

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		assert_eq!(
			request.transport(),
			"quic",
			"QUIC lost the race to WebSocket with both reachable",
		);
		let session = request.with_publisher(&pub_origin).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	assert_eq!(cs.version(), expected_version, "client negotiated stale version");

	drop(cs);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

// ── Linger: relay-style subscription reuse ──────────────────────────
//
// When the last consumer of an upstream subscription drops, the relay keeps
// the TrackProducer alive briefly so a returning consumer reuses it instead
// of triggering a fresh upstream Subscribe. This is the moq-lite linger
// behavior: on `track.unused()` the subscriber sends `SubscribeUpdate(priority=0)`
// + FIN, then waits up to LINGER_TIMEOUT for either the upstream to FIN back,
// the timeout to expire, or a new consumer to arrive (resume with
// `start_group = max + 1`).
//
// Reliably distinguishing the Reused vs Complete vs Cancelled branches from a
// black-box client test is hard because all three paths converge on the same
// observable behavior at the consumer (groups eventually flow). What we *can*
// test cheaply here is the end-to-end smoke: subscribe → drop → resubscribe
// must keep working, with a fresh group flowing afterwards.

/// Smoke test: dropping the last consumer and resubscribing within the linger
/// window doesn't wedge the subscription, and groups appended after the resume
/// still arrive at the new consumer.
#[tokio::test]
async fn linger_resubscribe_keeps_flowing_moq_lite_03() {
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	let mut group0 = track.append_group().expect("append group 0");
	group0.write_frame_now(b"a".as_ref()).expect("write frame 0");
	group0.finish().expect("finish group 0");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-03".parse().unwrap()];
	let mut server = server_config.init().expect("init server");
	let addr = server.local_addr().expect("server addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-03".parse().unwrap()];
	let client = client_config.init().expect("init client");
	let url: url::Url = format!("moqt://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let session = request.with_publisher(&pub_origin).ok().await?;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timeout")
		.expect("connect failed");

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timeout")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce");

	// First subscription: receive group 0.
	let mut sub1 = bc.track("video").unwrap().subscribe(None).await.expect("subscribe1");
	let mut g = tokio::time::timeout(TIMEOUT, sub1.recv_group())
		.await
		.expect("recv group 0 timeout")
		.expect("recv group 0 failed")
		.expect("track closed early");
	assert_eq!(g.sequence, 0);
	let frame = tokio::time::timeout(TIMEOUT, g.read_frame())
		.await
		.expect("read frame 0 timeout")
		.expect("read frame 0 failed")
		.expect("group closed early");
	assert_eq!(&*frame, b"a");

	// Drop the only consumer to trigger the linger phase.
	drop(g);
	drop(sub1);

	// Yield a few times so the subscriber task can observe `track.unused()` and
	// enter the linger select. A small real sleep also makes the test less
	// scheduler-dependent across runtimes.
	tokio::time::sleep(Duration::from_millis(20)).await;

	// Resubscribe well inside the 5s linger window.
	let mut sub2 = bc.track("video").unwrap().subscribe(None).await.expect("subscribe2");

	// A new group published after the resubscribe must reach the consumer
	// regardless of which linger branch fired.
	let mut group1 = track.append_group().expect("append group 1");
	group1.write_frame_now(b"b".as_ref()).expect("write frame 1");
	group1.finish().expect("finish group 1");

	let mut saw_group1 = false;
	for _ in 0..2 {
		let mut next = tokio::time::timeout(TIMEOUT, sub2.recv_group())
			.await
			.expect("recv group timeout")
			.expect("recv group failed")
			.expect("track closed early");
		if next.sequence == 1 {
			let frame = tokio::time::timeout(TIMEOUT, next.read_frame())
				.await
				.expect("read frame 1 timeout")
				.expect("read frame 1 failed")
				.expect("group closed early on resume");
			assert_eq!(&*frame, b"b");
			saw_group1 = true;
			break;
		}
	}
	assert!(
		saw_group1,
		"expected group 1 to be delivered to the resubscribed consumer"
	);

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

#[tracing_test::traced_test]
#[tokio::test]
async fn websocket_unauthorized_handshake_is_explicit() {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("failed to bind TCP listener");
	let addr = listener.local_addr().expect("failed to get local addr");

	let server_handle = tokio::spawn(async move {
		let (mut stream, _) = listener.accept().await?;
		let mut buf = [0; 1024];
		let _ = stream.read(&mut buf).await?;
		stream
			.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
			.await?;
		Ok::<_, anyhow::Error>(())
	});

	let mut client_config = moq_native::ClientConfig::default();
	client_config.websocket.delay = None;
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("ws://{addr}").parse().unwrap();

	let err = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out");
	let err = expect_connect_err(err);
	assert_connect_error(&err, moq_native::ConnectError::Unauthorized);

	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

#[tracing_test::traced_test]
#[tokio::test]
async fn reconnect_stops_on_websocket_unauthorized() {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("failed to bind TCP listener");
	let addr = listener.local_addr().expect("failed to get local addr");

	let server_handle = tokio::spawn(async move {
		let (mut stream, _) = listener.accept().await?;
		let mut buf = [0; 1024];
		let _ = stream.read(&mut buf).await?;
		stream
			.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
			.await?;
		Ok::<_, anyhow::Error>(())
	});

	let mut client_config = moq_native::ClientConfig::default();
	client_config.websocket.delay = None;
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("ws://{addr}").parse().unwrap();

	let reconnect = client.reconnect(url);
	let err = tokio::time::timeout(TIMEOUT, reconnect.closed())
		.await
		.expect("reconnect close timed out")
		.expect_err("reconnect unexpectedly succeeded");
	assert_connect_error(&err, moq_native::ConnectError::Unauthorized);

	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// A peer that expresses announce-interest in a prefix the publisher can't serve (e.g. a
/// subscribe-restricted token) must not tear down the whole session. The publisher FINs that
/// announce stream cleanly; the connection and other announce streams keep working.
#[tracing_test::traced_test]
#[tokio::test]
async fn announce_interest_unauthorized_keeps_session_alive() {
	use moq_native::moq_net::Origin;

	// ── publisher (server): only allowed to announce under "allowed" ──
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin
		.create_broadcast("allowed/test")
		.expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let publish = pub_origin
		.consume()
		.scope(&["allowed".into()])
		.expect("failed to scope publish origin");

	let (mut server, addr) = test_server();

	// ── subscriber (client): interested in both "allowed" and "denied" ──
	// "denied" is disjoint from the publisher's scope, so its announce stream is FINed.
	let sub_origin = Origin::random().produce();
	let consume = sub_origin
		.scope(&["allowed".into(), "denied".into()])
		.expect("failed to scope consume origin");
	let mut announcements = consume.consume().announced();

	let client = test_client();
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(publish).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_subscriber(consume);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// The "allowed" announce stream still delivers even though "denied" was FINed.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announce timed out")
		.expect("origin closed");
	assert_eq!(path.as_str(), "allowed/test");
	assert!(bc.broadcast().is_some(), "expected announce, got unannounce");

	// The unauthorized "denied" interest must not have torn down the session.
	assert!(
		tokio::time::timeout(Duration::from_millis(200), session.closed())
			.await
			.is_err(),
		"session closed after unauthorized announce interest",
	);

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// Reverse of the usual direction: a publish-only client (`with_publisher`, no `with_subscriber`)
/// serving a subscribe-only server (`with_subscriber`, no `with_publisher`). The server is also
/// interested in a disjoint "denied" prefix the client can't serve, so the server's
/// subscriber must survive that FIN and still receive the served broadcast.
#[tracing_test::traced_test]
#[tokio::test]
async fn publish_only_client_to_subscribe_only_server() {
	use moq_native::moq_net::Origin;

	// ── subscriber (server): interested in both "allowed" and "denied" ──
	let sub_origin = Origin::random().produce();
	let consume = sub_origin
		.scope(&["allowed".into(), "denied".into()])
		.expect("failed to scope consume origin");
	let mut announcements = consume.consume().announced();

	let (mut server, addr) = test_server();
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let session = server
			.accept()
			.await
			.expect("no incoming connection")
			.with_subscriber(consume)
			.ok()
			.await?;

		// The client serves "allowed/test"; the "denied" interest is FINed but must not
		// tear down the session.
		let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
			.await
			.expect("announce timed out")
			.expect("origin closed");
		assert_eq!(path.as_str(), "allowed/test");
		let bc = bc.broadcast().expect("expected announce, got unannounce");

		let mut track_sub = bc
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("consume_track failed");
		let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
			.await
			.expect("recv_group timed out")
			.expect("recv_group failed")
			.expect("track closed prematurely");
		let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
			.await
			.expect("read_frame timed out")
			.expect("read_frame failed")
			.expect("group closed prematurely");
		assert_eq!(&*frame, b"hello");

		// The disjoint "denied" interest must not have torn down the session.
		assert!(
			tokio::time::timeout(Duration::from_millis(200), session.closed())
				.await
				.is_err(),
			"server session closed after unauthorized announce interest",
		);

		Ok::<_, anyhow::Error>(())
	});

	// ── publisher (client): only allowed to serve under "allowed" ──
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin
		.create_broadcast("allowed/test")
		.expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame_now(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let publish = pub_origin
		.consume()
		.scope(&["allowed".into()])
		.expect("failed to scope publish origin");

	let session = tokio::time::timeout(TIMEOUT, test_client().with_publisher(publish).connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");

	drop(session);
	drop(track);
	drop(broadcast);
}

/// A test server bound to a free port with a generated localhost certificate.
fn test_server() -> (moq_native::Server, std::net::SocketAddr) {
	let mut config = moq_native::ServerConfig::default();
	config.bind = Some("[::]:0".to_string());
	config.tls.generate = vec!["localhost".into()];
	let server = config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");
	(server, addr)
}

/// A test client that skips TLS verification (servers use self-signed certs).
fn test_client() -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(true);
	config.init().expect("failed to init client")
}

fn assert_connect_error(err: &moq_native::Error, expected: moq_native::ConnectError) {
	assert_eq!(err.connect_error(), Some(expected), "unexpected error: {err}",);
}

fn expect_connect_err(result: moq_native::Result<moq_net::Session>) -> moq_native::Error {
	match result {
		Ok(_) => panic!("client connect unexpectedly succeeded"),
		Err(err) => err,
	}
}

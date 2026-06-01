//! Integration test: verify that announcing a broadcast and subscribing to a
//! track works end-to-end for every supported protocol version.
//!
//! The server publishes a broadcast containing a track with known data.
//! The client connects, receives the announcement, subscribes to the track,
//! and verifies it receives the correct payload.
//!
//! This covers raw QUIC (moqt://) and WebTransport (https://) transports,
//! exercising every protocol version the library supports.

use moq_native::moq_net::{self, Origin, Track};
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
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	// Write a group containing a single frame.
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
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
		let session = request.with_publisher(pub_origin.clone()).ok().await?;

		// Keep producers alive so the subscriber can read data.
		let _broadcast = broadcast;
		let _track = track;

		// Block until the client disconnects.
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
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
	use moq_native::moq_net::{Subscription, Timescale, Timestamp};

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");

	// Track with an advertised microsecond timescale. Without it, Lite05 publish
	// fails with ProtocolViolation.
	let mut track = broadcast
		.create_track(Track::new("video").with_timescale(Timescale::MICRO))
		.expect("failed to create track");

	// Three frames where the middle PTS goes backwards (B-frame decode order)
	// so the zigzag encoding has to carry a negative delta.
	let timestamps_us = [10_000u64, 30_000, 20_000];
	let mut group = track.append_group().expect("failed to append group");
	for &us in &timestamps_us {
		let payload = format!("frame@{us}").into_bytes();
		let frame = moq_native::moq_net::Frame {
			size: payload.len() as u64,
			timestamp: Some(Timestamp::new(us, Timescale::MICRO).unwrap()),
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
	server_config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publisher(pub_origin.clone()).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
		.consume_track("video")
		.subscribe(Subscription::default())
		.await
		.expect("consume_track failed");

	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timed out")
		.expect("recv_group failed")
		.expect("track closed prematurely");

	for &expected_us in &timestamps_us {
		let mut frame_sub = tokio::time::timeout(TIMEOUT, group_sub.next_frame())
			.await
			.expect("next_frame timed out")
			.expect("next_frame failed")
			.expect("group closed prematurely");

		let ts = frame_sub.timestamp.expect("Lite05 must carry per-frame timestamps");
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

/// On Lite05 a publisher that doesn't advertise a timescale still works:
/// SUBSCRIBE_OK carries `timescale = 0` and neither side encodes a
/// per-frame timestamp byte. Subscribers receive `frame.timestamp = None`.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_moq_lite_05_without_timescale() {
	use moq_native::moq_net::Subscription;

	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track(Track::new("video")).expect("create track");

	let mut group = track.append_group().expect("append group");
	group.write_frame(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	let mut server = server_config.init().expect("init server");
	let addr = server.local_addr().expect("local addr");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	let client = client_config.init().expect("init client");
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let session = request.with_publisher(pub_origin.clone()).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
		.consume_track("video")
		.subscribe(Subscription::default())
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

	assert_eq!(
		frame_sub.timestamp, None,
		"no timescale negotiated, no per-frame timestamp"
	);

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

// ── WebSocket (ws://) ───────────────────────────────────────────────

/// Test WebSocket transport end-to-end.
///
/// The server binds a WebSocket TCP listener on a separate port.
/// The client connects directly via ws://, bypassing QUIC entirely.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket() {
	use moq_native::moq_net::{Origin, Track};

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Server with both QUIC (required) and WebSocket listeners.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::WebSocketListener::bind("[::]:0".parse().unwrap())
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
		let session = request.with_publisher(pub_origin.clone()).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
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
	use moq_native::moq_net::{Origin, Track};

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// QUIC binds on its own port; WebSocket on a different port.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::WebSocketListener::bind("[::]:0".parse().unwrap())
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
	// No delay — race QUIC and WebSocket simultaneously.
	client_config.websocket.delay = None;

	let client = client_config.init().expect("failed to init client");

	// Connect via http:// to the WebSocket port.
	// QUIC will try UDP on this port and fail; WebSocket will try ws:// and succeed.
	let url: url::Url = format!("http://localhost:{}", ws_addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		assert_eq!(request.transport(), "websocket");
		let session = request.with_publisher(pub_origin.clone()).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
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
const NEWEST_LITE: &str = "moq-lite-04";

/// Regression guard for the WebSocket ALPN path. Lite02 over WebSocket means
/// the qmux subprotocol negotiation produced a bare `moql` (or no match)
/// instead of `moq-lite-04`, which falls through to legacy SETUP negotiation
/// and picks Lite02. This test fails immediately if that happens.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket_uses_newest_version() {
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let ws_listener = moq_native::WebSocketListener::bind("[::]:0".parse().unwrap())
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
		let session = request.with_publisher(pub_origin.clone()).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Bind WebSocket TCP first to pick a random port, then bind QUIC UDP to
	// the same port. UDP and TCP live in separate kernel namespaces, so this
	// works on every supported platform.
	let ws_listener = moq_native::WebSocketListener::bind("[::]:0".parse().unwrap())
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
		let session = request.with_publisher(pub_origin.clone()).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
	let mut track = broadcast.create_track(Track::new("video")).expect("create track");

	let mut group0 = track.append_group().expect("append group 0");
	group0.write_frame(b"a".as_ref()).expect("write frame 0");
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
		let session = request.with_publisher(pub_origin.clone()).ok().await?;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consumer(sub_origin);
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
	let mut sub1 = bc
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
		.await
		.expect("subscribe1");
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
	let mut sub2 = bc
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
		.await
		.expect("subscribe2");

	// A new group published after the resubscribe must reach the consumer
	// regardless of which linger branch fired.
	let mut group1 = track.append_group().expect("append group 1");
	group1.write_frame(b"b".as_ref()).expect("write frame 1");
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

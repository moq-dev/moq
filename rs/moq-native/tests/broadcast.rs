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
	let mut announcements = sub_origin.consume();

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
		let session = request.with_publish(pub_origin.consume()).ok().await?;

		// Keep producers alive so the subscriber can read data.
		let _broadcast = broadcast;
		let _track = track;

		// Block until the client disconnects.
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.subscribe_track(&Track::new("video"))
		.expect("subscribe_track failed");

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
	let mut announcements = sub_origin.consume();

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
		let session = request.with_publish(pub_origin.consume()).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.subscribe_track(&Track::new("video"))
		.expect("subscribe_track failed");

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
	let mut announcements = sub_origin.consume();

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
		let session = request.with_publish(pub_origin.consume()).ok().await?;

		let _broadcast = broadcast;
		let _track = track;

		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// Wait for the broadcast announcement.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
	let bc = bc.expect("expected announce, got unannounce");

	// Subscribe to the track.
	let mut track_sub = bc
		.subscribe_track(&Track::new("video"))
		.expect("subscribe_track failed");

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
		let session = request.with_publish(pub_origin.consume()).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	assert_eq!(session.version(), expected_version, "client negotiated stale version");

	drop(session);
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
		let session = request.with_publish(pub_origin.consume()).ok().await?;
		assert_eq!(session.version(), expected_version, "server negotiated stale version");
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(sub_origin);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	assert_eq!(session.version(), expected_version, "client negotiated stale version");

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
	use moq_native::moq_net::{Origin, Track};

	// ── publisher (server): only allowed to announce under "allowed" ──
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin
		.create_broadcast("allowed/test")
		.expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
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
	let mut announcements = consume.consume();

	let client = test_client();
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		let session = request.with_publish(publish).ok().await?;
		let _broadcast = broadcast;
		let _track = track;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(())
	});

	let client = client.with_consume(consume);
	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	// The "allowed" announce stream still delivers even though "denied" was FINed.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");
	assert_eq!(path.as_str(), "allowed/test");
	assert!(bc.is_some(), "expected announce, got unannounce");

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

/// Reverse of the usual direction: a publish-only client (`with_publish`, no `with_consume`)
/// serving a subscribe-only server (`with_consume`, no `with_publish`). The server is also
/// interested in a disjoint "denied" prefix the client can't serve, so the server's
/// subscriber must survive that FIN and still receive the served broadcast.
#[tracing_test::traced_test]
#[tokio::test]
async fn publish_only_client_to_subscribe_only_server() {
	use moq_native::moq_net::{Origin, Track};

	// ── subscriber (server): interested in both "allowed" and "denied" ──
	let sub_origin = Origin::random().produce();
	let consume = sub_origin
		.scope(&["allowed".into(), "denied".into()])
		.expect("failed to scope consume origin");
	let mut announcements = consume.consume();

	let (mut server, addr) = test_server();
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let session = server
			.accept()
			.await
			.expect("no incoming connection")
			.with_consume(consume)
			.ok()
			.await?;

		// The client serves "allowed/test"; the "denied" interest is FINed but must not
		// tear down the session.
		let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
			.await
			.expect("announce timed out")
			.expect("origin closed");
		assert_eq!(path.as_str(), "allowed/test");
		let bc = bc.expect("expected announce, got unannounce");

		let mut track_sub = bc
			.subscribe_track(&Track::new("video"))
			.expect("subscribe_track failed");
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
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let publish = pub_origin
		.consume()
		.scope(&["allowed".into()])
		.expect("failed to scope publish origin");

	let session = tokio::time::timeout(TIMEOUT, test_client().with_publish(publish).connect(url))
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

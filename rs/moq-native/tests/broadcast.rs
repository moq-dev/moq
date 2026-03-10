//! Integration test: verify that announcing a broadcast and subscribing to a
//! track works end-to-end for every supported protocol version.
//!
//! The server publishes a broadcast containing a track with known data.
//! The client connects, receives the announcement, subscribes to the track,
//! and verifies it receives the correct payload.
//!
//! This covers raw QUIC (moqt://) and WebTransport (https://) transports,
//! exercising every protocol version the library supports.

use moq_native::moq_lite::{self, Origin, Track};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Publish a broadcast on the server, subscribe on the client, and verify
/// the data arrives correctly for the given URL scheme and version configuration.
///
/// `client_version` and `server_version` can differ to test version negotiation.
/// `None` means "support all versions" (empty version vec).
async fn broadcast_test(scheme: &str, client_version: Option<&str>, server_version: Option<&str>) {
	let client_version: Option<moq_lite::Version> = client_version.map(|v| v.parse().expect("invalid client version"));
	let server_version: Option<moq_lite::Version> = server_version.map(|v| v.parse().expect("invalid server version"));

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	// Write a group containing a single frame.
	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	if let Some(v) = server_version {
		server_config.version = vec![v];
	}

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::produce();
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
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.next_group())
		.await
		.expect("next_group timed out")
		.expect("next_group failed")
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

// ── WebSocket (ws://) ───────────────────────────────────────────────

/// Test WebSocket transport end-to-end.
///
/// The server binds a WebSocket TCP listener on a separate port.
/// The client connects directly via ws://, bypassing QUIC entirely.
#[tracing_test::traced_test]
#[tokio::test]
async fn broadcast_websocket() {
	use moq_native::moq_lite::{Origin, Track};

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Server with both QUIC (required) and WebSocket listeners.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
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
	let sub_origin = Origin::produce();
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
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.next_group())
		.await
		.expect("next_group timed out")
		.expect("next_group failed")
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
	use moq_native::moq_lite::{Origin, Track};

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// QUIC binds on its own port; WebSocket on a different port.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
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
	let sub_origin = Origin::produce();
	let mut announcements = sub_origin.consume();

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

	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.next_group())
		.await
		.expect("next_group timed out")
		.expect("next_group failed")
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

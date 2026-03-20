//! Integration tests that explicitly exercise each QUIC backend (quinn, quiche, iroh)
//! with a simple client/server connect + broadcast flow.
//!
//! Each test is gated with `#[cfg(feature = "...")]` so it only compiles when the
//! corresponding backend is enabled. Running `cargo test --all-features` exercises all.

use moq_native::moq_lite::{Origin, Track};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Publish a broadcast on the server, subscribe on the client, and verify
/// the data arrives correctly using the specified QUIC backend and URL scheme.
#[cfg(any(feature = "quinn", feature = "quiche", feature = "noq"))]
async fn backend_test(scheme: &str, backend: moq_native::QuicBackend) {
	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.backend = Some(backend.clone());

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::produce();
	let mut announcements = sub_origin.consume();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.backend = Some(backend);

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
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

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
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

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

// ── Quinn backend ───────────────────────────────────────────────────

#[cfg(feature = "quinn")]
#[tracing_test::traced_test]
#[tokio::test]
async fn quinn_raw_quic() {
	backend_test("moqt", moq_native::QuicBackend::Quinn).await;
}

#[cfg(feature = "quinn")]
#[tracing_test::traced_test]
#[tokio::test]
async fn quinn_webtransport() {
	backend_test("https", moq_native::QuicBackend::Quinn).await;
}

// ── Quiche backend ──────────────────────────────────────────────────

#[cfg(feature = "quiche")]
#[tracing_test::traced_test]
#[tokio::test]
#[ignore = "quiche raw QUIC (moqt://) fails; likely a web-transport-quiche bug"]
async fn quiche_raw_quic() {
	backend_test("moqt", moq_native::QuicBackend::Quiche).await;
}

#[cfg(feature = "quiche")]
#[tracing_test::traced_test]
#[tokio::test]
async fn quiche_webtransport() {
	backend_test("https", moq_native::QuicBackend::Quiche).await;
}

// ── Iroh backend ────────────────────────────────────────────────────

#[cfg(feature = "iroh")]
#[tracing_test::traced_test]
#[tokio::test]
async fn iroh_connect() {
	use moq_native::IrohEndpointConfig;

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast
		.create_track(Track::new("video"))
		.expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group.write_frame(b"hello".as_ref()).expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Create server iroh endpoint
	let mut server_iroh_config = IrohEndpointConfig::default();
	server_iroh_config.enabled = Some(true);
	let server_endpoint = server_iroh_config
		.bind()
		.await
		.expect("failed to bind server iroh endpoint")
		.expect("server iroh endpoint not enabled");

	// Get the server's direct addresses before moving it into the server.
	let server_addr = server_endpoint.addr();
	let server_addrs: Vec<std::net::SocketAddr> = server_addr.ip_addrs().copied().collect();

	let server_endpoint_id = server_endpoint.id();

	// Server still needs a QUIC bind for init, but we'll connect via iroh
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_iroh(Some(server_endpoint));

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::produce();
	let mut announcements = sub_origin.consume();

	// Create client iroh endpoint
	let mut client_iroh_config = IrohEndpointConfig::default();
	client_iroh_config.enabled = Some(true);
	let client_endpoint = client_iroh_config
		.bind()
		.await
		.expect("failed to bind client iroh endpoint")
		.expect("client iroh endpoint not enabled");

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);

	let client = client_config
		.init()
		.expect("failed to init client")
		.with_iroh(Some(client_endpoint))
		.with_iroh_addrs(server_addrs);

	let url: url::Url = format!("iroh://{server_endpoint_id}").parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
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

	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
		.await
		.expect("announce timed out")
		.expect("origin closed");

	assert_eq!(path.as_str(), "test");
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

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

// ── Noq backend ─────────────────────────────────────────────────────

#[cfg(feature = "noq")]
#[tracing_test::traced_test]
#[tokio::test]
async fn noq_raw_quic() {
	backend_test("moqt", moq_native::QuicBackend::Noq).await;
}

#[cfg(feature = "noq")]
#[tracing_test::traced_test]
#[tokio::test]
async fn noq_webtransport() {
	backend_test("https", moq_native::QuicBackend::Noq).await;
}

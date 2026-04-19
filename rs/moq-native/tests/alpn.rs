//! Integration test: verify that forcing each supported version results in a
//! successful MoQ handshake between a Quinn server and client.
//!
//! This covers both ALPN-based version negotiation (moq-lite-03, moqt-15,
//! moqt-16) and SETUP-based version negotiation (moql, moq-00) used by
//! older protocol versions like moq-transport-14 and moq-lite-01/02.
//!
//! It also tests WebTransport, which uses sub-protocols in the HTTP CONNECT
//! request instead of TLS ALPN, but serves the same purpose.

/// Spin up a server and client both restricted to the given version,
/// and verify the handshake completes over raw QUIC (moqt:// URL).
async fn connect_with_version(version: &str) {
	let version: moq_native::moq_lite::Version = version.parse().expect("invalid version");

	// ── server ──────────────────────────────────────────────────────
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec![version];

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// Provide a dummy origin so the MoQ handshake has something to negotiate.
	let origin = moq_native::moq_lite::Origin::produce();
	let consumer = origin.consume();

	// ── client ──────────────────────────────────────────────────────
	let mut client_config = moq_native::ClientConfig::default();
	client_config.version = vec![version];
	client_config.tls.disable_verify = Some(true);

	let client = client_config.init().expect("failed to init client");

	// Use raw QUIC URL so ALPN negotiation is direct (no WebTransport framing).
	let url: url::Url = format!("moqt://localhost:{}", addr.port()).parse().unwrap();

	// Run server accept and client connect concurrently.
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		request.with_publish(consumer).ok().await
	});

	let client = client.with_publish(origin.consume());
	let client_result = client.connect(url).await;

	let server_result = server_handle.await.expect("server task panicked");

	// Both sides should succeed.
	if let Err(err) = &client_result {
		panic!("client handshake failed for version {version}: {err}");
	}
	if let Err(err) = &server_result {
		panic!("server handshake failed for version {version}: {err}");
	}
}

/// Connect via WebTransport (https:// URL with h3 ALPN).
/// Sub-protocols in the HTTP CONNECT request serve the same role as ALPN.
/// If a version is specified, both client and server are restricted to it.
async fn connect_with_webtransport(version: Option<&str>) {
	let version: Option<moq_native::moq_lite::Version> = version.map(|v| v.parse().expect("invalid version"));

	// ── server ──────────────────────────────────────────────────────
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	if let Some(v) = version {
		server_config.version = vec![v];
	}

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let origin = moq_native::moq_lite::Origin::produce();
	let consumer = origin.consume();

	// ── client ──────────────────────────────────────────────────────
	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	if let Some(v) = version {
		client_config.version = vec![v];
	}

	let client = client_config.init().expect("failed to init client");

	// Use https:// URL to trigger the WebTransport path.
	let url: url::Url = format!("https://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		request.with_publish(consumer).ok().await
	});

	let client = client.with_publish(origin.consume());
	let client_result = client.connect(url).await;

	let server_result = server_handle.await.expect("server task panicked");

	let label = version.map_or("default".to_string(), |v| v.to_string());
	if let Err(err) = &client_result {
		panic!("client WebTransport handshake failed for {label}: {err}");
	}
	if let Err(err) = &server_result {
		panic!("server WebTransport handshake failed for {label}: {err}");
	}
}

// ── SETUP-based version negotiation (via "moql" ALPN) ───────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_lite_01() {
	connect_with_version("moq-lite-01").await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_lite_02() {
	connect_with_version("moq-lite-02").await;
}

// ── ALPN-based version negotiation (no SETUP stream) ────────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_lite_03() {
	connect_with_version("moq-lite-03").await;
}

// ── SETUP-based via "moq-00" ALPN ───────────────────────────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_transport_14() {
	connect_with_version("moq-transport-14").await;
}

// ── ALPN-based (newer IETF drafts) ──────────────────────────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_transport_15() {
	connect_with_version("moq-transport-15").await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn version_moq_transport_16() {
	connect_with_version("moq-transport-16").await;
}

// ── WebTransport: sub-protocol negotiation ──────────────────────────
// Browser clients use WebTransport (h3 ALPN) and negotiate the MoQ
// protocol version via sub-protocols in the HTTP CONNECT request.

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport() {
	connect_with_webtransport(None).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_lite_01() {
	connect_with_webtransport(Some("moq-lite-01")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_lite_02() {
	connect_with_webtransport(Some("moq-lite-02")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_lite_03() {
	connect_with_webtransport(Some("moq-lite-03")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_transport_14() {
	connect_with_webtransport(Some("moq-transport-14")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_transport_15() {
	connect_with_webtransport(Some("moq-transport-15")).await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport_moq_transport_16() {
	connect_with_webtransport(Some("moq-transport-16")).await;
}

//! Integration test: verify that forcing each supported ALPN results in a
//! successful MoQ handshake between a Quinn server and client.
//!
//! This covers both ALPN-based version negotiation (moq-lite-03, moqt-15,
//! moqt-16) and SETUP-based version negotiation (moql, moq-00) used by
//! older protocol versions like moq-transport-14 and moq-lite-02.
//!
//! It also tests WebTransport, which uses sub-protocols in the HTTP CONNECT
//! request instead of TLS ALPN, but serves the same purpose.

/// Spin up a server, connect a client with a forced ALPN, and verify the
/// handshake completes over raw QUIC (moqt:// URL).
async fn connect_with_alpn(alpn: &str) {
	// ── server ──────────────────────────────────────────────────────
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// Provide a dummy origin so the MoQ handshake has something to negotiate.
	let origin = moq_native::moq_lite::Origin::produce();
	let consumer = origin.consume();

	// ── client ──────────────────────────────────────────────────────
	let mut client_config = moq_native::ClientConfig::default();
	client_config.alpn = vec![alpn.to_string()];
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
		panic!("client handshake failed for ALPN {alpn}: {err}");
	}
	if let Err(err) = &server_result {
		panic!("server handshake failed for ALPN {alpn}: {err}");
	}
}

/// Connect via WebTransport (https:// URL with h3 ALPN).
/// Sub-protocols in the HTTP CONNECT request serve the same role as ALPN.
async fn connect_with_webtransport() {
	// ── server ──────────────────────────────────────────────────────
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let origin = moq_native::moq_lite::Origin::produce();
	let consumer = origin.consume();

	// ── client ──────────────────────────────────────────────────────
	let mut client_config = moq_native::ClientConfig::default();
	// Don't force any ALPN: https:// will use h3 and negotiate sub-protocols.
	client_config.tls.disable_verify = Some(true);

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

	if let Err(err) = &client_result {
		panic!("client WebTransport handshake failed: {err}");
	}
	if let Err(err) = &server_result {
		panic!("server WebTransport handshake failed: {err}");
	}
}

// ── Raw QUIC: ALPN-based version negotiation (no SETUP stream) ──────

#[tracing_test::traced_test]
#[tokio::test]
async fn alpn_moq_lite_03() {
	connect_with_alpn("moq-lite-03").await;
}

// ── Raw QUIC: SETUP-based version negotiation ───────────────────────
// Old clients (moq-transport-14 / moq-lite-02) use the generic "moql"
// or "moq-00" ALPN and then exchange version numbers via a SETUP stream.

#[tracing_test::traced_test]
#[tokio::test]
async fn alpn_moq_lite() {
	connect_with_alpn("moql").await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn alpn_ietf_14() {
	connect_with_alpn("moq-00").await;
}

// ── Raw QUIC: ALPN-based (newer drafts) ─────────────────────────────

#[tracing_test::traced_test]
#[tokio::test]
async fn alpn_ietf_15() {
	connect_with_alpn("moqt-15").await;
}

#[tracing_test::traced_test]
#[tokio::test]
async fn alpn_ietf_16() {
	connect_with_alpn("moqt-16").await;
}

// ── WebTransport: sub-protocol negotiation ──────────────────────────
// Browser clients use WebTransport (h3 ALPN) and negotiate the MoQ
// protocol version via sub-protocols in the HTTP CONNECT request.

#[tracing_test::traced_test]
#[tokio::test]
async fn webtransport() {
	connect_with_webtransport().await;
}

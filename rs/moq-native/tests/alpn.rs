//! Integration test: verify that forcing each supported ALPN results in a
//! successful MoQ handshake between a Quinn server and client.

fn install_crypto_provider() {
	let _ = moq_native::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Spin up a server, connect a client with a forced ALPN, and verify the
/// handshake completes.
async fn connect_with_alpn(alpn: &str) {
	install_crypto_provider();

	// ── server ──────────────────────────────────────────────────────
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("127.0.0.1:0".parse().unwrap());
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

// moq-lite-03: ALPN-based version negotiation (no SETUP stream).
#[tokio::test]
async fn alpn_moq_lite_03() {
	connect_with_alpn("moq-lite-03").await;
}

// moql: moq-lite with SETUP-based version negotiation.
#[tokio::test]
async fn alpn_moq_lite() {
	connect_with_alpn("moql").await;
}

// moqt-16: IETF MoQ Transport Draft 16.
#[tokio::test]
async fn alpn_ietf_16() {
	connect_with_alpn("moqt-16").await;
}

// moqt-15: IETF MoQ Transport Draft 15.
#[tokio::test]
async fn alpn_ietf_15() {
	connect_with_alpn("moqt-15").await;
}

// moq-00: IETF MoQ Transport Draft 14.
#[tokio::test]
async fn alpn_ietf_14() {
	connect_with_alpn("moq-00").await;
}

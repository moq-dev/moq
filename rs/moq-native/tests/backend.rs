//! Integration tests that explicitly exercise each QUIC backend (quinn, quiche, iroh)
//! with a simple client/server connect + broadcast flow.
//!
//! Each test is gated with `#[cfg(feature = "...")]` so it only compiles when the
//! corresponding backend is enabled. Running `cargo test --all-features` exercises all.

use moq_native::moq_net::Origin;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Publish a broadcast on the server, subscribe on the client, and verify
/// the data arrives correctly using the specified QUIC backend and URL scheme.
#[cfg(any(feature = "quinn", feature = "quiche", feature = "noq"))]
async fn backend_test(scheme: &str, backend: moq_native::QuicBackend) {
	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group
		.write_frame(moq_native::moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("failed to write frame");
	group.finish().expect("failed to finish group");

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.backend = Some(backend.clone());

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	client_config.backend = Some(backend);

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	// ── run server and client concurrently ──────────────────────────
	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		// The client wired only a subscriber, so its advertised role reaches the server
		// over every transport, now that the SETUP is read before the caller authorizes
		// rather than deferred to `ok()`.
		assert_eq!(request.role(), moq_native::moq_net::Role::Subscriber);
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

	let moq_native::moq_net::announce::Update { path, event: bc, .. } =
		tokio::time::timeout(TIMEOUT, announcements.next())
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

	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");

	assert_eq!(&frame.payload[..], b"hello");

	drop(session);
	server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
}

/// Generate a CA, a server cert + key, and a client cert + key (all PEM, the
/// leaf certs signed by the CA) written to a tempdir. Returns the dir plus the
/// five paths so the caller can wire them into the TLS configs.
#[cfg(any(feature = "quinn", feature = "noq"))]
fn generate_mtls_certs() -> (tempfile::TempDir, MtlsPaths) {
	use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
	use std::io::Write;

	let dir = tempfile::tempdir().expect("failed to create tempdir");

	// Self-signed CA that signs both the server and client leaf certs.
	let ca_key = KeyPair::generate().expect("ca key");
	let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
	ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
	let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");
	let issuer = Issuer::from_params(&ca_params, &ca_key);

	// Server leaf with a localhost SAN so the client can verify the name.
	let server_key = KeyPair::generate().expect("server key");
	let server_params = CertificateParams::new(vec!["localhost".to_string()]).expect("server params");
	let server_cert = server_params.signed_by(&server_key, &issuer).expect("server cert");

	// Client leaf presented during the handshake for mTLS.
	let client_key = KeyPair::generate().expect("client key");
	let client_params = CertificateParams::new(Vec::new()).expect("client params");
	let client_cert = client_params.signed_by(&client_key, &issuer).expect("client cert");

	let write = |name: &str, contents: String| {
		let path = dir.path().join(name);
		let mut file = std::fs::File::create(&path).expect("create pem file");
		file.write_all(contents.as_bytes()).expect("write pem file");
		path
	};

	let paths = MtlsPaths {
		ca: write("ca.pem", ca_cert.pem()),
		server_cert: write("server.pem", server_cert.pem()),
		server_key: write("server.key", server_key.serialize_pem()),
		client_cert: write("client.pem", client_cert.pem()),
		client_key: write("client.key", client_key.serialize_pem()),
	};

	(dir, paths)
}

/// Filesystem paths to the PEM material produced by [`generate_mtls_certs`].
#[cfg(any(feature = "quinn", feature = "noq"))]
struct MtlsPaths {
	ca: std::path::PathBuf,
	server_cert: std::path::PathBuf,
	server_key: std::path::PathBuf,
	client_cert: std::path::PathBuf,
	client_key: std::path::PathBuf,
}

/// Connect with a client certificate signed by a CA the server trusts, and
/// assert the server observes the validated peer certificate via mTLS.
#[cfg(any(feature = "quinn", feature = "noq"))]
async fn mtls_test(scheme: &str, backend: moq_native::QuicBackend) {
	let (_dir, paths) = generate_mtls_certs();

	let pub_origin = Origin::random().produce();

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.cert = vec![paths.server_cert.clone()];
	server_config.tls.key = vec![paths.server_key.clone()];
	server_config.tls.root = vec![paths.ca.clone()];
	server_config.backend = Some(backend.clone());

	let mut server = server_config.init().expect("failed to init server");
	let addr = server.local_addr().expect("failed to get local addr");

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.root = vec![paths.ca.clone()];
	client_config.tls.system_roots = Some(false);
	client_config.tls.cert = Some(paths.client_cert.clone());
	client_config.tls.key = Some(paths.client_key.clone());
	client_config.backend = Some(backend);

	let client = client_config.init().expect("failed to init client");
	let url: url::Url = format!("{scheme}://localhost:{}", addr.port()).parse().unwrap();

	let server_handle = tokio::spawn(async move {
		let request = server.accept().await.expect("no incoming connection");
		// The peer cert must be visible before we accept the session.
		let has_cert = request.peer_identity().is_some();
		let session = request.with_publisher(pub_origin.consume()).ok().await?;
		let _ = session.closed().await;
		Ok::<_, anyhow::Error>(has_cert)
	});

	let session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("client connect timed out")
		.expect("client connect failed");

	drop(session);
	let has_cert = server_handle
		.await
		.expect("server task panicked")
		.expect("server task failed");
	assert!(has_cert, "server did not observe the client certificate");
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
async fn quinn_mtls() {
	mtls_test("https", moq_native::QuicBackend::Quinn).await;
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
#[ignore = "lite-05 TRACK stream trips a web-transport-quiche bug: dropping the TRACK \
            control stream after reading TRACK_INFO (closed early by design) surfaces as a \
            connection-level `quiche error: Done`, tearing down the whole session. lite-04 \
            (no TRACK stream) and the quinn backend both work. Re-enable when web-transport-quiche \
            handles the early stream drop, or revisit alongside the temporary lite-05 default."]
async fn quiche_webtransport() {
	backend_test("https", moq_native::QuicBackend::Quiche).await;
}

// ── Iroh backend ────────────────────────────────────────────────────

#[cfg(feature = "iroh")]
#[tracing_test::traced_test]
#[tokio::test]
async fn iroh_connect() {
	use moq_native::iroh::EndpointConfig;

	// ── publisher (server) ──────────────────────────────────────────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
	let mut track = broadcast.create_track("video", None).expect("failed to create track");

	let mut group = track.append_group().expect("failed to append group");
	group
		.write_frame(moq_native::moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("failed to write frame");
	group.finish().expect("failed to finish group");

	// Create server iroh endpoint
	let mut server_iroh_config = EndpointConfig::default();
	server_iroh_config.enabled = Some(true);
	let server_endpoint = server_iroh_config
		.bind(&moq_native::quic::Client::default())
		.await
		.expect("failed to bind server iroh endpoint")
		.expect("server iroh endpoint not enabled");

	// Get the server's direct addresses before moving it into the server.
	let server_addr = server_endpoint.addr();
	let server_addrs: Vec<std::net::SocketAddr> = server_addr.ip_addrs().copied().collect();

	let server_endpoint_id = server_endpoint.id();

	// Server still needs a QUIC bind for init, but we'll connect via iroh
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];

	let mut server = server_config
		.init()
		.expect("failed to init server")
		.with_iroh(Some(server_endpoint));

	// ── subscriber (client) ─────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	// Create client iroh endpoint
	let mut client_iroh_config = EndpointConfig::default();
	client_iroh_config.enabled = Some(true);
	let client_endpoint = client_iroh_config
		.bind(&moq_native::quic::Client::default())
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
		// The client wired only a subscriber, so its advertised role reaches the server
		// over every transport, now that the SETUP is read before the caller authorizes
		// rather than deferred to `ok()`.
		assert_eq!(request.role(), moq_native::moq_net::Role::Subscriber);
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

	let moq_native::moq_net::announce::Update { path, event: bc, .. } =
		tokio::time::timeout(TIMEOUT, announcements.next())
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

	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timed out")
		.expect("read_frame failed")
		.expect("group closed prematurely");

	assert_eq!(&frame.payload[..], b"hello");

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

#[cfg(feature = "noq")]
#[tracing_test::traced_test]
#[tokio::test]
async fn noq_mtls() {
	mtls_test("https", moq_native::QuicBackend::Noq).await;
}

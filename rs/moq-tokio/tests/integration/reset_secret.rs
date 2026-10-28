//! A server restarted with the same `listen.reset.secret` resets its predecessor's
//! connections, so a peer learns at once that they are gone instead of waiting out
//! its idle timeout.
//!
//! The predecessor "crashes" by dropping its runtime, which stops every task before
//! it can send a CONNECTION_CLOSE, and the successor binds the same port.

#![cfg(feature = "quic")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use moq_tokio::quic::driver;

const RESET_SECRET: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

struct Fixture {
	cert: rustls::pki_types::CertificateDer<'static>,
	cert_path: PathBuf,
	key_path: PathBuf,
	reset_secret: PathBuf,
	_dir: tempfile::TempDir,
}

impl Fixture {
	fn new() -> Self {
		let dir = tempfile::tempdir().expect("tempdir");
		let key = rcgen::KeyPair::generate().expect("key");
		let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
			.expect("params")
			.self_signed(&key)
			.expect("cert");
		let write = |name: &str, contents: &[u8]| {
			let path = dir.path().join(name);
			std::fs::write(&path, contents).expect("write fixture file");
			path
		};
		Self {
			cert: cert.der().clone(),
			cert_path: write("server.pem", cert.pem().as_bytes()),
			key_path: write("server.key", key.serialize_pem().as_bytes()),
			reset_secret: write("reset.key", format!("{RESET_SECRET}\n").as_bytes()),
			_dir: dir,
		}
	}

	/// Serve on `port` (0 for any) from a runtime of its own, returned with the bound port.
	fn serve(&self, port: u16, reset_secret: Option<PathBuf>) -> (tokio::runtime::Runtime, u16) {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.expect("server runtime");

		let mut config = moq_tokio::server::Config::default();
		config.listen.bind = Some(format!("127.0.0.1:{port}").parse().unwrap());
		config.listen.tls.cert = vec![self.cert_path.clone()];
		config.listen.tls.key = vec![self.key_path.clone()];
		config.listen.reset.secret = reset_secret;

		let port = runtime.block_on(async move {
			let mut listener = config
				.init()
				.expect("failed to init server")
				.listen()
				.await
				.expect("failed to listen");
			let port = listener.local_addr().expect("no quic addr").port();
			tokio::spawn(async move {
				while let Some(request) = listener.accept().await {
					tokio::spawn(async move {
						let _session = request.ok().await;
						std::future::pending::<()>().await;
					});
				}
			});
			port
		});
		(runtime, port)
	}
}

/// Connect, crash the server and restart it on the same port, then report how the
/// client's connection ended once it next sent something.
fn restart(reset_secret: bool) -> Result<driver::connection::Error, tokio::time::error::Elapsed> {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let fixture = Fixture::new();
	let client = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("client runtime");

	let file = reset_secret.then(|| fixture.reset_secret.clone());
	let (server, port) = fixture.serve(0, file.clone());
	let (_endpoint, conn) = client.block_on(async {
		let mut roots = rustls::RootCertStore::empty();
		roots.add(fixture.cert.clone()).expect("root");
		let mut tls = rustls::ClientConfig::builder()
			.with_root_certificates(roots)
			.with_no_client_auth();
		tls.alpn_protocols = vec![b"moq-lite-06".to_vec()];
		let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");

		let endpoint =
			driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
				.expect("endpoint");
		let conn = endpoint
			.connect(
				&driver::client::Config::new(Arc::new(quic)),
				([127, 0, 0, 1], port).into(),
				"localhost",
			)
			.expect("connect")
			.await
			.expect("handshake");
		(endpoint, conn)
	});

	drop(server);
	let (_server, _) = fixture.serve(port, file);

	client.block_on(async {
		let mut send = conn.open_uni(Default::default()).await.expect("open");
		send.write_all(&[0; 64]).await.expect("write");
		send.finish().expect("finish");
		// Far below the client's 30s idle timeout, and ample for a loopback round
		// trip; the keyless arm waits all of it.
		tokio::time::timeout(Duration::from_secs(1), conn.closed()).await
	})
}

#[test]
fn a_restarted_server_resets_its_predecessors_connections() {
	let closed = restart(true);
	assert!(matches!(closed, Ok(driver::connection::Error::Reset)), "{closed:?}");
}

/// The control: a random key per process drops the old connection IDs as invalid,
/// so no reset comes and the client is left to wait out its idle timeout.
#[test]
fn a_restarted_server_without_the_key_leaves_peers_waiting() {
	let closed = restart(false);
	assert!(closed.is_err(), "{closed:?}");
}

//! Over raw QUIC, a stream code reaches the peer as the protocol's own value, not
//! mapped into the HTTP/3 WebTransport code space, so other raw QUIC MoQ stacks
//! read it.
//!
//! The peer here is a plain QUIC client. A moq-lite server refuses a uni stream opened
//! ahead of the SETUP with STOP_SENDING, and the client reads that code off the wire.
//! moq-transport holds such a stream instead, but its codes take the same raw session.

#![cfg(feature = "noq")]

use std::{sync::Arc, time::Duration};

use web_transport_moq::noq;

/// moq-lite sends INTERNAL_ERROR (0x0) for a refused stream.
const REFUSED: u32 = 0x0;

struct Server {
	port: u16,
	cert: rustls::pki_types::CertificateDer<'static>,
	_dir: tempfile::TempDir,
}

async fn serve() -> Server {
	use std::io::Write;

	let dir = tempfile::tempdir().expect("tempdir");
	let key = rcgen::KeyPair::generate().expect("key");
	let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
		.expect("params")
		.self_signed(&key)
		.expect("cert");
	let write = |name: &str, contents: String| {
		let path = dir.path().join(name);
		std::fs::File::create(&path)
			.and_then(|mut file| file.write_all(contents.as_bytes()))
			.expect("write pem file");
		path
	};

	let mut config = moq_tokio::server::Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![write("server.pem", cert.pem())];
	config.listen.tls.key = vec![write("server.key", key.serialize_pem())];
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

	Server {
		port,
		cert: cert.der().clone(),
		_dir: dir,
	}
}

/// Dial the server over raw QUIC with `alpn`, open a uni stream that is not a SETUP,
/// and return the code the server stopped it with.
async fn refused_code(server: &Server, alpn: &str) -> Option<noq::VarInt> {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut roots = rustls::RootCertStore::empty();
	roots.add(server.cert.clone()).expect("root");
	let mut tls = rustls::ClientConfig::builder()
		.with_root_certificates(roots)
		.with_no_client_auth();
	tls.alpn_protocols = vec![alpn.as_bytes().to_vec()];
	let quic = noq::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");

	let endpoint = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("endpoint");
	let conn = endpoint
		.connect_with(
			noq::ClientConfig::new(Arc::new(quic)),
			([127, 0, 0, 1], server.port).into(),
			"localhost",
		)
		.expect("connect")
		.await
		.expect("handshake");

	// A group stream, which moq-lite refuses before the SETUP.
	let mut send = conn.open_uni().await.expect("open");
	send.write_all(&[0x00]).await.expect("write");

	tokio::time::timeout(Duration::from_secs(10), send.stopped())
		.await
		.expect("the stream was not stopped")
		.expect("stopped")
}

#[tokio::test]
async fn a_refused_stream_code_is_not_mapped() {
	let server = serve().await;
	assert_eq!(refused_code(&server, "moq-lite-06").await, Some(REFUSED.into()));
}

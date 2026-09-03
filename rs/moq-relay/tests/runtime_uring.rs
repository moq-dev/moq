//! The io_uring thread-per-core mode, end to end through a real relay:
//! browsers (WebTransport over `h3`) and native peers (raw QUIC) served by
//! pinned io_uring workers, with authentication and supervision on the shared
//! runtime and the frame crossing worker threads through the shared origin.
//!
//! Linux-only like the mode itself, and kernel-gated below the io_uring 6.12
//! floor (GitHub-hosted CI), where it skips loudly.
#![cfg(all(target_os = "linux", feature = "_uring"))]

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use moq_relay::{Config, PublicConfig, Relay};
use moq_tokio::moq_net::{self, Hop};

const TIMEOUT: Duration = Duration::from_secs(10);
const WORKERS: u16 = 2;

/// Whether this kernel can run the io_uring workers at all.
fn supported() -> bool {
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => true,
		Err(moq_uring::Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring relay test: {reason}");
			false
		}
		Err(err) => panic!("io_uring worker setup failed: {err}"),
	}
}

/// A UDP port nothing is bound to. Every worker binds the same port, so this
/// cannot be `:0`.
fn free_udp_port() -> u16 {
	let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

/// A CA on disk plus a certificate it signed, for the mTLS test. Returns the
/// root, the client's certificate, and its key.
fn signed_client(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
	let ca_key = rcgen::KeyPair::generate().expect("ca keypair");
	let mut ca_params = rcgen::CertificateParams::new(Vec::new()).expect("ca params");
	ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
	ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
	ca_params
		.distinguished_name
		.push(rcgen::DnType::CommonName, "moq test ca");
	let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("self-signed ca");

	let key = rcgen::KeyPair::generate().expect("client keypair");
	let mut params = rcgen::CertificateParams::new(vec!["client.localhost".to_string()]).expect("client params");
	params.use_authority_key_identifier_extension = true;
	params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
	let cert = params.signed_by(&key, &ca).expect("signed client cert");

	let root_path = dir.join("ca.pem");
	let cert_path = dir.join("client.pem");
	let key_path = dir.join("client.key.pem");
	std::fs::write(&root_path, ca.pem()).expect("write ca");
	std::fs::write(&cert_path, cert.pem()).expect("write client cert");
	std::fs::write(&key_path, key.serialize_pem()).expect("write client key");
	(root_path, cert_path, key_path)
}

/// A self-signed certificate on disk; the workers refuse `tls.generate`.
fn certificate(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
	let key = rcgen::KeyPair::generate().expect("keypair");
	let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("cert params");
	let cert = params.self_signed(&key).expect("self-signed cert");
	let cert_path = dir.join("cert.pem");
	let key_path = dir.join("key.pem");
	std::fs::write(&cert_path, cert.pem()).expect("write cert");
	std::fs::write(&key_path, key.serialize_pem()).expect("write key");
	(cert_path, key_path)
}

/// A relay config serving QUIC from io_uring workers. Pinning is off because a
/// CI container may restrict which cores it may run on.
fn uring_config(cert: &std::path::Path, key: &std::path::Path, port: u16) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some(format!("127.0.0.1:{port}"));
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.runtime.workers = Some(WORKERS);
	config.runtime.pin = Some(false);
	config.runtime.io_uring = Some(true);
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	config.auth.public = Some(public);
	config
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.once = Some(true);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

async fn connect(client: moq_tokio::Client, url: url::Url) -> moq_tokio::Connection {
	tokio::time::timeout(TIMEOUT, client.with_reconnect(false).connect(url).established())
		.await
		.expect("connect timeout")
		.expect("connect failed")
}

/// A WebTransport publisher and raw-QUIC subscribers (and vice versa on the
/// second broadcast) through io_uring workers: both peer flavors on one
/// steered socket group, with the frame crossing worker threads through the
/// shared origin.
#[tokio::test]
async fn uring_workers_serve_webtransport_and_raw_quic() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let relay = Relay::load(uring_config(&cert, &key, port)).await.expect("load relay");
	let expected: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
	assert_eq!(relay.addr, Some(expected), "workers bound a different address");

	// The stock loop serves everything: the uring workers own QUIC, the shared
	// runtime owns auth and supervision.
	let running = tokio::spawn(relay.run());

	// A WebTransport publisher (https, the browser path)...
	let wt_url: url::Url = format!("https://127.0.0.1:{port}/uring").parse().expect("parse url");
	// ...and a raw-QUIC subscriber (moql, the native path).
	let raw_url: url::Url = format!("moql://127.0.0.1:{port}/uring").parse().expect("parse url");

	let origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = origin.create_broadcast("test").expect("create broadcast");
	let _announce_broadcast = origin.announce("test", Default::default()).expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let publisher = connect(client().with_publisher(&origin), wt_url.clone()).await;

	// Several subscribers of each flavor, spread over the steered workers.
	let mut subscribers = Vec::new();
	for url in [&wt_url, &raw_url, &wt_url, &raw_url] {
		let origin = moq_tokio::origin::spawn(Hop::random());
		let consumer = origin.consume();
		let announced = consumer.announced();
		let connection = connect(client().with_subscriber(origin), url.clone()).await;
		subscribers.push((connection, consumer, announced));
	}

	for (index, (_connection, consumer, announced)) in subscribers.iter_mut().enumerate() {
		let update = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} announcement timeout"))
			.expect("origin closed");
		assert_eq!(update.prefix.as_path().as_str(), "test");
		assert!(update.active, "expected announce, got retraction");
		let broadcast = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("test"))
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} request timeout"))
			.expect("announced broadcast resolves");

		let mut subscription = broadcast
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = tokio::time::timeout(TIMEOUT, subscription.recv_group())
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} recv_group timeout"))
			.expect("recv_group failed")
			.expect("track closed prematurely");
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} read_frame timeout"))
			.expect("read_frame failed")
			.expect("group closed prematurely");
		assert_eq!(&frame.payload[..], b"hello");
	}

	assert!(!running.is_finished(), "the relay stopped while serving");

	drop(track);
	drop(broadcast);
	drop(publisher);
	drop(subscribers);
	// Dropping the run task drops the worker group, which joins its threads.
	running.abort();
	let _ = running.await;
}

/// One HTTP/1.1 GET against `addr`, returning the response body.
///
/// Hand-rolled because the relay has no HTTP client among its dev
/// dependencies, and one GET does not justify pulling one in.
async fn http_get(addr: SocketAddr, path: &str) -> String {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
	let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
	stream.write_all(request.as_bytes()).await.expect("write request");
	let mut response = String::new();
	stream.read_to_string(&mut response).await.expect("read response");
	let (head, body) = response.split_once("\r\n\r\n").expect("response has a body");
	assert!(head.starts_with("HTTP/1.1 200"), "got {head:?}");
	body.to_string()
}

/// `/certificate.sha256` serves what the io_uring workers are actually
/// presenting.
///
/// The fingerprints used to come from the shared server's TLS backend, which
/// is stream-only in this mode, so the endpoint 404'd while the workers served
/// a certificate. A client that pins a self-signed relay through it stopped
/// being able to connect the moment the flag was flipped.
#[tokio::test]
async fn uring_workers_publish_their_certificate_fingerprint() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let relay = Relay::load(uring_config(&cert, &key, free_udp_port()))
		.await
		.expect("load relay");

	// The relay's own web listener needs TLS; its router does not, and the
	// handler reads the same certificate handle either way.
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
	let addr = listener.local_addr().expect("local addr");
	let routes = relay.web.routes();
	let serving = tokio::spawn(async move { axum::serve(listener, routes).await });

	let served = http_get(addr, "/certificate.sha256").await;

	// The same value a client pins: SHA-256 over the leaf's DER.
	let pem = std::fs::read(&cert).expect("read cert");
	let expected = moq_tokio::tls::Certificates::from_pem(&pem)
		.expect("fingerprint")
		.fingerprints()
		.remove(0);
	assert_eq!(served, expected, "the fingerprint the workers serve");

	serving.abort();
	let _ = serving.await;
}

/// A client certificate is a credential the io_uring workers accept.
///
/// `listen.tls.root` used to be refused at startup here, so the mode could not
/// authenticate a peer mesh at all. There is deliberately no JWT or public path
/// configured below, so `Auth::verify` refuses every one of these connections:
/// only the mTLS path can carry the round trip through.
#[tokio::test]
async fn an_mtls_client_authenticates_without_a_token() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let (root, client_cert, client_key) = signed_client(dir.path());
	let port = free_udp_port();

	let mut config = uring_config(&cert, &key, port);
	config.auth.public = None;
	config.listen.tls.root = vec![root];

	let relay = Relay::load(config).await.expect("load relay");
	let running = tokio::spawn(relay.run());

	let client = || {
		let mut dial = moq_tokio::connect::Config::default();
		dial.tls.insecure = Some(true);
		dial.once = Some(true);
		dial.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		dial.tls.cert = Some(client_cert.clone());
		dial.tls.key = Some(client_key.clone());
		dial.init(Default::default()).expect("client init")
	};

	// An mTLS token is unrestricted within its root, so one certificate covers
	// both roles. A publish that reaches a subscriber is the proof: an
	// unauthorized session establishes and is then closed, so merely
	// connecting proves nothing.
	let url: url::Url = format!("moql://127.0.0.1:{port}/mtls").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = origin.create_broadcast("test").expect("create broadcast");
	let _announce_broadcast = origin.announce("test", Default::default()).expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");
	let publisher = connect(client().with_publisher(&origin), url.clone()).await;

	let subscriber_origin = moq_tokio::origin::spawn(Hop::random());
	let consumer = subscriber_origin.consume();
	let mut announced = consumer.announced();
	let subscriber = connect(client().with_subscriber(subscriber_origin), url).await;

	let update = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_path().as_str(), "test");
	assert!(update.active, "expected announce, got retraction");
	let announced = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("test"))
		.await
		.expect("request timeout")
		.expect("announced broadcast resolves");
	let mut subscription = announced
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("subscribe");
	let mut group = tokio::time::timeout(TIMEOUT, subscription.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&frame.payload[..], b"hello");

	assert!(!running.is_finished(), "the relay stopped while serving");
	drop(track);
	drop(broadcast);
	drop(publisher);
	drop(subscriber);
	running.abort();
	let _ = running.await;
}

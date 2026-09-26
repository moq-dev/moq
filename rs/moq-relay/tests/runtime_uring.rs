//! The io_uring thread-per-core mode, end to end through a real relay:
//! browsers (WebTransport over `h3`) and native peers (raw QUIC) served by
//! pinned io_uring workers, with authentication and supervision on the shared
//! runtime and the frame crossing worker threads through the shared origin.
//!
//! Linux-only like the mode itself, and kernel-gated below the io_uring 6.12
//! floor (GitHub-hosted CI), where it skips loudly.
#![cfg(all(target_os = "linux", feature = "_uring"))]

use std::net::SocketAddr;
use std::time::Duration;

use moq_relay::{Config, Relay};
use moq_tokio::moq_net;

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
fn uring_config(cert: &std::path::Path, key: &std::path::Path) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.runtime.workers = Some(WORKERS);
	config.runtime.pin = false;
	config.runtime.io_uring = true;
	config.auth.public = vec![moq_auth::Pattern::all()];
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
	let relay = Relay::load(uring_config(&cert, &key)).await.expect("load relay");
	let port = relay.addr().expect("workers bound an address").port();

	// The stock loop serves everything: the uring workers own QUIC, the shared
	// runtime owns auth and supervision.
	let running = tokio::spawn(relay.run());

	// A WebTransport publisher (https, the browser path)...
	let wt_url: url::Url = format!("https://127.0.0.1:{port}/uring").parse().expect("parse url");
	// ...and a raw-QUIC subscriber (moql, the native path).
	let raw_url: url::Url = format!("moql://127.0.0.1:{port}/uring").parse().expect("parse url");

	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let publisher = connect(client().with_publisher(&origin), wt_url.clone()).await;

	// Several subscribers of each flavor, spread over the steered workers.
	let mut subscribers = Vec::new();
	for url in [&wt_url, &raw_url, &wt_url, &raw_url] {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let announced = consumer.announced();
		let connection = connect(client().with_subscriber(origin), url.clone()).await;
		subscribers.push((connection, consumer, announced));
	}

	for (index, (_connection, consumer, announced)) in subscribers.iter_mut().enumerate() {
		let (update, active) = tokio::time::timeout(TIMEOUT, next_update(announced))
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} announcement timeout"))
			.expect("origin closed");
		assert_eq!(update.prefix.as_str(), "test");
		assert!(active, "expected announce, got retraction");
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

/// A session the workers accept reports its peer address, the listener's
/// address, and the name the client dialed, as the tokio listener does: the
/// SNI on raw QUIC and WebTransport alike.
#[tokio::test]
async fn uring_workers_report_link_facts() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let relay = Relay::load(uring_config(&cert, &key)).await.expect("load relay");
	let local = relay.addr().expect("bound address");
	let port = local.port();
	let sessions = relay.sessions().clone();
	let running = tokio::spawn(relay.run());

	let mut connections = Vec::new();
	for scheme in ["moql", "https"] {
		let url: url::Url = format!("{scheme}://localhost:{port}/link").parse().expect("parse url");
		connections.push(connect(client(), url).await);
	}

	// A client can see its session established before the relay lists it.
	let deadline = std::time::Instant::now() + TIMEOUT;
	let views = loop {
		let views = sessions.list(&Default::default());
		if views.len() == 2 {
			break views;
		}
		assert!(std::time::Instant::now() < deadline, "expected 2 sessions: {views:?}");
		tokio::time::sleep(Duration::from_millis(25)).await;
	};
	for view in &views {
		let remote = view.remote.expect("peer address");
		assert!(remote.ip().is_loopback() && remote.port() != 0, "peer address {remote}");
		assert_ne!(remote, local, "the peer is not the listener");
		assert_eq!(view.local, Some(local), "listener address");
		assert_eq!(view.server_name.as_deref(), Some("localhost"), "dialed name");
	}

	drop(connections);
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
	let relay = Relay::load(uring_config(&cert, &key)).await.expect("load relay");

	// The relay's own web listener needs TLS; its router does not, and the
	// handler reads the same certificate handle either way.
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
	let addr = listener.local_addr().expect("local addr");
	let routes = relay.web().routes();
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

/// A client certificate the io_uring workers verified is reported to the auth
/// server, whose mTLS grant admits it.
///
/// `listen.tls.root` used to be refused at startup here, so the mode could not
/// authenticate a peer mesh at all. The server below grants certificates and
/// nothing else, so only the mTLS path can carry the round trip through.
#[tokio::test]
async fn an_mtls_client_authenticates_without_a_token() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let (root, client_cert, client_key) = signed_client(dir.path());
	let mut config = uring_config(&cert, &key);
	config.auth.public = Vec::new();
	config.auth.url = Some(spawn_auth_server(mtls_only()).await);
	config.listen.tls.root = vec![root];

	let relay = Relay::load(config).await.expect("load relay");
	let port = relay.addr().expect("workers bound an address").port();
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

	// The server grants a certificate everything, so one certificate covers
	// both roles. A publish that reaches a subscriber is the proof: an
	// unauthorized session establishes and is then closed, so merely
	// connecting proves nothing.
	let url: url::Url = format!("moql://127.0.0.1:{port}/mtls").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");
	let publisher = connect(client().with_publisher(&origin), url.clone()).await;

	let subscriber_origin = moq_tokio::origin::spawn();
	let consumer = subscriber_origin.consume();
	let mut announced = consumer.announced();
	let subscriber = connect(client().with_subscriber(subscriber_origin), url).await;

	let (update, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(active, "expected announce, got retraction");
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

/// `quic.qlog` captures traces from the io_uring workers, the same as it does
/// from the tokio ones.
///
/// A real session keeps both trace writers active through worker shutdown, so
/// the test covers capture on the io_uring data path and the final flush.
#[cfg(feature = "qlog")]
#[tokio::test]
async fn uring_workers_write_qlog_traces() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let traces = dir.path().join("qlog");
	std::fs::create_dir(&traces).expect("create qlog dir");
	let mut config = uring_config(&cert, &key);
	config.quic.qlog = Some(traces.clone());
	let relay = Relay::load(config).await.expect("load relay");
	let port = relay.addr().expect("workers bound an address").port();
	let running = tokio::spawn(relay.run());

	// A real session, so a trace covers a handshake and application data
	// rather than a connection that only ever exchanged Initials.
	let url: url::Url = format!("moql://127.0.0.1:{port}/qlog").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");
	let publisher = connect(client().with_publisher(&origin), url.clone()).await;

	let subscriber_origin = moq_tokio::origin::spawn();
	let consumer = subscriber_origin.consume();
	let mut announced = consumer.announced();
	let subscriber = connect(client().with_subscriber(subscriber_origin), url).await;
	let (_, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert!(active, "expected announce, got retraction");

	assert!(!running.is_finished(), "the relay stopped while serving");
	drop(track);
	drop(broadcast);
	drop(publisher);
	drop(subscriber);
	// Dropping the relay drops the worker group and with it the qlog sink,
	// which flushes every trace before its writer thread joins.
	running.abort();
	let _ = running.await;

	let written = tokio::time::timeout(TIMEOUT, async {
		loop {
			let files: Vec<_> = std::fs::read_dir(&traces)
				.expect("read qlog dir")
				.map(|entry| entry.expect("dir entry").path())
				.filter(|path| path.metadata().is_ok_and(|meta| meta.len() > 0))
				.collect();
			if !files.is_empty() {
				return files;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	})
	.await
	.unwrap_or_else(|_| panic!("no qlog traces in {}", traces.display()));

	// Every record has to be JSON on a line of its own, or the trace is not
	// something a qlog reader can open.
	for path in written {
		let raw = std::fs::read_to_string(&path).expect("read trace");
		let records: Vec<&str> = raw
			.split('\n')
			.map(|line| line.trim_matches(|c: char| c == '\u{1e}' || c.is_whitespace()))
			.filter(|line| !line.is_empty())
			.collect();
		assert!(!records.is_empty(), "{} holds no records", path.display());
		for record in records {
			serde_json::from_str::<serde_json::Value>(record)
				.unwrap_or_else(|err| panic!("{}: {err}: {record}", path.display()));
		}
	}
}

/// A policy admitting verified certificates and nobody else.
fn mtls_only() -> moq_auth::serve::Policy {
	let mut policy = moq_auth::serve::Policy::default();
	policy.mtls = moq_auth::Permissions::new(
		[moq_auth::Pattern::all()].into_iter().collect(),
		[moq_auth::Pattern::all()].into_iter().collect(),
	);
	policy
}

/// Serve `policy` on a loopback port for the test's lifetime, returning its URL.
async fn spawn_auth_server(policy: moq_auth::serve::Policy) -> url::Url {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("bind auth server");
	let url = format!("http://{}/", listener.local_addr().expect("auth addr"))
		.parse()
		.expect("auth url");
	let server = moq_auth::serve::Server::new(policy);
	tokio::spawn(async move { server.serve(listener).await });
	url
}

/// The next route and whether it is active, skipping the caught-up marker.
async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
	loop {
		return match announced.next().await? {
			moq_net::announce::Event::Announced(route) | moq_net::announce::Event::Updated(route) => {
				Some((route, true))
			}
			moq_net::announce::Event::Retracted(route) => Some((route, false)),
			moq_net::announce::Event::Live => continue,
		};
	}
}

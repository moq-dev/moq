//! The io_uring thread-per-core mode, end to end through a real relay:
//! browsers (WebTransport over `h3`) and native peers (raw QUIC) served by
//! pinned io_uring workers, with authentication and supervision on the shared
//! runtime and the frame crossing worker threads through the shared origin.
//!
//! Linux-only like the mode itself. Unsupported local kernels skip loudly;
//! CI_STRICT makes CI refuse them.
#![cfg(all(target_os = "linux", feature = "_uring"))]

use std::net::SocketAddr;
use std::time::Duration;

use moq_relay::{Config, Relay};
use moq_tokio::moq_net;

use crate::logs::Logs;

const TIMEOUT: Duration = Duration::from_secs(10);
const WORKERS: u16 = 2;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Whether this kernel can run the io_uring workers at all.
fn supported() -> bool {
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => true,
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
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

/// A self-signed certificate on disk.
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
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
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
	let relay = Relay::load(uring_config(&cert, &key))
		.await
		.expect("load relay")
		.with_signals(false);
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
	let track = broadcast.create_track("video", timed()).expect("create track");
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
		let broadcast = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("test", None))
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

/// The workers serve every version the listener lists, not only moq-lite on
/// its own ALPN: moq-transport, both on its own ALPN and over the bidi SETUP
/// exchange, and the SETUP-negotiated moq-lite versions. A moq-transport
/// publisher feeds a subscriber per version over raw QUIC and WebTransport,
/// each of which must negotiate exactly the version it was pinned to.
#[tokio::test]
async fn uring_workers_serve_every_configured_version() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let pinned = |version: &str| {
		let version: moq_net::Version = version.parse().expect("known version");
		let mut config = moq_tokio::connect::Config::default();
		config.tls.insecure = Some(true);
		config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		config.version = vec![version];
		(
			version,
			config
				.init(Default::default())
				.expect("client init")
				.with_reconnect(false),
		)
	};

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let relay = Relay::load(uring_config(&cert, &key))
		.await
		.expect("load relay")
		.with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	let running = tokio::spawn(relay.run());

	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let raw = format!("moql://127.0.0.1:{port}/versions");
	let (version, client) = pinned("moq-transport-22");
	let publisher = connect(client.with_publisher(&origin), raw.parse().unwrap()).await;
	assert_eq!(publisher.version(), Some(version), "publisher version");

	// moq-lite-02 negotiates in the SETUP; moq-transport-22 reads its SETUP off a
	// uni stream.
	for name in ["moq-transport-22", "moq-lite-02"] {
		for scheme in ["moql", "https"] {
			let url: url::Url = format!("{scheme}://127.0.0.1:{port}/versions").parse().unwrap();
			let (version, client) = pinned(name);
			let origin = moq_tokio::origin::spawn();
			let consumer = origin.consume();
			let mut announced = consumer.announced();
			let connection = connect(client.with_subscriber(origin), url).await;
			assert_eq!(connection.version(), Some(version), "{scheme} {name}");

			let (update, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
				.await
				.unwrap_or_else(|_| panic!("{scheme} {name}: announcement timeout"))
				.expect("origin closed");
			// moq-lite-02 has no in-band path, so a raw QUIC session sits at the root.
			let path = update.prefix.as_str().to_owned();
			assert!(active && path.ends_with("test"), "{scheme} {name}: announced {path}");
			let broadcast = tokio::time::timeout(TIMEOUT, consumer.request_broadcast(&path, None))
				.await
				.unwrap_or_else(|_| panic!("{scheme} {name}: request timeout"))
				.expect("announced broadcast resolves");
			let mut subscription = broadcast
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.expect("subscribe");
			let mut group = tokio::time::timeout(TIMEOUT, subscription.recv_group())
				.await
				.unwrap_or_else(|_| panic!("{scheme} {name}: recv_group timeout"))
				.expect("recv_group failed")
				.expect("track closed prematurely");
			let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{scheme} {name}: read_frame timeout"))
				.expect("read_frame failed")
				.expect("group closed prematurely");
			assert_eq!(&frame.payload[..], b"hello", "{scheme} {name}");
		}
	}

	assert!(!running.is_finished(), "the relay stopped while serving");
	drop(publisher);
	running.abort();
	let _ = running.await;
}

/// A session the workers accept reports its peer address, the listener's
/// address, and the name the client dialed, as the tokio listener does: the
/// SNI on raw QUIC and WebTransport alike, and none for an IP-literal dial, which
/// sends no SNI, rather than the WebTransport CONNECT host.
#[tokio::test]
async fn uring_workers_report_link_facts() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let relay = Relay::load(uring_config(&cert, &key))
		.await
		.expect("load relay")
		.with_signals(false);
	let local = relay.addr().expect("bound address");
	let port = local.port();
	let sessions = relay.sessions().clone();
	let running = tokio::spawn(relay.run());

	let mut connections = Vec::new();
	for scheme in ["moql", "https"] {
		for host in ["localhost", "127.0.0.1"] {
			let url: url::Url = format!("{scheme}://{host}:{port}/link/{host}")
				.parse()
				.expect("parse url");
			connections.push(connect(client(), url).await);
		}
	}

	// A client can see its session established before the relay lists it.
	let deadline = std::time::Instant::now() + TIMEOUT;
	let views = loop {
		let views = sessions.list(&Default::default());
		if views.len() == 4 {
			break views;
		}
		assert!(std::time::Instant::now() < deadline, "expected 4 sessions: {views:?}");
		tokio::time::sleep(Duration::from_millis(25)).await;
	};
	for view in &views {
		let remote = view.remote.expect("peer address");
		assert!(remote.ip().is_loopback() && remote.port() != 0, "peer address {remote}");
		assert_ne!(remote, local, "the peer is not the listener");
		assert_eq!(view.local, Some(local), "listener address");
		let sni = view.path.ends_with("/localhost").then_some("localhost");
		assert_eq!(view.server_name.as_deref(), sni, "dialed name of {}", view.path);
	}

	drop(connections);
	running.abort();
	let _ = running.await;
}

/// The shutdown trigger drains sessions the io_uring workers serve as it does
/// the shared runtime's: an established session and one arriving mid-drain
/// are each sent a GOAWAY and leave, and `run` then returns with the worker
/// threads joined and the port free.
///
/// What an arrival is told is left of the window only reaches the wire on
/// moq-transport, which this test does not dial; `shutdown_signal.rs`
/// reads it through the shared runtime, whose supervision this path shares.
#[tokio::test]
async fn uring_workers_drain_on_the_trigger() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	const DRAIN: Duration = Duration::from_secs(2);

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	let mut config = uring_config(&cert, &key);
	config.drain_timeout = Some((DRAIN).into());
	config.auth.public.clear();
	let mut relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	let trigger = relay.shutdown_trigger().clone();
	let mut admissions = relay.admissions().expect("embedded admission");
	let running = tokio::spawn(relay.run());

	// One-shot (see `client`), so a session leaves on its GOAWAY rather than
	// migrating, and closing cleanly shows it was one.
	let client = client();
	let url: url::Url = format!("moql://127.0.0.1:{port}/drain").parse().expect("parse url");

	let connecting = tokio::spawn(connect(client.clone(), url.clone()));
	let first = admissions.next().await.expect("first admission");
	let all = moq_auth::Pattern::all;
	let (_first_lease, first_consumer) = moq_auth::lease::Producer::new(moq_auth::Grant::new(all(), all()));
	first.grant(first_consumer);
	let established = connecting.await.expect("client task");

	// Hold the next real connection at authentication. Its admission keeps the
	// drain open even after the first session leaves, without an unrelated peer.
	let arriving = tokio::spawn(connect(client, url));
	let pending = admissions.next().await.expect("pending admission");
	trigger.start();
	let draining = established.draining().expect("connected");
	let goaway = tokio::time::timeout(TIMEOUT, moq_net::kio::wait(|waiter| draining.poll(waiter)))
		.await
		.expect("no GOAWAY after the trigger")
		.expect("session closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");
	tokio::time::timeout(TIMEOUT, established.closed())
		.await
		.expect("the drained session never closed")
		.expect("a one-shot session leaves cleanly on GOAWAY");

	let (_arrival_lease, arrival_consumer) = moq_auth::lease::Producer::new(moq_auth::Grant::new(all(), all()));
	pending.grant(arrival_consumer);
	let arrival = arriving.await.expect("arrival task");
	let draining = arrival.draining().expect("connected");
	let goaway = tokio::time::timeout(
		Duration::from_secs(1),
		moq_net::kio::wait(|waiter| draining.poll(waiter)),
	)
	.await
	.expect("an arrival mid-drain was not sent a GOAWAY")
	.expect("arrival closed without a GOAWAY");
	assert_eq!(goaway.uri(), "", "expected a reconnect-to-me GOAWAY");
	tokio::time::timeout(TIMEOUT, arrival.closed())
		.await
		.expect("the arrival never closed")
		.expect("a one-shot session leaves cleanly on GOAWAY");

	// `run` joins the worker threads before returning, so returning at all is
	// the clean join; the rebind proves every worker let go of the port.
	tokio::time::timeout(TIMEOUT, running)
		.await
		.expect("run did not return after the drain window")
		.expect("run panicked")
		.expect("run returned an error after the drain");
	std::net::UdpSocket::bind(("127.0.0.1", port)).expect("a worker still holds the QUIC port");
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

/// The advertised fingerprint pins fresh handshakes before and after rotation.
#[tokio::test]
async fn workers_publish_their_rotated_certificate_fingerprint() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	for io_uring in [false, true] {
		if io_uring && !supported() {
			continue;
		}
		let dir = tempfile::tempdir().expect("tempdir");
		let (cert, key) = certificate(dir.path());
		let mut config = uring_config(&cert, &key);
		config.runtime.io_uring = io_uring;
		let relay = Relay::load(config).await.expect("load relay").with_signals(false);
		let port = relay.addr().expect("worker address").port();
		let trigger = relay.shutdown_trigger().clone();
		// The router reads the same certificate handle as the HTTPS listener.
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
		let addr = listener.local_addr().expect("local addr");
		let routes = relay.web().routes();
		let serving = tokio::spawn(async move { axum::serve(listener, routes).await });
		let running = tokio::spawn(relay.run());
		let result = tokio::time::timeout(TIMEOUT, async {
			for rotated in [false, true] {
				if rotated {
					let replacement = tempfile::tempdir_in(dir.path()).expect("replacement directory");
					let (next_cert, next_key) = certificate(replacement.path());
					std::fs::rename(next_cert, &cert).expect("replace certificate");
					std::fs::rename(next_key, &key).expect("replace key");
				}
				let pem = std::fs::read(&cert).expect("read certificate");
				let expected = moq_tokio::tls::Certificates::from_pem(&pem)
					.expect("fingerprint")
					.fingerprints()
					.remove(0);
				while http_get(addr, "/certificate.sha256").await != expected {
					tokio::task::yield_now().await;
				}
				for scheme in ["moql", "https"] {
					let mut dial = moq_tokio::connect::Config::default();
					dial.tls.fingerprint = vec![expected.clone()];
					dial.bind = Some("127.0.0.1:0".parse().unwrap());
					let client = dial
						.init(Default::default())
						.expect("pinned client")
						.with_reconnect(false);
					let url = format!("{scheme}://localhost:{port}/rotation").parse().unwrap();
					let connection = connect(client, url).await;
					drop(connection);
				}
			}
		})
		.await;
		trigger.start();
		tokio::time::timeout(TIMEOUT, running)
			.await
			.expect("relay shutdown")
			.expect("relay task")
			.expect("relay run");
		serving.abort();
		let _ = serving.await;
		result.expect("rotation and fresh pinned handshakes");
	}
}

/// A generated worker identity is advertised once and accepted by pinned clients.
#[tokio::test]
async fn workers_share_a_generated_identity() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	for io_uring in [false, true] {
		if io_uring && !supported() {
			continue;
		}
		let mut config = Config::default();
		config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		config.listen.tls.generate = vec!["localhost".into()];
		config.runtime.workers = Some(WORKERS);
		config.runtime.pin = false;
		config.runtime.io_uring = io_uring;
		config.auth.public = vec![moq_auth::Pattern::all()];
		let relay = Relay::load(config)
			.await
			.expect("generated worker identity")
			.with_signals(false);
		let port = relay.addr().unwrap().port();
		let trigger = relay.shutdown_trigger().clone();
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let routes = relay.web().routes();
		let serving = tokio::spawn(async move { axum::serve(listener, routes).await });
		let running = tokio::spawn(relay.run());
		let fingerprint = http_get(addr, "/certificate.sha256").await;
		let mut dial = moq_tokio::connect::Config::default();
		dial.tls.fingerprint = vec![fingerprint];
		dial.bind = Some("127.0.0.1:0".parse().unwrap());
		let url = format!("https://localhost:{port}/generated").parse().unwrap();
		let connection = connect(dial.init(Default::default()).unwrap().with_reconnect(false), url).await;
		drop(connection);
		trigger.start();
		tokio::time::timeout(TIMEOUT, running)
			.await
			.expect("relay shutdown")
			.expect("relay task")
			.expect("relay run");
		serving.abort();
		let _ = serving.await;
	}
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

	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	let running = tokio::spawn(relay.run());

	let client = || {
		let mut dial = moq_tokio::connect::Config::default();
		dial.tls.insecure = Some(true);
		dial.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		dial.tls.cert = Some(client_cert.clone());
		dial.tls.key = Some(client_key.clone());
		dial.init(Default::default())
			.expect("client init")
			.with_reconnect(false)
	};

	// The server grants a certificate everything, so one certificate covers
	// both roles. A publish that reaches a subscriber is the proof: an
	// unauthorized session establishes and is then closed, so merely
	// connecting proves nothing.
	let url: url::Url = format!("moql://127.0.0.1:{port}/mtls").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");
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
	let announced = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("test", None))
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
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	let running = tokio::spawn(relay.run());

	// A real session, so a trace covers a handshake and application data
	// rather than a connection that only ever exchanged Initials.
	let url: url::Url = format!("moql://127.0.0.1:{port}/qlog").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");
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

/// A session the workers refuse reaches `/metrics` like one the shared runtime
/// refuses, over WebTransport and raw QUIC alike.
#[tokio::test]
async fn uring_refusals_reach_metrics() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut config = uring_config(&cert, &key);
	config.auth.public = vec!["anon/**".parse().unwrap()];
	config.internal.listen = Some("127.0.0.1:0".parse().unwrap());
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	let internal = relay.internal().addr().expect("internal listener bound");
	let running = tokio::spawn(relay.run());

	// `/rooms` is outside the public rules. A client may finish connecting before
	// the verdict lands, so a session that closes promptly counts as refused.
	// Scraped after each dial, so a miscount names the transport that made it.
	for (want, url) in [
		(1, format!("https://127.0.0.1:{port}/rooms")),
		(2, format!("moql://127.0.0.1:{port}/rooms")),
	] {
		let url: url::Url = url.parse().expect("parse url");
		let connected = tokio::time::timeout(
			TIMEOUT,
			client().with_reconnect(false).connect(url.clone()).established(),
		)
		.await
		.expect("connect timeout");
		if let Ok(connection) = connected {
			let _ = tokio::time::timeout(TIMEOUT, connection.closed())
				.await
				.expect("the workers kept a session they should refuse");
		}

		let body = reqwest::get(format!("http://{internal}/metrics"))
			.await
			.expect("scrape")
			.text()
			.await
			.expect("metrics body");
		assert!(
			body.contains(&format!(
				"moq_relay_sessions_refused_total{{reason=\"refused\"}} {want}\n"
			)),
			"after {url}:\n{body}"
		);
	}

	running.abort();
	let _ = running.await;
}

/// A policy admitting verified certificates and nobody else.
fn mtls_only() -> moq_auth::serve::Policy {
	let mut policy = moq_auth::serve::Policy::default();
	policy.mtls = moq_auth::Permissions::new(moq_auth::Pattern::all(), moq_auth::Pattern::all());
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
	let server = moq_auth::serve::Server::new(policy).unwrap();
	tokio::spawn(async move { server.serve(listener).await });
	url
}

/// The next route and whether it is active.
async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
	match announced.next().await? {
		moq_net::announce::Event::Start(route)
		| moq_net::announce::Event::Update(route)
		| moq_net::announce::Event::Restart(route) => Some((route, true)),
		moq_net::announce::Event::End(route) => Some((route, false)),
	}
}

/// A moq-transport SETUP credential reaches the decider on both ring transports.
#[cfg(feature = "quic")]
#[tokio::test]
async fn uring_forwards_the_setup_token() {
	use moq_tokio::quic::{driver, web};

	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let mut config = uring_config(&cert, &key);
	config.auth.public.clear();
	let mut relay = Relay::load(config).await.unwrap().with_signals(false);
	let addr = relay.addr().unwrap();
	let mut admissions = relay.admissions().expect("embedded auth");
	let token = moq_auth::Token {
		kind: moq_auth::Token::OUT_OF_BAND,
		value: vec![0x00, 0xff, 0x80, b'x'],
	};
	let expected = token.clone();
	let deciding = tokio::spawn(async move {
		for transport in [moq_auth::Transport::Quic, moq_auth::Transport::WebTransport] {
			let admission = tokio::time::timeout(TIMEOUT, admissions.next()).await.unwrap().unwrap();
			assert_eq!(admission.request.token, Some(expected.clone()));
			assert_eq!(admission.request.transport, transport);
			if transport == moq_auth::Transport::WebTransport {
				assert_eq!(admission.request.path, "/room");
				assert_eq!(admission.request.query.as_deref(), Some("source=setup"));
			} else {
				assert_eq!(admission.request.path, "/");
				assert!(admission.request.query.is_none());
			}
			let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
			admission.grant(moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(all.clone(), all)));
		}
	});
	let running = tokio::spawn(relay.run());
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	let tls = config.tls.build().unwrap();
	let client =
		driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap();
	for webtransport in [false, true] {
		let mut tls = tls.clone();
		tls.alpn_protocols = vec![if webtransport {
			b"h3".to_vec()
		} else {
			b"moqt-18".to_vec()
		}];
		let crypto = driver::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
		let conn = client
			.connect(
				&driver::client::Config::new(std::sync::Arc::new(crypto)),
				addr,
				"localhost",
			)
			.unwrap()
			.await
			.unwrap();
		let session = if webtransport {
			let url: url::Url = format!("https://{addr}/room?source=setup").parse().unwrap();
			let request = web::proto::ConnectRequest::new(url).with_protocol("moqt-18".to_string());
			web::Session::connect(conn, request).await.unwrap()
		} else {
			web::Session::raw(conn)
		};
		// Draft-18 SETUP (0x2F00): AUTHORIZATION TOKEN (3), USE_VALUE (3), Token Type 0.
		let encoded = [&[0x03, 0x00][..], &token.value].concat();
		let params = [&[0x03, encoded.len() as u8][..], &encoded].concat();
		let setup = [&[0xaf, 0x00][..], &(params.len() as u16).to_be_bytes(), &params].concat();
		let mut send = session.open_uni(Default::default()).await.unwrap();
		send.write_all(&setup).await.unwrap();
		let mut recv = tokio::time::timeout(TIMEOUT, session.accept_uni())
			.await
			.unwrap()
			.unwrap();
		let mut reply = [0u8; 1];
		assert_eq!(
			tokio::time::timeout(TIMEOUT, recv.read(&mut reply))
				.await
				.unwrap()
				.unwrap(),
			Some(1),
			"SETUP"
		);
	}
	deciding.await.unwrap();
	client.close(driver::VarInt::from_u32(0), "test complete").await;
	running.abort();
	let _ = running.await;
}

/// The embedded auth decider receives the same transport facts as the Tokio path.
#[tokio::test]
async fn uring_auth_reports_distinct_quic_and_webtransport() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let mut config = uring_config(&cert, &key);
	config.auth.public.clear();
	let mut relay = Relay::load(config).await.unwrap().with_signals(false);
	let port = relay.addr().unwrap().port();
	let mut admissions = relay.admissions().expect("embedded auth");
	let deciding = tokio::spawn(async move {
		let mut leases = Vec::new();
		for expected in ["quic", "webtransport"] {
			let admission = tokio::time::timeout(TIMEOUT, admissions.next()).await.unwrap().unwrap();
			let actual = admission.request.transport.as_str();
			assert!(
				admission
					.request
					.alpn
					.as_ref()
					.is_some_and(|alpn| alpn.starts_with("moq-lite-"))
			);
			let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
			let grant = moq_auth::Grant::new(all.clone(), all);
			let (producer, consumer) = moq_auth::lease::Producer::new(grant);
			leases.push(producer);
			admission.grant(consumer);
			assert_eq!(actual, expected);
		}
		leases
	});
	let running = tokio::spawn(relay.run());
	let mut connections = Vec::new();
	for scheme in ["moql", "https"] {
		let url = format!("{scheme}://127.0.0.1:{port}/transport").parse().unwrap();
		connections.push(connect(client(), url).await);
	}
	let _leases = deciding.await.unwrap();
	drop(connections);
	running.abort();
	let _ = running.await;
}

/// `listen.extensions` reaches the workers' SETUP, as it does the tokio listener's.
/// A moq-transport client's connection token reaches the decider while the relay
/// offers MoQ Auth, and is refused as unsupported once it is turned off, since no
/// request would carry it.
#[tokio::test]
async fn uring_workers_offer_the_configured_extensions() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
	let grant = || moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(all.clone(), all.clone()));
	for offered in [true, false] {
		let mut config = uring_config(&cert, &key);
		config.auth.public.clear();
		config.listen.extensions.auth = offered;
		let mut relay = Relay::load(config).await.unwrap().with_signals(false);
		let port = relay.addr().unwrap().port();
		let mut admissions = relay.admissions().expect("embedded auth");
		let running = tokio::spawn(relay.run());

		let mut dial = moq_tokio::connect::Config::default();
		dial.tls.insecure = Some(true);
		dial.bind = Some("127.0.0.1:0".parse().unwrap());
		dial.version = vec!["moq-transport-22".parse().unwrap()];
		let client = dial.init(Default::default()).unwrap();
		let url = format!("moql://127.0.0.1:{port}/extensions").parse().unwrap();
		let (connection, ()) = tokio::join!(connect(client, url), async {
			let admission = tokio::time::timeout(TIMEOUT, admissions.next()).await.unwrap().unwrap();
			assert!(!admission.attached(), "offered {offered}: the session arrived attached");
			admission.grant(grant());
		});

		let auth = connection.auth();
		let presented = tokio::time::timeout(TIMEOUT, auth.add("token"));
		match offered {
			true => {
				let (token, ()) = tokio::join!(presented, async {
					let admission = tokio::time::timeout(TIMEOUT, admissions.next()).await.unwrap().unwrap();
					assert!(admission.attached(), "an AUTH token arrived as a new session");
					admission.grant(grant());
				});
				let (_, answer) = token
					.expect("the token was never answered")
					.expect("the token was refused");
				assert_eq!(answer, moq_net::auth::Answer::Granted);
			}
			false => {
				// Without MoQ Auth no AUTH stream carries it: only a request naming it does.
				let (_, answer) = presented
					.await
					.expect("the token was never answered")
					.expect("a moq-transport request can carry the token");
				assert_eq!(answer, moq_net::auth::Answer::Requests, "a token without MoQ Auth");
			}
		}

		drop(connection);
		running.abort();
		let _ = running.await;
	}
}

/// `listen.timeout` bounds every handshake phase from the first packet: TLS,
/// HTTP/3 before CONNECT, and MoQ SETUP over raw QUIC and WebTransport. Each
/// phase closes in its own error space.
///
/// The relay and the client run on a manual clock, so only the test passes time.
/// Both select no congestion control, so neither paces: a pacer waits on time
/// passing.
#[cfg(feature = "quic")]
#[test]
fn uring_workers_bound_stalled_handshakes() {
	use moq_tokio::quic::{driver, web};
	use web_transport_trait::Error as _;

	const LISTEN: Duration = Duration::from_millis(200);

	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("runtime");
	let clock = moq_clock::Manual::new().install(runtime.handle());
	runtime.block_on(async move {
		let dir = tempfile::tempdir().expect("tempdir");
		let (cert, key) = certificate(dir.path());
		let mut config = uring_config(&cert, &key);
		config.listen.timeout = Some(LISTEN.into());
		config.quic.congestion_control = Some(moq_tokio::quic::CongestionControl::None);
		let relay = Relay::load(config).await.expect("load relay").with_signals(false);
		let port = relay.addr().expect("workers bound an address").port();
		let addr = SocketAddr::from(([127, 0, 0, 1], port));
		let running = tokio::spawn(relay.run());

		let mut config = moq_tokio::connect::Config::default();
		config.tls.insecure = Some(true);
		let tls = config.tls.build().expect("tls config");
		let crypto = |alpn: &[u8]| {
			let mut tls = tls.clone();
			tls.alpn_protocols = vec![alpn.to_vec()];
			let crypto = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("QUIC crypto");
			let mut transport = driver::transport::Config::default();
			transport.congestion = std::sync::Arc::new(moq_congestion::Unlimited);
			let mut config = driver::client::Config::new(std::sync::Arc::new(crypto));
			config.transport = transport;
			config
		};
		let client =
			driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
				.expect("client");

		// TLS: the relay never hears more than the ClientHello. Once it answers, its
		// deadline is armed, and passing it is the only way the handshake can end:
		// the client's own timeouts are far beyond the time this test lets pass.
		let (hop, answered) = stall_after_hello(addr).await;
		let stalled = client.connect(&crypto(b"moq-lite-06"), hop, "localhost").unwrap();
		let stalled = tokio::spawn(async move {
			match stalled.await {
				Ok(conn) => conn.closed().await,
				Err(err) => err,
			}
		});
		answered.await.expect("the relay answered the ClientHello");
		clock.advance(LISTEN);
		let error = stalled.await.unwrap();
		assert!(
			!matches!(error, driver::connection::Error::TimedOut),
			"the relay must close a stalled QUIC handshake: {error}"
		);

		// `h3` stalls before CONNECT, where only HTTP/3 codes apply.
		const H3_GENERAL_PROTOCOL_ERROR: u32 = 0x0101;
		let timeout = moq_net::SessionError::Timeout.to_code();
		for (scheme, code) in [("moql", timeout), ("https", timeout), ("h3", H3_GENERAL_PROTOCOL_ERROR)] {
			let session = async {
				let alpn: &[u8] = if scheme == "moql" { b"moq-lite-06" } else { b"h3" };
				let conn = client.connect(&crypto(alpn), addr, "localhost").unwrap().await.unwrap();
				// The client is connected once it sends its Finished, but the relay has
				// left the TLS phase only once it confirms the handshake.
				while conn.stats().frame_rx.handshake_done == 0 {
					tokio::task::yield_now().await;
				}
				if scheme == "https" {
					let url: url::Url = format!("https://{addr}/stalled").parse().unwrap();
					let request = web::proto::ConnectRequest::new(url).with_protocol("moq-lite-06".to_string());
					web::Session::connect(conn, request).await.unwrap()
				} else {
					web::Session::raw(conn)
				}
			}
			.await;
			// No time passed since the first packet armed the deadline.
			clock.advance(LISTEN);
			let error = session.closed().await;
			// The WebTransport wrapper translates its close code back to the MoQ value.
			assert_eq!(
				error.session_error().map(|(code, _)| code),
				Some(code),
				"{scheme}: {error}"
			);
		}
		// Not awaited: draining waits on time this clock never passes.
		drop(client.close(driver::VarInt::from_u32(0), "test complete"));
		running.abort();
		let _ = running.await;
	});
}

/// A UDP hop to `server` that forwards one client's datagrams until the server
/// first answers, and every answer back, so the server never sees the client
/// finish its handshake. Returns the address to dial, and a signal for the
/// server's first answer.
#[cfg(feature = "quic")]
async fn stall_after_hello(server: SocketAddr) -> (SocketAddr, tokio::sync::oneshot::Receiver<()>) {
	let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("bind hop");
	let addr = socket.local_addr().expect("hop address");
	let (answered_tx, answered) = tokio::sync::oneshot::channel();
	tokio::spawn(async move {
		let mut client = None;
		let mut answered = Some(answered_tx);
		let mut buf = vec![0; usize::from(u16::MAX)];
		while let Ok((len, from)) = socket.recv_from(&mut buf).await {
			if from == server {
				if let Some(client) = client {
					let _ = socket.send_to(&buf[..len], client).await;
				}
				if let Some(answered) = answered.take() {
					let _ = answered.send(());
				}
			} else if answered.is_some() {
				client = Some(from);
				let _ = socket.send_to(&buf[..len], server).await;
			}
		}
	});
	(addr, answered)
}

/// The harness keeps only the connection target, including its span on worker and shared tasks.
#[tokio::test]
async fn uring_sessions_log_their_close_under_a_connection_span() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	if !supported() {
		return;
	}
	let logs = Logs::default();
	let _logging = logs.capture();
	let parent = tracing::info_span!(target: "moq_relay::connection", "embedder");
	let _parent = parent.enter();
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let relay = Relay::load(uring_config(&cert, &key))
		.await
		.unwrap()
		.with_signals(false);
	let port = relay.addr().unwrap().port();
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	let mut connections = Vec::new();
	for (id, scheme) in ["moql", "https"].into_iter().enumerate() {
		let url = format!("{scheme}://127.0.0.1:{port}/logs").parse().unwrap();
		connections.push(connect(client(), url).await);
		tokio::time::timeout(TIMEOUT, logs.wait_for(&format!("conn{{id={id} ")))
			.await
			.expect("negotiated log missing");
	}
	// Both sessions are still open when the drain starts, so the workers stop
	// right after they leave: their closes must not depend on a stopped worker.
	trigger.start();
	for connection in connections {
		let _ = connection.close().await;
	}
	tokio::time::timeout(TIMEOUT, running).await.unwrap().unwrap().unwrap();
	for id in 0..2 {
		tokio::time::timeout(TIMEOUT, logs.wait_for(&format!("connection closed id={id}")))
			.await
			.unwrap_or_else(|_| panic!("conn {id} never logged its close: {}", logs.text()));
	}
	let text = logs.text();
	let negotiated: Vec<_> = text.lines().filter(|line| line.contains("negotiated")).collect();
	let closed: Vec<_> = text.lines().filter(|line| line.contains("connection closed")).collect();
	assert_eq!(negotiated.len(), 2, "{text}");
	assert_eq!(closed.len(), 2, "{text}");
	assert!(
		negotiated
			.iter()
			.chain(&closed)
			.all(|line| line.contains("embedder:conn{")),
		"worker and shared-runtime logs must keep their embedder parent: {text}"
	);
	for id in 0..2 {
		for lines in [&negotiated, &closed] {
			assert!(
				lines.iter().any(|line| line.contains(&format!("conn{{id={id} "))),
				"missing conn {id}: {text}"
			);
		}
	}
}

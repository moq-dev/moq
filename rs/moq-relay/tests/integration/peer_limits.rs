//! A session admitted as a cluster peer widens its QUIC limits before SETUP OK,
//! while a viewer keeps the `--quic-*` ones, on the tokio listener and the
//! io_uring workers alike.
#![cfg(feature = "quic")]

use std::time::Duration;

use futures::FutureExt;
use moq_relay::{Config, Relay};
use moq_tokio::quic::driver;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The viewer stream limit. Small, so a peer's widened one is unmistakable.
const VIEWER_STREAMS: u64 = 4;
/// How many streams the probe tries to open without waiting for credit.
const PROBE: usize = 16;
/// The [`bare_server`] idle timeout.
pub(crate) const BARE_IDLE: Duration = Duration::from_secs(300);

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

pub(crate) fn config(dir: &std::path::Path) -> Config {
	let (cert, key) = certificate(dir);
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![cert];
	config.listen.tls.key = vec![key];
	config.quic.max_streams = Some(VIEWER_STREAMS);
	config.limits.peer.quic.max_streams = std::num::NonZeroU64::new(4 * PROBE as u64).unwrap();
	config
}

/// What a raw QUIC client sees after SETUP: how many unidirectional streams it
/// can open at once, and how many MAX_DATA frames the relay sent.
struct Seen {
	streams: usize,
	max_data: u64,
}

/// Dial with an empty moq-lite SETUP, wait for the relay's SETUP, then probe.
///
/// A peer waits for each stream's credit, since SETUP and the MAX_STREAMS
/// queued before it can travel in separate packets. A viewer takes only the
/// credit it holds once SETUP arrives: the relay widens before that, so any
/// widening would already show.
async fn probe(port: u16, peer: bool) -> Seen {
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let mut tls = connect.tls.build().expect("tls config");
	tls.alpn_protocols = vec![b"moq-lite-06".to_vec()];
	let crypto = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic crypto");
	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
			.expect("endpoint");
	let conn = endpoint
		.connect(
			&driver::client::Config::new(std::sync::Arc::new(crypto)),
			([127, 0, 0, 1], port).into(),
			"localhost",
		)
		.expect("connect")
		.await
		.expect("handshake");

	// SETUP type, then a one-byte message holding zero parameters.
	let mut setup = conn.open_uni(Default::default()).await.expect("open");
	setup.write_all(&[0x01, 0x01, 0x00][..]).await.expect("write");
	setup.finish();

	// The relay widens a peer before it answers SETUP, so its SETUP stream
	// arrives behind the new credit.
	let _setup = kio::clock::timeout(TIMEOUT, conn.accept_uni())
		.await
		.expect("relay SETUP timeout")
		.expect("relay SETUP");

	let mut streams = Vec::new();
	while streams.len() < PROBE {
		let open = conn.open_uni(Default::default());
		let stream = match peer {
			true => kio::clock::timeout(TIMEOUT, open)
				.await
				.expect("peer stream credit timeout"),
			false => match open.now_or_never() {
				Some(stream) => stream,
				None => break,
			},
		};
		streams.push(stream.expect("open"));
	}
	let seen = Seen {
		streams: streams.len(),
		max_data: conn.stats().frame_rx.max_data,
	};
	conn.close(driver::VarInt::from_u32(0), "done");
	endpoint.close(driver::VarInt::from_u32(0), "done").await;
	seen
}

/// Admit the first session as a cluster peer and the second as a viewer, and
/// check that only the peer saw wider limits.
async fn peer_widens_and_viewer_does_not(mut relay: Relay, port: u16) {
	let mut admissions = relay.admissions().expect("embedded auth");
	let deciding = tokio::spawn(async move {
		let mut leases = Vec::new();
		for peer in [true, false] {
			let admission = kio::clock::timeout(TIMEOUT, admissions.next())
				.await
				.expect("admission timeout")
				.expect("admission");
			let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
			let mut grant = moq_auth::Grant::new(all.clone(), all);
			grant.peer = peer;
			let (producer, consumer) = moq_auth::lease::Producer::new(grant);
			leases.push(producer);
			admission.grant(consumer);
		}
		leases
	});
	let running = tokio::spawn(relay.run());

	let peer = probe(port, true).await;
	assert_eq!(peer.streams, PROBE, "a peer opens past the viewer stream limit");
	assert!(peer.max_data > 0, "a peer's wider receive window is advertised");

	let viewer = probe(port, false).await;
	assert!(
		viewer.streams < PROBE && viewer.streams as u64 <= VIEWER_STREAMS + 1,
		"a viewer keeps the stream limit, opened {}",
		viewer.streams
	);
	assert_eq!(viewer.max_data, 0, "a viewer's receive window is unchanged");

	let _leases = deciding.await.expect("decider");
	running.abort();
	let _ = running.await;
}

#[tokio::test]
async fn tokio_peer_session_widens_quic_limits() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let relay = Relay::load(config(dir.path()))
		.await
		.expect("load relay")
		.with_signals(false);
	let port = relay.quic_addr().expect("quic listener bound").port();
	peer_widens_and_viewer_does_not(relay, port).await;
}

/// Load a relay with `set` applied, returning why it was refused.
async fn refused_at_load(set: impl FnOnce(&mut Config)) -> String {
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path());
	set(&mut config);
	match Relay::load(config).await {
		Ok(_) => panic!("an unadvertisable QUIC limit must be refused"),
		Err(err) => format!("{err:#}"),
	}
}

/// A limit QUIC cannot advertise or honor is refused at load, in `[limits.peer.quic]`
/// and `[quic]` alike, rather than wedging the peer or the handshake later.
#[tokio::test]
async fn unadvertisable_quic_limits_are_refused_at_load() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let err = refused_at_load(|config| {
		config.limits.peer.quic.receive_window = std::num::NonZeroU64::new(1 << 62).unwrap();
	})
	.await;
	assert!(
		err.contains("[limits.peer.quic]") && err.contains("receive_window"),
		"{err}"
	);

	// Incomplete messages hold connection credit, so the window has to fit the largest.
	let err = refused_at_load(|config| {
		let window = moq_net::transport::MIN_RECEIVE_WINDOW - 1;
		config.limits.peer.quic.receive_window = std::num::NonZeroU64::new(window).unwrap();
	})
	.await;
	assert!(
		err.contains("[limits.peer.quic]") && err.contains("largest message"),
		"{err}"
	);

	let err = refused_at_load(|config| {
		config.limits.peer.quic.max_streams = std::num::NonZeroU64::new((1 << 60) + 1).unwrap();
	})
	.await;
	assert!(
		err.contains("[limits.peer.quic]") && err.contains("max_streams"),
		"{err}"
	);

	// Zero lets the peer open nothing, so it is refused too rather than wedging the connection.
	for max_streams in [0, (1 << 60) + 1] {
		let err = refused_at_load(|config| config.quic.max_streams = Some(max_streams)).await;
		assert!(
			err.contains("[quic]") && err.contains("max_streams"),
			"{max_streams}: {err}"
		);
	}
}

/// A bare QUIC server speaking the dial template's ALPNs: the dialer's stream
/// limit is a transport parameter, so the handshake alone shows it. Its idle
/// timeout is far past any test's, so only a close ends a link in time.
pub(crate) fn bare_server(connect: &moq_tokio::connect::Config) -> driver::endpoint::Endpoint {
	let key = rcgen::KeyPair::generate().expect("keypair");
	let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
		.expect("cert params")
		.self_signed(&key)
		.expect("self-signed cert");
	let mut tls = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(
			vec![cert.der().clone()],
			rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
		)
		.expect("server tls");
	tls.alpn_protocols = connect
		.versions()
		.alpns()
		.iter()
		.map(|alpn| alpn.as_bytes().to_vec())
		.collect();
	let crypto = driver::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic crypto");
	let mut transport = driver::transport::Config::default();
	transport.idle_timeout = Some(BARE_IDLE);
	let mut config = driver::server::Config::new(std::sync::Arc::new(crypto));
	config.transport = transport;
	driver::endpoint::Endpoint::new(
		driver::endpoint::Config::default().with_server(config),
		std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
	)
	.expect("server endpoint")
}

/// Start a cluster whose one static peer is `server`, and accept its dial.
async fn static_dial(
	server: &driver::endpoint::Endpoint,
	connect: moq_tokio::connect::Config,
	quic: moq_tokio::quic::Config,
) -> (
	driver::connection::Connection,
	tokio::task::JoinHandle<anyhow::Result<()>>,
) {
	let port = server.local_addr().port();
	let mut options = moq_relay::cluster::Options::new(Default::default());
	options.config.connect = vec![moq_relay::cluster::Peer::new(format!("moql://127.0.0.1:{port}/"))];
	options.peer.quic.max_streams = std::num::NonZeroU64::new(4 * PROBE as u64).unwrap();
	let cluster = moq_relay::cluster::Cluster::new(options)
		.expect("cluster init")
		.with_connect(connect, quic);
	let running = tokio::spawn(cluster.start().await.expect("cluster start").run());
	let conn = kio::clock::timeout(TIMEOUT, server.accept())
		.await
		.expect("dial timeout")
		.expect("incoming")
		.accept()
		.expect("accept")
		.await
		.expect("handshake");
	(conn, running)
}

/// Every cluster dial is a peer link, so a dial template carrying the plain
/// `[quic]` settings still advertises the peer limits from the handshake on.
#[tokio::test]
async fn cluster_dials_widen_a_plain_client() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let server = bare_server(&connect);
	let mut quic = moq_tokio::quic::Config::default();
	quic.max_streams = Some(VIEWER_STREAMS);
	let (conn, running) = static_dial(&server, connect, quic).await;

	let mut streams = Vec::new();
	while streams.len() < PROBE {
		match conn.open_uni(Default::default()).now_or_never() {
			Some(stream) => streams.push(stream.expect("open")),
			None => break,
		}
	}
	assert_eq!(streams.len(), PROBE, "the dial advertises the peer stream limit");

	running.abort();
	let _ = running.await;
	conn.close(driver::VarInt::from_u32(0), "done");
}

/// A static peer dials from the `[connect]` bind port, so a deployment that
/// admits cluster traffic by source port keeps working.
#[tokio::test]
async fn static_peer_dials_from_the_configured_bind() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	// Borrow a free port from the OS, then hand it to the dial template.
	let bind = std::net::UdpSocket::bind("127.0.0.1:0")
		.expect("probe socket")
		.local_addr()
		.expect("probe addr");
	connect.bind = Some(bind);
	let server = bare_server(&connect);
	let (conn, running) = static_dial(&server, connect, Default::default()).await;

	assert_eq!(conn.remote_address(), bind, "the dial keeps the configured bind");

	running.abort();
	let _ = running.await;
	conn.close(driver::VarInt::from_u32(0), "done");
}

/// The tokio worker shards accept through the same request path, so a peer
/// widens there too.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn tokio_worker_peer_session_widens_quic_limits() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path());
	config.runtime.workers = Some(1);
	config.runtime.pin = false;
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	peer_widens_and_viewer_does_not(relay, port).await;
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_peer_session_widens_quic_limits() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => {}
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring relay test: {reason}");
			return;
		}
		Err(err) => panic!("io_uring worker setup failed: {err}"),
	}
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path());
	config.runtime.workers = Some(1);
	config.runtime.pin = false;
	config.runtime.io_uring = true;
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	peer_widens_and_viewer_does_not(relay, port).await;
}

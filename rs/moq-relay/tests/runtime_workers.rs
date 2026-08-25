//! The thread-per-core QUIC mode, end to end through a real relay.
//!
//! Linux-only, because the mode is: every worker binds the listen address with
//! `SO_REUSEPORT`, and no other platform load-balances a unicast UDP port
//! across the group.
#![cfg(target_os = "linux")]

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use moq_relay::{Config, PublicConfig, Relay};
use moq_tokio::moq_net::{self, Origin};

const TIMEOUT: Duration = Duration::from_secs(10);
const WORKERS: u16 = 4;

/// A UDP port nothing is bound to.
///
/// Every worker binds the same port, so this cannot be `:0`: each would pick an
/// ephemeral port of its own and they would not form a group.
fn free_udp_port() -> u16 {
	let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

/// A self-signed certificate on disk. Workers refuse `listen.tls.generate`,
/// since each would generate one of its own and serve a different identity.
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

/// A relay config serving QUIC from `workers` pinned-less worker threads.
///
/// Pinning is off because a CI container may restrict which cores it may run on,
/// and none of these tests are about placement.
fn worker_config(cert: &std::path::Path, key: &std::path::Path, port: u16, workers: u16) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some(format!("127.0.0.1:{port}"));
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.runtime.workers = Some(workers);
	config.runtime.pin = Some(false);
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

/// A publisher and several subscribers over QUIC, served by a pool of workers.
///
/// The relay hands each connection to whichever worker the kernel steered its
/// first packet to, so the subscribers are spread over threads the publisher is
/// probably not on: the frame has to travel through the shared origin to reach
/// them. That crossing is the part of the mode that could deadlock or drop, and
/// it is what this asserts, whichever way the placement lands.
#[tokio::test]
async fn workers_serve_quic_and_share_one_origin() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let config = worker_config(&cert, &key, port, WORKERS);

	let relay = Relay::load(config).await.expect("load relay");
	let expected: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
	assert_eq!(relay.addr, Some(expected), "workers bound a different address");

	let cluster = relay.cluster.clone();
	let auth = relay.auth.clone();
	let shutdown = relay.shutdown.clone();
	// The group has to outlive the accept loops it owns, so it stays on this
	// stack: stopping it is what stops the workers.
	let mut workers = relay.workers.expect("workers configured");
	let mut tasks = Vec::new();
	for (server, spawner) in workers.split() {
		tasks.push(spawner.run(moq_relay::serve(
			server,
			cluster.clone(),
			auth.clone(),
			shutdown.clone(),
		)));
	}

	let url: url::Url = format!("https://127.0.0.1:{port}/workers").parse().expect("parse url");

	// ── publisher ───────────────────────────────────────────────────
	let origin = moq_tokio::origin::spawn(Origin::random());
	let mut broadcast = origin
		.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let publisher = connect(client().with_publisher(&origin), url.clone()).await;

	// ── subscribers ─────────────────────────────────────────────────
	let mut subscribers = Vec::new();
	for _ in 0..WORKERS {
		let origin = moq_tokio::origin::spawn(Origin::random());
		let announced = origin.consume().announced();
		let connection = connect(client().with_subscriber(origin), url.clone()).await;
		subscribers.push((connection, announced));
	}

	for (index, (_connection, announced)) in subscribers.iter_mut().enumerate() {
		let moq_net::announce::Update { path, broadcast } = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.unwrap_or_else(|_| panic!("subscriber {index} announcement timeout"))
			.expect("origin closed");
		assert_eq!(path.as_str(), "test");
		let broadcast = broadcast.expect("expected announce, got unannounce");

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

	assert!(
		tasks.iter().all(|task| !task.is_finished()),
		"a QUIC worker stopped while serving"
	);

	drop(track);
	drop(broadcast);
	drop(publisher);
	drop(subscribers);
	workers.shutdown().await;
}

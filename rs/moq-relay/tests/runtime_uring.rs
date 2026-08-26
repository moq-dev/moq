//! The io_uring thread-per-core mode, end to end through a real relay:
//! browsers (WebTransport over `h3`) and native peers (raw QUIC) served by
//! pinned io_uring workers, with authentication and supervision on the shared
//! runtime and the frame crossing worker threads through the shared origin.
//!
//! Linux-only like the mode itself, and kernel-gated below the io_uring 6.12
//! floor (GitHub-hosted CI), where it skips loudly.
#![cfg(all(target_os = "linux", feature = "io-uring"))]

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use moq_relay::{Config, PublicConfig, Relay};
use moq_tokio::moq_net::{self, Origin};

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

	let publisher = connect(client().with_publisher(&origin), wt_url.clone()).await;

	// Several subscribers of each flavor, spread over the steered workers.
	let mut subscribers = Vec::new();
	for url in [&wt_url, &raw_url, &wt_url, &raw_url] {
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

	assert!(!running.is_finished(), "the relay stopped while serving");

	drop(track);
	drop(broadcast);
	drop(publisher);
	drop(subscribers);
	// Dropping the run task drops the worker group, which joins its threads.
	running.abort();
	let _ = running.await;
}

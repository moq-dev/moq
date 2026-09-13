//! External-crate embedding: custom routes, cloned handles, the owner keeps
//! listeners and workers.
//!
//! Integration tests compile as a separate crate, so they can only use the
//! public API. Each runtime layout (shared Tokio, worker Tokio, Linux
//! io_uring) mounts a custom HTTP route, clones the cluster origin, serves a
//! live QUIC subscriber, then stops the owner and proves the ports are free.

#![cfg(feature = "_quic")]

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::time::Duration;

use moq_relay::{Config, PublicConfig, Relay};
use moq_tokio::moq_net::{self, Hop};

const TIMEOUT: Duration = Duration::from_secs(10);

fn free_tcp_port() -> u16 {
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

fn free_udp_port() -> u16 {
	let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

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

fn public_auth(config: &mut Config) {
	#[allow(deprecated)]
	{
		config.auth.public = Some(PublicConfig::Simple(vec![String::new()]));
	}
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.once = Some(true);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

async fn wait_for_http(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			return;
		}
		if std::time::Instant::now() >= deadline {
			panic!("relay http listener never became ready on port {port}");
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

async fn assert_owner_stopped(quic: SocketAddr, http: SocketAddr) {
	assert!(
		tokio::net::TcpStream::connect(http).await.is_err(),
		"HTTP still accepted after the owner stopped"
	);
	let url: url::Url = format!("https://{quic}/").parse().expect("parse url");
	let after = tokio::time::timeout(
		Duration::from_secs(2),
		client().with_reconnect(false).connect(url).established(),
	)
	.await;
	assert!(
		matches!(after, Err(_) | Ok(Err(_))),
		"QUIC still accepted after the owner stopped"
	);
}

/// Load, mount a custom route, clone the origin, run the owner, prove QUIC
/// plus HTTP, then stop it and rebind both ports with a replacement owner.
async fn embed_and_stop(mut config: Config) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let http = config.web.http.listen.expect("http listener configured");
	let relay = Relay::load(config.clone()).await.expect("load relay");
	let quic = relay.addr().expect("quic listener bound");
	// Pin the replacement to the same ports, including a `:0` first bind.
	config.listen.bind = Some(quic.to_string());

	// The application handle: in-process workers publish here, same origin the
	// QUIC sessions see.
	let origin = relay.cluster().origin.clone();
	let web = relay
		.web()
		.routes()
		.route("/embedded", axum::routing::get(|| async { "embedded\n" }));
	let running = tokio::spawn(relay.with_web(web).run());

	wait_for_http(http.port()).await;
	assert!(!running.is_finished(), "the relay stopped while serving");

	let body = reqwest::get(format!("http://127.0.0.1:{}/embedded", http.port()))
		.await
		.expect("fetch embedded route")
		.text()
		.await
		.expect("read embedded response");
	assert_eq!(body, "embedded\n");

	let mut broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let url: url::Url = format!("https://{quic}/").parse().expect("parse url");
	let subscriber_origin = moq_tokio::origin::spawn(Hop::random());
	let consumer = subscriber_origin.consume();
	let mut announced = consumer.announced();
	let subscriber = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_reconnect(false)
			.with_subscriber(subscriber_origin)
			.connect(url)
			.established(),
	)
	.await
	.expect("connect timeout")
	.expect("connect failed");

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

	drop(track);
	drop(broadcast);
	drop(subscriber);

	running.abort();
	let _ = running.await;
	assert_owner_stopped(quic, http).await;

	// A replacement owner can bind the same ports, so the workers joined.
	let replacement = Relay::load(config).await.expect("rebind after stop");
	assert_eq!(replacement.addr(), Some(quic), "replacement bound a different address");
	let replacing = tokio::spawn(replacement.run());
	wait_for_http(http.port()).await;
	let health = reqwest::get(format!("http://127.0.0.1:{}/health", http.port()))
		.await
		.expect("replacement health")
		.text()
		.await
		.expect("replacement health body");
	assert!(!health.is_empty(), "replacement HTTP did not serve");
	replacing.abort();
	let _ = replacing.await;
}

fn http_and_quic(cert: &std::path::Path, key: &std::path::Path, quic_bind: String) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some(quic_bind);
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.web.http.listen = Some(format!("127.0.0.1:{}", free_tcp_port()).parse().expect("parse http"));
	config.web.ws = false;
	public_auth(&mut config);
	config
}

/// Shared Tokio runtime: one work-stealing runtime owns QUIC.
#[tokio::test]
async fn shared_tokio_custom_route_and_quic() {
	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	embed_and_stop(http_and_quic(&cert, &key, "127.0.0.1:0".into())).await;
}

/// Thread-per-core Tokio workers. Linux-only: the mode binds with `SO_REUSEPORT`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn worker_tokio_custom_route_and_quic() {
	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut config = http_and_quic(&cert, &key, format!("127.0.0.1:{}", free_udp_port()));
	config.runtime.workers = Some(2);
	config.runtime.pin = false;
	embed_and_stop(config).await;
}

/// io_uring workers. Off the default feature set; `just rs uring` and nightly
/// `--all-features` compile it. Skips on kernels below the 6.12 floor.
#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_custom_route_and_quic() {
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => {}
		Err(moq_uring::Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring embed test: {reason}");
			return;
		}
		Err(err) => panic!("io_uring worker setup failed: {err}"),
	}

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut config = http_and_quic(&cert, &key, format!("127.0.0.1:{}", free_udp_port()));
	config.runtime.workers = Some(2);
	config.runtime.pin = false;
	config.runtime.io_uring = true;
	embed_and_stop(config).await;
}

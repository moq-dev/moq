//! External-crate embedding: custom routes, cloned handles, the owner keeps
//! listeners and workers.
//!
//! Integration tests compile as a separate crate, so they can only use the
//! public API. Each runtime layout (shared Tokio, worker Tokio, Linux
//! io_uring) mounts a custom HTTP route, clones the cluster origin, serves a
//! live QUIC subscriber, then stops the owner through its shutdown trigger
//! and proves `run` returned with the ports free.

#![cfg(feature = "_quic")]

#[cfg(target_os = "linux")]
use std::net::UdpSocket;
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use moq_relay::{Config, Relay};
use moq_tokio::moq_net;

const TIMEOUT: Duration = Duration::from_secs(10);

fn free_tcp_port() -> u16 {
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

/// Only used by the Linux-only worker/uring tests below; without the gate the
/// macOS test build fails `-D warnings` on dead code.
#[cfg(target_os = "linux")]
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
	config.auth.public = vec![moq_auth::Pattern::all()];
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

/// Fire the embedder stop and wait for `run` to return: the join every
/// worker thread and listener goes through, as opposed to aborting the task.
async fn stop(trigger: moq_relay::shutdown::Trigger, running: tokio::task::JoinHandle<anyhow::Result<()>>) {
	trigger.start();
	tokio::time::timeout(TIMEOUT, running)
		.await
		.expect("run did not return after the shutdown trigger")
		.expect("run panicked")
		.expect("run returned an error after shutdown");
}

/// Load, mount a custom route, clone the origin, run the owner, prove QUIC
/// plus HTTP, then stop it through the trigger and rebind both ports with a
/// replacement owner.
async fn embed_and_stop(mut config: Config) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// No drain window: the sessions are already gone by the time the owner
	// stops, and the test should not wait out the default.
	config.drain_timeout = Duration::ZERO;
	let http = config.web.http.listen.expect("http listener configured");
	let relay = Relay::load(config.clone()).await.expect("load relay");
	let quic = relay.quic_addr().expect("quic listener bound");
	assert_eq!(relay.web_addrs().http, Some(http));
	assert_eq!(
		relay.config().quic.max_streams,
		Some(moq_tokio::quic::DEFAULT_MAX_STREAMS)
	);
	assert_eq!(relay.cluster().id(), relay.cluster().origin.hop().id());
	// Pin the replacement to the same ports, including a `:0` first bind.
	config.listen.bind = Some(moq_tokio::listen::Bind::Addr(quic));

	// The application handles: in-process workers publish into the origin the
	// QUIC sessions see, and the trigger stops the owner from any task. Both
	// are cloned before `run` consumes the relay.
	let origin = relay.cluster().origin.clone();
	let trigger = relay.shutdown_trigger().clone();
	let ready = relay.ready();
	let web = relay
		.web()
		.routes()
		.route("/embedded", axum::routing::get(|| async { "embedded\n" }))
		.route(
			"/restricted",
			axum::routing::post(|| async { ([("access-control-allow-origin", "https://trusted.example")], "private") }),
		)
		.route("/plain-post", axum::routing::post(|| async { "plain" }));
	let running = tokio::spawn(relay.with_web(web).run());
	ready.wait().await.expect("relay ready");

	wait_for_http(http.port()).await;
	assert!(!running.is_finished(), "the relay stopped while serving");

	let body = reqwest::get(format!("http://127.0.0.1:{}/embedded", http.port()))
		.await
		.expect("fetch embedded route")
		.text()
		.await
		.expect("read embedded response");
	assert_eq!(body, "embedded\n");
	let response = reqwest::Client::new()
		.get(format!("http://127.0.0.1:{}/embedded", http.port()))
		.header(reqwest::header::ORIGIN, "https://example.test")
		.send()
		.await
		.expect("fetch embedded route with Origin");
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
			.unwrap(),
		"*"
	);
	let response = reqwest::Client::new()
		.post(format!("http://127.0.0.1:{}/restricted", http.port()))
		.header(reqwest::header::ORIGIN, "https://untrusted.example")
		.send()
		.await
		.expect("post to restricted embedder route");
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
			.unwrap(),
		"https://trusted.example",
		"relay CORS must preserve the embedder's policy on POST"
	);
	let response = reqwest::Client::new()
		.post(format!("http://127.0.0.1:{}/plain-post", http.port()))
		.header(reqwest::header::ORIGIN, "https://untrusted.example")
		.send()
		.await
		.expect("post to plain embedder route");
	assert!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
			.is_none(),
		"relay CORS must not grant wildcard access to POST"
	);
	let response = reqwest::Client::new()
		.request(
			reqwest::Method::OPTIONS,
			format!("http://127.0.0.1:{}/embedded", http.port()),
		)
		.header(reqwest::header::ORIGIN, "https://example.test")
		.header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
		.header(reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
		.send()
		.await
		.expect("preflight embedded GET route");
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
			.unwrap(),
		"*"
	);
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_METHODS)
			.unwrap(),
		"GET"
	);
	assert_eq!(
		response
			.headers()
			.get(reqwest::header::ACCESS_CONTROL_ALLOW_HEADERS)
			.unwrap(),
		"authorization"
	);

	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let url: url::Url = format!("https://{quic}/").parse().expect("parse url");
	let subscriber_origin = moq_tokio::origin::spawn();
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

	drop(track);
	drop(broadcast);
	drop(subscriber);

	stop(trigger, running).await;
	assert_owner_stopped(quic, http).await;

	// A replacement owner can bind the same ports, so the workers joined.
	let replacement = Relay::load(config).await.expect("rebind after stop");
	assert_eq!(replacement.addr(), Some(quic), "replacement bound a different address");
	let trigger = replacement.shutdown_trigger().clone();
	let replacing = tokio::spawn(replacement.run());
	wait_for_http(http.port()).await;
	let health = reqwest::get(format!("http://127.0.0.1:{}/health", http.port()))
		.await
		.expect("replacement health")
		.text()
		.await
		.expect("replacement health body");
	assert!(!health.is_empty(), "replacement HTTP did not serve");
	stop(trigger, replacing).await;
	assert_owner_stopped(quic, http).await;
}

fn http_and_quic(cert: &std::path::Path, key: &std::path::Path, quic_bind: String) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some(quic_bind.parse().unwrap());
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.web.http.listen = Some(format!("127.0.0.1:{}", free_tcp_port()).parse().expect("parse http"));
	config.web.ws = false;
	public_auth(&mut config);
	config
}

/// A late TCP bind failure must close readiness without reporting success.
#[tokio::test]
async fn tcp_bind_failure_does_not_report_ready() {
	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve TCP port");
	let mut config = http_and_quic(&cert, &key, "127.0.0.1:0".into());
	config.listen.tcp.bind = Some(occupied.local_addr().expect("reserved address"));
	let relay = Relay::load(config).await.expect("load relay before TCP bind");
	let ready = relay.ready();
	let running = tokio::spawn(relay.run());

	let result = tokio::time::timeout(TIMEOUT, ready.wait())
		.await
		.expect("readiness never resolved");
	assert!(result.is_err(), "failed TCP bind reported readiness");
	let error = running
		.await
		.expect("run panicked")
		.expect_err("run accepted an occupied TCP port");
	assert!(error.to_string().contains("failed to bind listeners"), "{error:#}");
}

/// An occupied internal port must fail before the relay reports readiness.
#[tokio::test]
async fn internal_bind_failure_does_not_report_ready() {
	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve internal port");
	let mut config = http_and_quic(&cert, &key, "127.0.0.1:0".into());
	config.internal.listen = Some(occupied.local_addr().expect("reserved address"));
	let relay = Relay::load(config).await.expect("load relay before internal bind");
	let ready = relay.ready();
	let running = tokio::spawn(relay.run());

	let result = tokio::time::timeout(TIMEOUT, ready.wait())
		.await
		.expect("readiness never resolved");
	assert!(result.is_err(), "failed internal bind reported readiness");
	let error = running
		.await
		.expect("run panicked")
		.expect_err("run accepted an occupied internal port");
	assert!(
		error.to_string().contains("failed to bind internal listener"),
		"{error:#}"
	);
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

/// An embedder-owned accept loop joins the relay's own /metrics exposition.
#[tokio::test]
async fn embedded_listener_health_reaches_metrics() {
	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut config = http_and_quic(&cert, &key, "127.0.0.1:0".into());
	config.internal.listen = Some(format!("127.0.0.1:{}", free_tcp_port()).parse().unwrap());
	config.drain_timeout = Duration::ZERO;
	let internal = config.internal.listen.unwrap();
	let relay = Relay::load(config).await.expect("load relay");
	let ready = relay.ready();
	let trigger = relay.shutdown_trigger().clone();
	let health = moq_tokio::accept::Health::new("embedded");
	let running = tokio::spawn(relay.with_listeners([health]).run());
	ready.wait().await.expect("relay ready");
	let body = reqwest::get(format!("http://{internal}/metrics"))
		.await
		.expect("fetch metrics")
		.text()
		.await
		.expect("read metrics");
	assert!(
		body.contains("moq_relay_accept_failures_total{listener=\"embedded\",class=\"exhausted\"} 0"),
		"{body}"
	);
	stop(trigger, running).await;
}

#[derive(usage::Cli, Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
#[usage(name = "embedded-relay", unknown_flags = "error", args_override_self = false)]
#[usage(settings)]
struct EmbeddedConfig {
	#[usage(flatten)]
	#[serde(flatten)]
	relay: Config,
	#[usage(long = "worker-name")]
	#[serde(skip)]
	worker_name: Option<String>,
}

/// A flattened relay merges its settings without resetting the embedder's flags.
#[test]
fn embedded_cli_merges_only_relay_settings() {
	let (mut parsed, cli) = EmbeddedConfig::parse_from_with_settings(&[
		std::ffi::OsStr::new("--worker-name"),
		std::ffi::OsStr::new("recorder"),
		std::ffi::OsStr::new("--cluster-id"),
		std::ffi::OsStr::new("9"),
	])
	.expect("parse embedding CLI");
	let file = toml::from_str::<toml::Value>("[cluster]\nid = 7\n").unwrap();
	let source = moq_tokio::cli::FileSource {
		path: std::path::Path::new("relay.toml"),
		value: &file,
	};
	parsed
		.relay
		.merge_into(&cli, &usage::config::EnvLayer::from_process(), Some(source))
		.unwrap();
	assert_eq!(parsed.worker_name.as_deref(), Some("recorder"));
	assert_eq!(parsed.relay.cluster.id, Some(9));
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

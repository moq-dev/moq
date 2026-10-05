//! Sessions admission refuses reach `/metrics` by reason, through a real relay
//! over QUIC and WebSocket.

#![cfg(feature = "_quic")]

use std::net::SocketAddr;
use std::time::Duration;

use moq_relay::{Config, Relay};

const TIMEOUT: Duration = Duration::from_secs(10);

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

/// QUIC and HTTP on ephemeral ports, `/metrics` on the internal listener, and no
/// admission configured: each test sets its own. WebSocket is served only when `ws`,
/// on the HTTP port. A QUIC dial's WebSocket fallback goes to the QUIC port, where
/// nothing listens over TCP, so it cannot reach admission and count a second time.
fn config(dir: &std::path::Path, ws: bool) -> Config {
	let (cert, key) = certificate(dir);
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![cert];
	config.listen.tls.key = vec![key];
	config.web.http.listen = Some("127.0.0.1:0".parse().expect("parse http"));
	config.web.ws = ws;
	config.internal.listen = Some("127.0.0.1:0".parse().unwrap());
	config.drain_timeout = Duration::ZERO;
	config
}

struct Running {
	quic: SocketAddr,
	#[cfg_attr(not(feature = "websocket"), allow(dead_code))]
	http: SocketAddr,
	metrics: String,
	trigger: moq_relay::shutdown::Trigger,
	running: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(config: Config) -> Running {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let relay = Relay::load(config).await.expect("load relay");
	let quic = relay.quic_addr().expect("quic listener bound");
	let http = relay.web_addrs().http.expect("http listener bound");
	let internal = relay.internal().addr().expect("internal listener bound");
	let ready = relay.ready();
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");
	Running {
		quic,
		http,
		metrics: format!("http://{internal}/metrics"),
		trigger,
		running,
	}
}

impl Running {
	async fn stop(self) {
		self.trigger.start();
		tokio::time::timeout(TIMEOUT, self.running)
			.await
			.expect("run did not return after the shutdown trigger")
			.expect("run panicked")
			.expect("run returned an error after shutdown");
	}

	/// The `/metrics` count for one refusal reason.
	async fn refused(&self, reason: &str) -> u64 {
		let body = reqwest::get(&self.metrics)
			.await
			.expect("scrape")
			.text()
			.await
			.expect("metrics body");
		let row = format!("moq_relay_sessions_refused_total{{reason=\"{reason}\"}} ");
		body.lines()
			.find_map(|line| line.strip_prefix(row.as_str()))
			.unwrap_or_else(|| panic!("no {reason} row in /metrics:\n{body}"))
			.parse()
			.expect("refusal count")
	}
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.once = Some(true);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

/// Dial `url` as a publisher or a subscriber and require the relay to refuse.
/// A client may finish connecting before the relay's verdict lands, so a session
/// that closes promptly counts as refused too.
async fn assert_refused(url: url::Url, publisher: bool) {
	let origin = moq_tokio::origin::spawn();
	let client = match publisher {
		true => client().with_publisher(origin.consume()),
		false => client().with_subscriber(origin),
	};
	let connected = tokio::time::timeout(TIMEOUT, client.with_reconnect(false).connect(url).established())
		.await
		.expect("connect timeout");
	if let Ok(session) = connected {
		// How it closed does not matter here; /metrics says why.
		let _ = tokio::time::timeout(TIMEOUT, session.closed())
			.await
			.expect("the relay kept a session it should refuse");
	}
}

/// A path the public rules do not reach is `refused`.
#[tokio::test]
async fn refused_sessions_are_counted() {
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.public = vec!["anon/**".parse().unwrap()];
	let relay = start(config).await;

	assert_eq!(relay.refused("refused").await, 0);
	assert_refused(format!("https://{}/rooms", relay.quic).parse().unwrap(), false).await;
	assert_eq!(relay.refused("refused").await, 1);
	assert_eq!(relay.refused("forbidden").await, 0);
	relay.stop().await;
}

/// The WebSocket path counts a refusal the same way.
#[cfg(feature = "websocket")]
#[tokio::test]
async fn websocket_refused_sessions_are_counted() {
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), true);
	config.auth.public = vec!["anon/**".parse().unwrap()];
	let relay = start(config).await;

	assert_refused(format!("ws://{}/rooms", relay.http).parse().unwrap(), false).await;
	assert_eq!(relay.refused("refused").await, 1);
	relay.stop().await;
}

/// A grant that only allows subscribing refuses a publisher as `forbidden`.
#[tokio::test]
async fn forbidden_sessions_are_counted() {
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.public_subscribe = vec!["anon/**".parse().unwrap()];
	let relay = start(config).await;

	assert_refused(format!("https://{}/anon", relay.quic).parse().unwrap(), true).await;
	assert_eq!(relay.refused("forbidden").await, 1);
	assert_eq!(relay.refused("refused").await, 0);
	relay.stop().await;
}

/// A `/.cluster` dial at a relay without LAN discovery is `lan`, decided before
/// any auth.
#[tokio::test]
async fn lan_refusals_are_counted() {
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.public = vec![moq_auth::Pattern::all()];
	let relay = start(config).await;

	assert_refused(
		format!("https://{}/.cluster/not-a-proof", relay.quic).parse().unwrap(),
		false,
	)
	.await;
	assert_eq!(relay.refused("lan").await, 1);
	assert_eq!(relay.refused("refused").await, 0);
	relay.stop().await;
}

/// An auth server that cannot be reached is `unavailable`, apart from a refusal.
#[tokio::test]
async fn unavailable_auth_server_is_counted() {
	let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
	let auth_url = format!("http://{}/", closed.local_addr().expect("reserved address"));
	drop(closed);

	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.url = Some(auth_url.parse().unwrap());
	let relay = start(config).await;

	assert_refused(format!("https://{}/rooms", relay.quic).parse().unwrap(), false).await;
	assert_eq!(relay.refused("unavailable").await, 1);
	assert_eq!(relay.refused("refused").await, 0);
	relay.stop().await;
}

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
	config.drain_timeout = Some((Duration::ZERO).into());
	config
}

struct Running {
	stats: moq_net::stats::Registry,
	origin: moq_net::origin::Consumer,
	quic: SocketAddr,
	#[cfg_attr(
		not(feature = "websocket"),
		allow(dead_code, reason = "only the `websocket` feature uses it")
	)]
	http: SocketAddr,
	metrics: String,
	trigger: moq_relay::shutdown::Trigger,
	running: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(config: Config) -> Running {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let stats = relay.stats().registry().clone();
	let origin = relay.cluster().origin.consume();
	let quic = relay.quic_addr().expect("quic listener bound");
	let http = relay.web_addrs().http.expect("http listener bound");
	let internal = relay.internal().addr().expect("internal listener bound");
	let ready = relay.ready();
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");
	Running {
		stats,
		origin,
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
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
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

/// An auth server whose connection fails is `unavailable`, apart from a refusal.
#[tokio::test]
async fn unavailable_auth_server_is_counted() {
	// Listen to reserve the port on every OS, then hang up before answering HTTP.
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
		.await
		.expect("auth listener");
	let address = listener.local_addr().expect("reserved address");
	assert!(
		std::net::TcpListener::bind(address).is_err(),
		"auth port must stay reserved"
	);
	let mut tasks = tokio::task::JoinSet::new();
	tasks.spawn(async move {
		loop {
			let (stream, _) = listener.accept().await.expect("accept auth connection");
			drop(stream);
		}
	});
	let auth_url = format!("http://{address}/");
	assert!(reqwest::get(&auth_url).await.is_err(), "auth endpoint must fail HTTP");

	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.url = Some(auth_url.parse().unwrap());
	let relay = start(config).await;

	assert_refused(format!("https://{}/rooms", relay.quic).parse().unwrap(), false).await;
	assert_eq!(relay.refused("unavailable").await, 1);
	assert_eq!(relay.refused("refused").await, 0);
	relay.stop().await;
}

/// A refusal creates the root stats group even when no session was ever admitted.
#[tokio::test]
async fn refused_only_stats_depth_one_has_root_totals_and_shared_embedder_metrics() {
	use moq_net::stats::{RefusalKind, Tier};
	let dir = tempfile::tempdir().unwrap();
	let mut config = config(dir.path(), false);
	config.auth.public = vec!["anon/**".parse().unwrap()];
	config.stats.enabled = true;
	config.stats.depth = 1;
	config.stats.node = Some("test".into());
	let relay = start(config).await;
	assert_refused(format!("https://{}/rooms", relay.quic).parse().unwrap(), false).await;
	assert_eq!(
		relay.refused("refused").await,
		1,
		"native admission counts exactly once"
	);
	let broadcast = tokio::time::timeout(
		TIMEOUT,
		relay.origin.follow(".stats/node/test", None).unwrap().available(),
	)
	.await
	.unwrap()
	.unwrap();
	let consumer = moq_stats::Consumer::new(broadcast, Default::default());
	let mut totals = consumer.totals().await.unwrap();
	tokio::time::timeout(TIMEOUT, async {
		loop {
			let frame = totals.next().await.unwrap().unwrap();
			if frame
				.get("")
				.is_some_and(|total| total.sessions.sessions_refused.refused == 1)
			{
				break;
			}
		}
	})
	.await
	.expect("refusal-only root totals");
	relay
		.stats
		.tier(Tier::new(moq_net::Path::from_static("gateway")))
		.refuse(moq_net::Path::from_static("tenant"), RefusalKind::Forbidden);
	assert_eq!(
		relay.refused("forbidden").await,
		1,
		"embedder stats and metrics share the counter"
	);
	tokio::time::timeout(TIMEOUT, async {
		loop {
			let frame = totals.next().await.unwrap().unwrap();
			if frame
				.get("gateway")
				.is_some_and(|total| total.sessions.sessions_refused.forbidden == 1)
			{
				break;
			}
		}
	})
	.await
	.expect("embedder outcome in its own tier");
	relay.stop().await;
}

/// Typed peer closes flow through the actual QUIC listener into failure stats.
#[tokio::test]
async fn peer_zero_close_is_clean_and_application_close_counts_once() {
	let dir = tempfile::tempdir().unwrap();
	let mut config = config(dir.path(), false);
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.stats.enabled = true;
	config.stats.node = Some("test".into());
	let relay = start(config).await;
	let broadcast = relay
		.origin
		.follow(".stats/node/test", None)
		.unwrap()
		.available()
		.await
		.unwrap();
	let consumer = moq_stats::Consumer::new(broadcast, Default::default());
	let mut totals = consumer.totals().await.unwrap();
	for (index, error) in [moq_net::Error::Cancel, moq_net::Error::App(42)]
		.into_iter()
		.enumerate()
	{
		let connection = client()
			.with_reconnect(false)
			.connect(format!("https://{}/", relay.quic).parse::<url::Url>().unwrap());
		let session = connection.established().await.unwrap();
		session.abort(error);
		tokio::time::timeout(TIMEOUT, async {
			loop {
				let frame = totals.next().await.unwrap().unwrap();
				if let Some(total) = frame.get("")
					&& total.sessions.sessions_ended > index as u64
				{
					assert_eq!(total.sessions.sessions_failed.application, index as u64);
					assert_eq!(total.sessions.sessions_failed.idle_timeout, 0);
					assert_eq!(total.sessions.sessions_failed.transport, 0);
					break;
				}
			}
		})
		.await
		.expect("peer close reaches the session stats");
	}
	relay.stop().await;
}

#[tokio::test]
async fn explicit_auth_server_refusal_counts_once() {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}/", listener.local_addr().unwrap());
	let app = axum::Router::new().route(
		"/",
		axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
	);
	let (stop, stopped) = tokio::sync::oneshot::channel();
	let server = tokio::spawn(async move {
		axum::serve(listener, app)
			.with_graceful_shutdown(async {
				let _ = stopped.await;
			})
			.await
			.unwrap();
	});
	let dir = tempfile::tempdir().unwrap();
	let mut config = config(dir.path(), false);
	config.auth.url = Some(url.parse().unwrap());
	config.stats.enabled = true;
	let relay = start(config).await;
	assert_refused(format!("https://{}/room", relay.quic).parse().unwrap(), false).await;
	assert_eq!(relay.refused("refused").await, 1);
	assert_eq!(relay.stats.snapshot().sessions()[0].1.sessions_refused.refused, 1);
	relay.stop().await;
	stop.send(()).unwrap();
	server.await.unwrap();
}

#[tokio::test]
async fn expired_verified_tokens_count_under_the_token_root_and_auth_tier() {
	let keys = tempfile::tempdir().unwrap();
	let key = moq_auth::Key::generate(moq_auth::Algorithm::HS256, None).unwrap();
	let key_path = keys.path().join("key.jwk");
	key.to_file(&key_path).unwrap();
	let mut claims = moq_auth::Claims::default()
		.with_root("verified/tenant")
		.with_subscribe([moq_auth::Pattern::all()]);
	claims.expires = Some(moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(1));
	let token = key.sign(&claims).unwrap();
	for tier in [None, Some("edge")] {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let mut policy = moq_auth::serve::Policy::default();
		policy.keys = Some(moq_auth::serve::Keys::File(key_path.clone()));
		policy.tier = tier.map(str::to_owned);
		let server = moq_auth::serve::Server::new(policy).unwrap();
		let auth_url = format!("http://{}/", listener.local_addr().unwrap());
		let running = tokio::spawn(async move {
			server.serve(listener).await.unwrap();
		});
		let dir = tempfile::tempdir().unwrap();
		let mut config = config(dir.path(), false);
		config.auth.url = Some(auth_url.parse().unwrap());
		config.stats.enabled = true;
		let relay = start(config).await;
		assert_refused(
			format!("https://{}/attacker/path?jwt={token}", relay.quic)
				.parse()
				.unwrap(),
			false,
		)
		.await;
		assert_eq!(relay.refused("expired").await, 1);
		assert_eq!(relay.refused("refused").await, 0);
		let mut report = moq_net::stats::Report::default();
		relay.stats.report(&mut report);
		assert_eq!(report.sessions.len(), 1);
		let entry = &report.sessions[0];
		assert_eq!(entry.tier.as_str(), tier.unwrap_or(""));
		assert_eq!(entry.label.as_str(), "verified/tenant");
		let presence = entry.presence;
		assert_eq!(presence.sessions_started, 0);
		assert_eq!(presence.sessions_refused.expired, 1);
		relay.stop().await;
		running.abort();
		assert!(running.await.unwrap_err().is_cancelled());
	}
}

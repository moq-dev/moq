//! End-to-end smoke test through a real moq-relay.
//!
//! Stands up the relay's actual axum + auth + cluster stack on an ephemeral port,
//! connects a publisher and a subscriber via WebSocket, and confirms that
//! a frame round-trips with the newest moq-lite version on both sides. The
//! version assertion is the regression guard for the
//! "axum-only-advertises-bare-`webtransport`" bug that silently downgraded
//! relay clients to moq-lite-02.

use std::time::Duration;

use moq_relay::{Config, Connection, Relay, auth, cluster, web};
use moq_tokio::moq_net;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The newest moq-lite ALPN both sides should converge on. Derived from
/// `moq_net::ALPNS` so a future version bump
/// doesn't break this test independently of the production negotiation.
/// We filter on the `moq-lite-` prefix specifically; the relay smoke test
/// is asserting lite behavior, not IETF moqt drafts.
fn newest_lite_version() -> moq_net::Version {
	moq_net::ALPNS
		.iter()
		.copied()
		.find(|alpn| alpn.starts_with("moq-lite-"))
		.expect("no moq-lite ALPN in moq_net::ALPNS")
		.parse()
		.expect("parse newest lite ALPN as a Version")
}

/// A [`web::Web`] already bound to an ephemeral loopback HTTP port.
///
/// Binding up front, rather than probing for a free port and rebinding it,
/// means no other process can take the port in between and answer our client.
async fn build_web(ws: bool) -> web::Web {
	let mut config = web::Config::default();
	config.ws = ws;
	config.http.listen = Some("127.0.0.1:0".parse().expect("parse listen"));
	build_web_with(config).await
}

/// [`build_web`] for a test that configures the listeners itself (e.g. HTTPS).
async fn build_web_with(web_config: web::Config) -> web::Web {
	// Crypto provider is process-global; reinstalls after the first one are
	// no-ops, but the test binary may run before any other moq code does.
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// A public grant of `**` lets any path through.
	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];
	let auth = auth_config
		.init("test", &moq_tokio::tls::Connect::default())
		.expect("auth init");

	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");

	// moq_tokio::Server is needed for `certificates`, even though we never
	// expose HTTPS or QUIC in this test. Binding QUIC to `[::]:0` picks an
	// unused UDP port that we ignore.
	let mut server_config = moq_tokio::listen::Config::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	let server = server_config.init(Default::default()).expect("server init");

	web::Web::new(auth, cluster, server.certificates(), web_config)
		.bind()
		.expect("bind web listeners")
}

/// The shared bootstrap: stand up a relay listening on `127.0.0.1:<ephemeral>`
/// with fully public auth, and return the port plus an abort handle for the
/// spawned web server.
async fn spawn_relay() -> (u16, tokio::task::JoinHandle<()>) {
	let web = build_web(true).await;
	let port = web.addrs().http.expect("HTTP listener is configured").port();

	// `Web::run` only returns on error; in tests we abort it at teardown.
	let handle = tokio::spawn(async move { web.run().await.expect("relay web server") });

	(port, handle)
}

/// Stand up the assembled relay path with `--server-version` restricted.
async fn spawn_versioned_relay(versions: Vec<moq_net::Version>) -> (u16, tokio::task::JoinHandle<()>) {
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.listen.version = versions;
	config.web.ws = true;
	config.web.http.listen = Some("127.0.0.1:0".parse().expect("parse listen"));

	config.auth.public = vec![moq_auth::Pattern::all()];

	// `load` binds the web listener, so the port is ours before anyone dials it.
	let relay = Relay::load(config).await.expect("load relay");
	let port = relay.web_addrs().http.expect("HTTP listener is configured").port();
	let handle = tokio::spawn(async move { relay.run().await.expect("relay") });

	(port, handle)
}

fn client() -> moq_tokio::Client {
	client_version(None)
}

/// A client pinned to a single MoQ version, or all versions when `None`.
fn client_version(version: Option<moq_net::Version>) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	// One-shot: these tests were written against a single dial, and a background
	// redial would re-register with the relay behind the assertions' back.
	config.once = Some(true);
	// Zero head start so the WebSocket path runs immediately.
	config.websocket.delay = std::time::Duration::ZERO;
	// Every relay in this file listens on IPv4 loopback, so bind the same family
	// rather than egressing a QUIC dial from a dual-stack IPv6 socket.
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	if let Some(version) = version {
		config.version = vec![version];
	}
	config.init(Default::default()).expect("client init")
}

/// Connect a publisher and a subscriber to a real relay over `ws://`, push
/// one frame end-to-end, and assert both sides see the newest moq-lite ALPN.
/// Regression for the `serve_ws` downgrade to Lite02.
#[tokio::test]
async fn relay_websocket_round_trip_uses_newest_version() {
	let (port, web_handle) = spawn_relay().await;
	let url: url::Url = format!("ws://127.0.0.1:{port}/smoke").parse().expect("parse url");
	let expected_version = newest_lite_version();

	// ── publisher ───────────────────────────────────────────────────
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_publisher(&pub_origin), url.clone()))
			.await
			.expect("publisher connect timeout")
			.expect("publisher connect failed");
	assert_eq!(
		pub_connection.version(),
		Some(expected_version),
		"publisher negotiated stale version"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();

	let (_client, sub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");
	assert_eq!(
		sub_connection.version(),
		Some(expected_version),
		"subscriber negotiated stale version"
	);

	// ── data path ───────────────────────────────────────────────────
	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = moq_net::Path::new(update.prefix.as_str()).to_owned();
	assert!(update.kind.is_active(), "expected announce, got retraction");
	// Auth root for `/smoke` is "smoke"; the broadcast "test" announces underneath.
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request_broadcast(&path)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&frame.payload[..], b"hello");

	// Hold the producers until after data is read; dropping them earlier
	// would close the publishing side of the broadcast.
	drop(track);
	drop(broadcast);

	drop(pub_connection);
	drop(sub_connection);
	web_handle.abort();
}

/// Read announcements until `until` shows up, returning every active prefix seen.
async fn announced_until(announcements: &mut moq_net::announce::Consumer, until: &str) -> Vec<String> {
	let mut seen = Vec::new();
	while !seen.iter().any(|prefix| prefix == until) {
		let update = tokio::time::timeout(TIMEOUT, announcements.next())
			.await
			.expect("announcement timeout")
			.expect("origin closed");
		if update.kind.is_active() {
			seen.push(update.prefix.as_str().to_owned());
		}
	}
	seen
}

/// A `.`-named broadcast stays out of discovery unless the reader opts in, and
/// a client that predates the opt-in (moq-lite-06) never discovers it. Subscribing
/// by exact path needs no opt-in.
#[tokio::test]
async fn hidden_broadcasts_need_a_lite07_opt_in() {
	// lite-07 is work-in-progress and off by default, so the relay must enable it.
	let lite07: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
	let lite06: moq_net::Version = "moq-lite-06".parse().unwrap();
	let (port, web_handle) = spawn_versioned_relay(vec![lite07, lite06]).await;
	let url: url::Url = format!("ws://127.0.0.1:{port}/hidden").parse().expect("parse url");

	let pub_origin = moq_tokio::origin::spawn();
	let hidden = pub_origin.create_broadcast(".x/y").expect("create hidden");
	hidden.announce(Default::default()).expect("announce hidden");
	let track = hidden.create_track("video", None).expect("create track");
	track
		.append_group()
		.expect("append group")
		.write_frame(moq_net::Timestamp::ZERO, b"hidden".as_ref())
		.expect("write frame");
	// The relay's announce request carries the opt-in only on lite-07, so the publisher
	// must speak it too for the hidden route to reach the relay.
	let (_pub_client, pub_connection) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client_version(Some(lite07)).with_publisher(&pub_origin), url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	// An opted-in lite-07 client discovers the hidden broadcast.
	let opted_origin = moq_tokio::origin::spawn();
	let mut opted = opted_origin.consume().with_hidden(true).announced();
	let (_opted_client, opted_connection) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client_version(Some(lite07)).with_subscriber(opted_origin), url.clone()),
	)
	.await
	.expect("opted connect timeout")
	.expect("opted connect failed");
	assert_eq!(announced_until(&mut opted, ".x/y").await, [".x/y"]);

	// Announced after the hidden one, so any client that could see both lists the
	// hidden one first (the relay drains its table in path order).
	let visible = pub_origin.create_broadcast("visible").expect("create visible");
	visible.announce(Default::default()).expect("announce visible");
	assert_eq!(announced_until(&mut opted, "visible").await, ["visible"]);

	// A lite-07 client that did not opt in, and a lite-06 client that cannot even
	// when its local reader asks, see only the visible broadcast.
	for (version, local_hidden) in [(lite07, false), (lite06, true)] {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume().with_hidden(local_hidden);
		let mut announcements = consumer.announced();
		let (_client, connection) = tokio::time::timeout(
			TIMEOUT,
			connect_once(client_version(Some(version)).with_subscriber(origin), url.clone()),
		)
		.await
		.expect("connect timeout")
		.expect("connect failed");
		assert_eq!(connection.version(), Some(version));
		assert_eq!(
			announced_until(&mut announcements, "visible").await,
			["visible"],
			"{version} discovered a hidden broadcast"
		);

		// Hiding narrows discovery only: the lite-07 session still mirrors the route,
		// so a subscription by exact path reaches the broadcast. A lite-06 session never
		// learns the route, so it has nothing to resolve through.
		if version == lite06 {
			continue;
		}
		let bc = consumer
			.request_broadcast(".x/y")
			.await
			.expect("hidden broadcast resolves");
		let mut sub = bc.track("video").unwrap().subscribe(None).await.expect("subscribe");
		let mut group = tokio::time::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv_group timeout")
			.expect("recv_group failed")
			.expect("track closed");
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read_frame timeout")
			.expect("read frame")
			.expect("frame");
		assert_eq!(&frame.payload[..], b"hidden");
		drop(connection);
	}

	drop((track, hidden, visible));
	drop((pub_connection, opted_connection));
	web_handle.abort();
}

/// `--server-version` applies to the WebSocket fallback as well as QUIC.
#[tokio::test]
async fn relay_websocket_honors_server_version() {
	let allowed: moq_net::Version = "moq-transport-16".parse().expect("parse allowed version");
	let excluded = newest_lite_version();
	let (port, web_handle) = spawn_versioned_relay(vec![allowed]).await;
	let url: url::Url = format!("ws://127.0.0.1:{port}/smoke").parse().expect("parse url");

	let excluded_result = tokio::time::timeout(
		TIMEOUT,
		client_version(Some(excluded)).connect(url.clone()).established(),
	)
	.await
	.expect("excluded client connect timeout");
	assert!(
		excluded_result.is_err(),
		"WebSocket accepted excluded version {excluded} despite --server-version {allowed}"
	);

	let session = tokio::time::timeout(TIMEOUT, client_version(Some(allowed)).connect(url).established())
		.await
		.expect("allowed client connect timeout")
		.expect("allowed client connect failed");
	assert_eq!(session.version(), Some(allowed));

	drop(session);
	web_handle.abort();
}

#[tokio::test]
async fn relay_web_serves_merged_routes() {
	tokio::time::pause();
	let web = build_web(false).await;
	let port = web.addrs().http.expect("HTTP listener is configured").port();
	let app = web
		.routes()
		.route("/embedded", axum::routing::get(|| async { "embedded\n" }));

	let handle = tokio::spawn(async move { web.serve(app).await.expect("relay web server") });

	let body = reqwest::get(format!("http://127.0.0.1:{port}/embedded"))
		.await
		.expect("fetch embedded route")
		.text()
		.await
		.expect("read embedded response");
	assert_eq!(body, "embedded\n");

	handle.abort();
}

/// The HTTPS listener has to terminate TLS and answer a real request.
///
/// The plain-HTTP tests above run no handshake at all, so they say nothing about
/// the TLS stack: the HTTPS acceptor wraps `RustlsAcceptor`, hot reload swaps the
/// config underneath it, and the listener the relay hands `axum_server` is its own.
/// A compile is not evidence any of that still handshakes.
#[tokio::test]
async fn relay_https_terminates_tls() {
	let dir = tempfile::TempDir::new().expect("tempdir");

	let key = rcgen::KeyPair::generate().expect("keypair");
	let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("cert params");
	let cert = params.self_signed(&key).expect("self-signed cert");
	let cert_path = dir.path().join("cert.pem");
	let key_path = dir.path().join("key.pem");
	std::fs::write(&cert_path, cert.pem()).expect("write cert");
	std::fs::write(&key_path, key.serialize_pem()).expect("write key");

	let mut config = web::Config::default();
	config.ws = false;
	config.https.listen = Some("127.0.0.1:0".parse().expect("parse listen"));
	config.https.cert = vec![cert_path];
	config.https.key = vec![key_path];
	let web = build_web_with(config).await;
	let port = web.addrs().https.expect("HTTPS listener is configured").port();

	// Held past `serve`, which consumes the server: this is the whole point of
	// taking the handle up front. `Some` because a listener is configured; a relay
	// with neither HTTP nor HTTPS reports nothing rather than a permanent zero.
	let health = web.accept_health().expect("an HTTPS listener is configured");

	let handle = tokio::spawn(async move { web.run().await.expect("relay web server") });

	let client = reqwest::Client::builder()
		.add_root_certificate(reqwest::Certificate::from_pem(cert.pem().as_bytes()).expect("parse root"))
		.build()
		.expect("build https client");
	let resp = tokio::time::timeout(TIMEOUT, client.get(format!("https://localhost:{port}/health")).send())
		.await
		.expect("https request timed out")
		.expect("https request failed");
	assert_eq!(resp.status(), reqwest::StatusCode::OK);

	// A listener that just served a request is not stalled, and the connections it
	// fielded were not junk.
	assert_eq!(health.stalled(), None);
	assert_eq!(health.failures(moq_tokio::accept::Failure::Exhausted), 0);

	handle.abort();
}

/// A client that dials a bare `host:port` with no path must still get a
/// WebSocket upgrade at the root, not the landing page. The empty path is the
/// root auth scope (same as the internal listener). Regression for the
/// `/{*path}`-only route, which left bare-URL clients (e.g.
/// `moqsink url="https://host:4443"`) with a silently dead WS fallback.
#[tokio::test]
async fn relay_websocket_root_path_upgrades() {
	let (port, web_handle) = spawn_relay().await;
	// No path: the URL is just host:port, so the WS handshake targets "/".
	let url: url::Url = format!("ws://127.0.0.1:{port}").parse().expect("parse url");

	// ── publisher ───────────────────────────────────────────────────
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_origin.consume()), url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed (root-path WS upgrade)");

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let (_client, sub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed (root-path WS upgrade)");

	// ── data path ───────────────────────────────────────────────────
	// The root auth scope is the empty path, so the broadcast announces at its
	// own name with no prefix.
	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = moq_net::Path::new(update.prefix.as_str()).to_owned();
	assert!(update.kind.is_active(), "expected announce, got retraction");
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request_broadcast(&path)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&frame.payload[..], b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_connection);
	drop(sub_connection);
	web_handle.abort();
}

/// Two publish-only clients (each `with_publisher`, no `with_subscriber`) coexist on one relay;
/// a single subscriber sees broadcasts forwarded from both. Verifies that multiple
/// publish-only connections don't interfere with each other or get torn down.
#[tokio::test]
async fn two_publish_only_clients_coexist() {
	let (port, web_handle) = spawn_relay().await;
	let url: url::Url = format!("ws://127.0.0.1:{port}/smoke").parse().expect("parse url");

	// ── two publish-only publishers, each serving a distinct broadcast ──
	let pub_a = moq_tokio::origin::spawn();
	let broadcast_a = pub_a.create_broadcast("alpha").expect("create broadcast a");
	broadcast_a.announce(Default::default()).expect("create broadcast a");
	let track_a = broadcast_a.create_track("video", None).expect("create track a");
	track_a
		.append_group()
		.expect("append group a")
		.write_frame(moq_net::Timestamp::ZERO, b"a".as_ref())
		.expect("write frame a");

	let pub_b = moq_tokio::origin::spawn();
	let broadcast_b = pub_b.create_broadcast("beta").expect("create broadcast b");
	broadcast_b.announce(Default::default()).expect("create broadcast b");
	let track_b = broadcast_b.create_track("video", None).expect("create track b");
	track_b
		.append_group()
		.expect("append group b")
		.write_frame(moq_net::Timestamp::ZERO, b"b".as_ref())
		.expect("write frame b");

	let (_client, sess_a) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_a.consume()), url.clone()),
	)
	.await
	.expect("publisher a connect timeout")
	.expect("publisher a connect failed");
	let (_client, sess_b) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_b.consume()), url.clone()),
	)
	.await
	.expect("publisher b connect timeout")
	.expect("publisher b connect failed");

	// ── one subscriber should see broadcasts from both publish-only clients ──
	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let (_client, sub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");

	let mut seen = std::collections::HashSet::new();
	while seen.len() < 2 {
		let update = tokio::time::timeout(TIMEOUT, announcements.next())
			.await
			.expect("announcement timeout")
			.expect("origin closed");
		if update.kind.is_active() {
			seen.insert(update.prefix.as_str().to_owned());
		}
	}
	assert!(
		seen.contains("alpha") && seen.contains("beta"),
		"expected both publish-only broadcasts, saw {seen:?}"
	);

	// Hold producers until announcements are observed.
	drop(track_a);
	drop(broadcast_a);
	drop(track_b);
	drop(broadcast_b);

	drop(sess_a);
	drop(sess_b);
	drop(sub_connection);
	web_handle.abort();
}

/// Run the relay's accept loop over the given server config, the same path
/// `main.rs` uses. Authenticates through the shared [`Auth`], here with fully
/// public access (`--auth-public ""`) so no-JWT clients get the root.
///
/// Returns the QUIC and TCP sockets the server bound, when it has them, so a
/// caller that asked for an ephemeral port can dial it.
async fn spawn_accept_relay(
	config: moq_tokio::listen::Config,
	auth_config: auth::Config,
) -> (
	Option<std::net::SocketAddr>,
	Option<std::net::SocketAddr>,
	tokio::task::JoinHandle<()>,
) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let server = config.init(Default::default()).expect("server init");

	let auth = auth_config
		.init("test", &moq_tokio::tls::Connect::default())
		.expect("auth init");

	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	let mut server = server.listen().await.expect("listen");
	let quic = server.local_addr().ok();
	let tcp = server.tcp_local_addr();

	let handle = tokio::spawn(async move {
		let mut id = 0;
		while let Some(request) = server.accept().await {
			let conn = Connection::new(request, cluster.clone(), auth.clone())
				.with_id(id)
				.with_shutdown(moq_relay::shutdown::Observer::disabled());
			id += 1;
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	});

	(quic, tcp, handle)
}

/// Stand up the relay listening only on a plain-TCP qmux `--server-bind` on a
/// ephemeral loopback port, with fully public auth (no-JWT => whole root). Returns
/// the port and an abort handle.
async fn spawn_internal_relay() -> (u16, tokio::task::JoinHandle<()>) {
	// Stream-only: a TCP listener with no `--server-bind`, so no QUIC.
	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));

	// Public Simple([""]) lets any no-JWT stream client through at the root.
	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];

	let (_, tcp, handle) = spawn_accept_relay(config, auth_config).await;
	(tcp.expect("relay bound no TCP socket").port(), handle)
}

/// Connect a publisher and subscriber to a stream `--server-bind` over `tcp://`
/// (plain TCP, no TLS, no JWT) and confirm a frame round-trips. Exercises the
/// qmux-over-TCP transport and no-JWT resolution through public auth.
#[tokio::test]
async fn internal_tcp_round_trip() {
	let (port, handle) = spawn_internal_relay().await;
	// The raw-TCP transport dials host:port only; any URL path is ignored.
	let url: url::Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let expected_version = newest_lite_version();

	// ── publisher ───────────────────────────────────────────────────
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_origin.consume()), url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");
	assert_eq!(
		pub_connection.version(),
		Some(expected_version),
		"publisher should negotiate the newest moq-lite version in-band over TCP"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let (_client, sub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	// The internal listener grants the empty root, so the broadcast announces
	// at its own name with no path prefix.
	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = moq_net::Path::new(update.prefix.as_str()).to_owned();
	assert!(update.kind.is_active(), "expected announce, got retraction");
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request_broadcast(&path)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&frame.payload[..], b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_connection);
	drop(sub_connection);
	handle.abort();
}

/// Stand up a stream `--server-bind` on a Unix socket and return the socket path
/// plus an abort handle.
#[cfg(unix)]
async fn spawn_internal_unix_relay() -> (std::path::PathBuf, tokio::task::JoinHandle<()>) {
	// Keep the path short: macOS caps AF_UNIX paths around 104 bytes, and the
	// system temp dir is long. /tmp is fine on macOS and Linux. A per-call counter
	// keeps concurrent tests in the same process off each other's socket.
	static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
	let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	let path = std::path::PathBuf::from(format!("/tmp/moq-internal-{}-{seq}.sock", std::process::id()));

	// Stream-only: a Unix listener with no `--server-bind`, so no QUIC.
	let mut config = moq_tokio::listen::Config::default();
	config.unix.bind = Some(path.clone());

	// Public Simple([""]) lets any no-JWT stream client through at the root.
	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];

	let (_, _, handle) = spawn_accept_relay(config, auth_config).await;
	(path, handle)
}

/// Connect over `unix://` (qmux on a Unix socket) and confirm a frame
/// round-trips. Also asserts both sides land on the newest moq-lite version,
/// which proves the in-band ALPN negotiation populated the protocol.
#[cfg(unix)]
#[tokio::test]
async fn internal_unix_round_trip() {
	let (path, handle) = spawn_internal_unix_relay().await;
	// `unix://` + an absolute path yields the triple-slash form the client expects.
	let url: url::Url = format!("unix://{}", path.display()).parse().expect("parse url");
	let expected_version = newest_lite_version();

	// ── publisher ───────────────────────────────────────────────────
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_origin.consume()), url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");
	assert_eq!(
		pub_connection.version(),
		Some(expected_version),
		"publisher should negotiate the newest moq-lite version in-band over the Unix socket"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let (_client, sub_connection) =
		tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(update.kind.is_active(), "expected announce, got retraction");
	let bc = tokio::time::timeout(TIMEOUT, sub_consumer.request_broadcast("test"))
		.await
		.expect("request timeout")
		.expect("announced broadcast resolves");

	let mut track_sub = tokio::time::timeout(TIMEOUT, async { bc.track("video").unwrap().subscribe(None).await })
		.await
		.expect("subscribe timeout")
		.expect("consume_track");
	let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed prematurely");
	assert_eq!(&frame.payload[..], b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_connection);
	drop(sub_connection);
	handle.abort();
}

/// Every version whose SETUP carries a request path the server reads: moq-lite-05/06
/// (Setup Stream) and moq-transport 14-18 (the `Path` SETUP parameter, in-band on
/// the bidi stream for 14-16 and the uni Setup Stream for 17-18).
fn path_versions() -> Vec<moq_net::Version> {
	[
		"moq-lite-05",
		"moq-lite-06",
		"moq-transport-14",
		"moq-transport-15",
		"moq-transport-16",
		"moq-transport-17",
		"moq-transport-18",
	]
	.iter()
	.map(|alpn| alpn.parse().expect("parse version alpn"))
	.collect()
}

/// Publisher and subscriber (both pinned to `version`) that announce/observe
/// `broadcast` over the internal listener at `pub_url` / `sub_url`. Returns the
/// path the subscriber sees the publisher's broadcast announced at, proving
/// whether the request path reached the server (it scopes the publisher's grant
/// to that root).
async fn path_round_trip(version: moq_net::Version, pub_url: url::Url, sub_url: url::Url, broadcast: &str) -> String {
	let pub_origin = moq_tokio::origin::spawn();
	let bc = pub_origin.create_broadcast(broadcast).expect("create broadcast");
	bc.announce(Default::default()).expect("create broadcast");
	let track = bc.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let pub_client = client_version(Some(version)).with_publisher(pub_origin.consume());
	let (_client, pub_connection) = tokio::time::timeout(TIMEOUT, connect_once(pub_client, pub_url))
		.await
		.expect("publisher connect timeout")
		.expect("publisher connect failed");

	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let sub_client = client_version(Some(version)).with_subscriber(sub_origin);
	let (_client, sub_connection) = tokio::time::timeout(TIMEOUT, connect_once(sub_client, sub_url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = moq_net::Path::new(update.prefix.as_str()).to_owned();

	drop(track);
	drop(bc);
	drop(pub_connection);
	drop(sub_connection);
	path.as_str().to_string()
}

/// A `tcp://host:port/<path>` client advertises `<path>` in the SETUP; the relay
/// scopes its grant to that root, so the publisher's broadcast lands prefixed.
/// Proves the request path can be specified and reaches the server over plain TCP,
/// across every version whose SETUP carries a path.
#[tokio::test]
async fn internal_tcp_path_reaches_server() {
	let (port, handle) = spawn_internal_relay().await;

	// Publisher addresses `/room`; subscriber addresses the bare root.
	let pub_url: url::Url = format!("tcp://127.0.0.1:{port}/room").parse().expect("parse url");
	let sub_url: url::Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");

	for version in path_versions() {
		let announced = path_round_trip(version, pub_url.clone(), sub_url.clone(), "test").await;
		assert_eq!(
			announced, "room/test",
			"the SETUP path should scope the publisher's grant ({version})"
		);
	}

	handle.abort();
}

/// `unix://<socket>` carries no resource path in its URL (that's the socket), so
/// the request path rides in the `?path=` query. Same assertion as TCP, across
/// every version whose SETUP carries a path.
#[cfg(unix)]
#[tokio::test]
async fn internal_unix_path_reaches_server() {
	let (path, handle) = spawn_internal_unix_relay().await;

	let pub_url: url::Url = format!("unix://{}?path=room", path.display())
		.parse()
		.expect("parse url");
	let sub_url: url::Url = format!("unix://{}", path.display()).parse().expect("parse url");

	for version in path_versions() {
		let announced = path_round_trip(version, pub_url.clone(), sub_url.clone(), "test").await;
		assert_eq!(
			announced, "room/test",
			"the SETUP path should scope the publisher's grant ({version})"
		);
	}

	handle.abort();
}

/// Stand up the relay listening only on a QUIC `--server-bind` on an ephemeral
/// loopback port, with fully public auth (no-JWT => whole root). Returns the bound
/// address and an abort handle.
async fn spawn_quic_relay() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];

	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];

	let (quic, _, handle) = spawn_accept_relay(config, auth_config).await;
	(quic.expect("relay bound no QUIC socket"), handle)
}

/// Raw QUIC has no request URI either, so `moqt://host:port/<path>` only reaches the
/// relay if the client puts it in the SETUP. Same assertion as TCP: the relay scopes
/// the publisher's grant to that root, across every version whose SETUP carries a path.
#[tokio::test]
async fn raw_quic_path_reaches_server() {
	let (addr, handle) = spawn_quic_relay().await;

	// Dialing an IP literal sends no SNI, so the SETUP is the only thing the server
	// has to go on.
	let pub_url: url::Url = format!("moqt://{addr}/room").parse().expect("parse url");
	let sub_url: url::Url = format!("moqt://{addr}").parse().expect("parse url");

	for version in path_versions() {
		let announced = path_round_trip(version, pub_url.clone(), sub_url.clone(), "test").await;
		assert_eq!(
			announced, "room/test",
			"the SETUP path should scope the publisher's grant ({version})"
		);
	}

	handle.abort();
}

/// `/health` is a liveness probe that always returns `200 ok`.
#[tokio::test]
async fn health_endpoint_reports_ok() {
	let (port, web_handle) = spawn_relay().await;

	let resp = tokio::time::timeout(TIMEOUT, reqwest::get(format!("http://127.0.0.1:{port}/health")))
		.await
		.expect("health request timeout")
		.expect("health request failed");

	assert_eq!(resp.status(), reqwest::StatusCode::OK);
	let body = resp.text().await.expect("health body");
	assert_eq!(body, "ok\n");

	web_handle.abort();
}

/// Stand up a stream relay whose public access grants **subscribe only**, returning
/// the TCP port and an abort handle. A no-JWT client gets the root for subscribing but
/// no publish scope, so a publisher's role is rejected.
async fn spawn_subscribe_only_relay() -> (u16, tokio::task::JoinHandle<()>) {
	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));

	// Subscribe-only public access: the root is granted for subscribing, never publishing.
	let mut auth_config = auth::Config::default();
	auth_config.public_subscribe = vec![moq_auth::Pattern::all()];

	let (_, tcp, handle) = spawn_accept_relay(config, auth_config).await;
	(tcp.expect("relay bound no TCP socket").port(), handle)
}

/// A publisher whose token grants only subscribe scope is rejected during the
/// handshake instead of being accepted and silently carrying no media. The client
/// advertises `Role::Publisher` in its SETUP (derived from `with_publisher`), and the
/// relay closes the session because the token has no publish scope. This is the
/// regression guard for moq.pro#338: before the role hint, this connection was
/// accepted and the publisher streamed into a dropped session forever.
#[tokio::test]
async fn subscribe_only_public_rejects_publisher_role() {
	let (port, handle) = spawn_subscribe_only_relay().await;
	let url: url::Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");

	let pub_origin = moq_tokio::origin::spawn();

	// The lite-05 client resolves `connect()` optimistically, so it may return Ok
	// before the relay's verdict lands. Either the connect fails outright, or the
	// session it returns closes shortly after with the relay's rejection. A correctly
	// scoped subscriber, by contrast, would stay open indefinitely.
	match tokio::time::timeout(
		TIMEOUT,
		connect_once(client().with_publisher(pub_origin.consume()), url),
	)
	.await
	{
		Ok(Ok((_client, connection))) => {
			let _ = tokio::time::timeout(TIMEOUT, connection.closed())
				.await
				.expect("relay should close a publisher whose token lacks publish scope, not leave it open");
		}
		Ok(Err(_)) => {} // rejected synchronously at connect; also acceptable.
		Err(_) => panic!("publisher connect neither resolved nor was rejected within the timeout"),
	}

	handle.abort();
}

/// The mirror of the reject test: a subscriber (`Role::Subscriber`, from `with_subscriber`)
/// is accepted by the same subscribe-only relay, and its session stays open. This proves
/// the role gate rejects only the mismatched direction, not the whole listener.
#[tokio::test]
async fn subscribe_only_public_accepts_subscriber_role() {
	let (port, handle) = spawn_subscribe_only_relay().await;
	let url: url::Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");

	let sub_origin = moq_tokio::origin::spawn();
	let (_client, connection) = tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	// The session must NOT be closed by the relay: a short wait should time out.
	let still_open = tokio::time::timeout(Duration::from_millis(500), connection.closed()).await;
	assert!(
		still_open.is_err(),
		"subscribe-only relay should keep a subscriber session open"
	);

	handle.abort();
}

/// The mirror of [`spawn_subscribe_only_relay`]: public access grants **publish only**,
/// so a no-JWT client gets the root for publishing but no subscribe scope.
async fn spawn_publish_only_relay() -> (u16, tokio::task::JoinHandle<()>) {
	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));

	// Publish-only public access: the root is granted for publishing, never subscribing.
	let mut auth_config = auth::Config::default();
	auth_config.public_publish = vec![moq_auth::Pattern::all()];

	let (_, tcp, handle) = spawn_accept_relay(config, auth_config).await;
	(tcp.expect("relay bound no TCP socket").port(), handle)
}

/// The mirror of the publisher-reject test, covering the other branch of the role gate:
/// a subscriber (`Role::Subscriber`, from `with_subscriber`) whose token grants only
/// publish scope is rejected during the handshake instead of left silently empty.
#[tokio::test]
async fn publish_only_public_rejects_subscriber_role() {
	let (port, handle) = spawn_publish_only_relay().await;
	let url: url::Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");

	let sub_origin = moq_tokio::origin::spawn();

	// Like the publisher case, `connect()` may resolve optimistically; either it fails
	// outright, or the session the relay hands back closes shortly after.
	match tokio::time::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url)).await {
		Ok(Ok((_client, connection))) => {
			let _ = tokio::time::timeout(TIMEOUT, connection.closed())
				.await
				.expect("relay should close a subscriber whose token lacks subscribe scope, not leave it open");
		}
		Ok(Err(_)) => {} // rejected synchronously at connect; also acceptable.
		Err(_) => panic!("subscriber connect neither resolved nor was rejected within the timeout"),
	}

	handle.abort();
}

/// Dial once and hand back the client with its connection.
///
/// These tests want a single transport, so reconnecting is off: there is nothing
/// left to redial, and dropping the connection closes the transport because it
/// holds the last session clone.
///
/// The client comes back because it owns the transport endpoint (iroh's dies with
/// it), and the caller has to outlive the connection it just got.
async fn connect_once(
	client: moq_tokio::Client,
	url: url::Url,
) -> moq_tokio::Result<(moq_tokio::Client, moq_tokio::Connection)> {
	let connection = client.clone().with_reconnect(false).connect(url).established().await?;
	Ok((client, connection))
}

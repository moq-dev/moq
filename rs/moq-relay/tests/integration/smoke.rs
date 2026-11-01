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

use crate::logs::Logs;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

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
	build_web_with(config, new_cluster()).await
}

/// A standalone cluster with default options.
fn new_cluster() -> cluster::Cluster {
	cluster::Cluster::new(cluster::Options::default()).expect("cluster init")
}

/// [`build_web`] for a test that configures the listeners or the cluster itself (e.g. HTTPS).
async fn build_web_with(web_config: web::Config, cluster: cluster::Cluster) -> web::Web {
	// Crypto provider is process-global; reinstalls after the first one are
	// no-ops, but the test binary may run before any other moq code does.
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// A public grant of `**` lets any path through.
	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];
	let auth = auth_config
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.expect("auth init");

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
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
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
	// Zero head start so the WebSocket path runs immediately.
	#[cfg(feature = "websocket")]
	{
		config.websocket.delay = Some((std::time::Duration::ZERO).into());
	}
	// Every relay in this file listens on IPv4 loopback, so bind the same family
	// rather than egressing a QUIC dial from a dual-stack IPv6 socket.
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	if let Some(version) = version {
		config.version = vec![version];
	}
	config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
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
	let track = broadcast.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) =
		moq_clock::timeout(TIMEOUT, connect_once(client().with_publisher(&pub_origin), url.clone()))
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
		moq_clock::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");
	assert_eq!(
		sub_connection.version(),
		Some(expected_version),
		"subscriber negotiated stale version"
	);

	// ── data path ───────────────────────────────────────────────────
	let (update, active) = moq_clock::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = update.prefix.clone();
	assert!(active, "expected announce, got retraction");
	// Auth root for `/smoke` is "smoke"; the broadcast "test" announces underneath.
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request(&path, None)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = moq_clock::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = moq_clock::timeout(TIMEOUT, group_sub.read_frame())
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

/// A WebSocket session takes its id from the cluster-wide counter and logs its close, like
/// a QUIC one, so the interop harness can pair its `conn{id=N}` span with that close.
#[tokio::test]
async fn relay_websocket_logs_its_close_under_a_cluster_id() {
	let logs = Logs::default();
	let _logging = logs.capture();

	let cluster = new_cluster();
	// Take an id as a QUIC accept would, so a WebSocket counter of its own would reuse it.
	let id = cluster.next_connection_id() + 1;
	let mut config = web::Config::default();
	config.ws = true;
	config.http.listen = Some("127.0.0.1:0".parse().expect("parse listen"));
	let web = build_web_with(config, cluster).await;
	let port = web.addrs().http.expect("HTTP listener is configured").port();
	let web_handle = tokio::spawn(async move { web.run().await.expect("relay web server") });

	let url: url::Url = format!("ws://127.0.0.1:{port}/smoke").parse().expect("parse url");
	let (client, connection) = moq_clock::timeout(
		TIMEOUT,
		connect_once(client().with_subscriber(moq_tokio::origin::spawn()), url),
	)
	.await
	.expect("connect timeout")
	.expect("connect failed");
	moq_clock::timeout(TIMEOUT, logs.wait_for(&format!("conn{{id={id} ")))
		.await
		.expect("the WebSocket session was not numbered on the cluster counter");

	let _ = connection.close().await;
	drop(client);
	moq_clock::timeout(TIMEOUT, logs.wait_for(&format!("connection closed id={id}")))
		.await
		.expect("the WebSocket session's close was not logged");

	assert!(logs.text().contains("connection closed"));
	web_handle.abort();
}

/// Read announcements until `until` shows up, returning every active prefix seen.
async fn announced_until(announcements: &mut moq_net::announce::Consumer, until: &str) -> Vec<String> {
	let mut seen = Vec::new();
	while !seen.iter().any(|prefix| prefix == until) {
		let event = moq_clock::timeout(TIMEOUT, announcements.next())
			.await
			.expect("announcement timeout")
			.expect("origin closed");
		if let moq_net::announce::Event::Start(announce) | moq_net::announce::Event::Update(announce) = event {
			seen.push(announce.prefix.as_str().to_owned());
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
	let track = hidden.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hidden".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");
	// The relay's announce request carries the opt-in only on lite-07, so the publisher
	// must speak it too for the hidden route to reach the relay.
	let (_pub_client, pub_connection) = moq_clock::timeout(
		TIMEOUT,
		connect_once(client_version(Some(lite07)).with_publisher(&pub_origin), url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	// An opted-in lite-07 client discovers the hidden broadcast.
	let opted_origin = moq_tokio::origin::spawn();
	let mut opted = opted_origin.consume().with_hidden(true).announced();
	let (_opted_client, opted_connection) = moq_clock::timeout(
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
		let (_client, connection) = moq_clock::timeout(
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
		let bc = consumer.request(".x/y", None).await.expect("hidden broadcast resolves");
		let mut sub = bc.track("video").unwrap().subscribe(None).await.expect("subscribe");
		let mut group = moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv_group timeout")
			.expect("recv_group failed")
			.expect("track closed");
		let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
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
	let allowed: moq_net::Version = "moq-transport-18".parse().expect("parse allowed version");
	let excluded = newest_lite_version();
	let (port, web_handle) = spawn_versioned_relay(vec![allowed]).await;
	let url: url::Url = format!("ws://127.0.0.1:{port}/smoke").parse().expect("parse url");

	let excluded_result = moq_clock::timeout(
		TIMEOUT,
		client_version(Some(excluded)).connect(url.clone()).established(),
	)
	.await
	.expect("excluded client connect timeout");
	assert!(
		excluded_result.is_err(),
		"WebSocket accepted excluded version {excluded} despite --server-version {allowed}"
	);

	let session = moq_clock::timeout(TIMEOUT, client_version(Some(allowed)).connect(url).established())
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
	let web = build_web_with(config, new_cluster()).await;
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
	let resp = moq_clock::timeout(TIMEOUT, client.get(format!("https://localhost:{port}/health")).send())
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
	let track = broadcast.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = moq_clock::timeout(
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
		moq_clock::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed (root-path WS upgrade)");

	// ── data path ───────────────────────────────────────────────────
	// The root auth scope is the empty path, so the broadcast announces at its
	// own name with no prefix.
	let (update, active) = moq_clock::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = update.prefix.clone();
	assert!(active, "expected announce, got retraction");
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request(&path, None)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = moq_clock::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = moq_clock::timeout(TIMEOUT, group_sub.read_frame())
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

/// Run the relay's accept loop over the given server config, the same path
/// `main.rs` uses. Authenticates through the shared [`Auth`], here with fully
/// public access (`--auth-public "**"`) so no-JWT clients get the root.
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
		.init("test", &moq_tokio::tls::Connect::default(), false)
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
				conn.run().await;
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

	// Public `**` lets any no-JWT stream client through at the root.
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
	let track = broadcast.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = moq_clock::timeout(
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
		moq_clock::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	// The internal listener grants the empty root, so the broadcast announces
	// at its own name with no path prefix.
	let (update, active) = moq_clock::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = update.prefix.clone();
	assert!(active, "expected announce, got retraction");
	assert_eq!(path.as_str(), "test");
	let bc = sub_consumer
		.request(&path, None)
		.await
		.expect("announced broadcast resolves");

	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("consume_track");
	let mut group_sub = moq_clock::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = moq_clock::timeout(TIMEOUT, group_sub.read_frame())
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
#[cfg(all(unix, feature = "uds"))]
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

	// Public `**` lets any no-JWT stream client through at the root.
	let mut auth_config = auth::Config::default();
	auth_config.public = vec![moq_auth::Pattern::all()];

	let (_, _, handle) = spawn_accept_relay(config, auth_config).await;
	(path, handle)
}

/// Connect over `unix://` (qmux on a Unix socket) and confirm a frame
/// round-trips. Also asserts both sides land on the newest moq-lite version,
/// which proves the in-band ALPN negotiation populated the protocol.
#[cfg(all(unix, feature = "uds"))]
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
	let track = broadcast.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let (_client, pub_connection) = moq_clock::timeout(
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
		moq_clock::timeout(TIMEOUT, connect_once(client().with_subscriber(sub_origin), url))
			.await
			.expect("subscriber connect timeout")
			.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	let (update, active) = moq_clock::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(active, "expected announce, got retraction");
	let bc = moq_clock::timeout(TIMEOUT, sub_consumer.request("test", None))
		.await
		.expect("request timeout")
		.expect("announced broadcast resolves");

	let mut track_sub = moq_clock::timeout(TIMEOUT, async { bc.track("video").unwrap().subscribe(None).await })
		.await
		.expect("subscribe timeout")
		.expect("consume_track");
	let mut group_sub = moq_clock::timeout(TIMEOUT, track_sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed prematurely");
	let frame = moq_clock::timeout(TIMEOUT, group_sub.read_frame())
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
/// (Setup Stream) and moq-transport 18 (the `Path` SETUP parameter on the uni Setup
/// Stream).
fn path_versions() -> Vec<moq_net::Version> {
	["moq-lite-05", "moq-lite-06", "moq-transport-18"]
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
	let track = bc.create_track("video", timed()).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let pub_client = client_version(Some(version)).with_publisher(pub_origin.consume());
	let (_client, pub_connection) = moq_clock::timeout(TIMEOUT, connect_once(pub_client, pub_url))
		.await
		.expect("publisher connect timeout")
		.expect("publisher connect failed");

	let sub_origin = moq_tokio::origin::spawn();
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
	let sub_client = client_version(Some(version)).with_subscriber(sub_origin);
	let (_client, sub_connection) = moq_clock::timeout(TIMEOUT, connect_once(sub_client, sub_url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	let (update, _) = moq_clock::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	let path = update.prefix.clone();

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
#[cfg(all(unix, feature = "uds"))]
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

	let resp = moq_clock::timeout(TIMEOUT, reqwest::get(format!("http://127.0.0.1:{port}/health")))
		.await
		.expect("health request timeout")
		.expect("health request failed");

	assert_eq!(resp.status(), reqwest::StatusCode::OK);
	let body = resp.text().await.expect("health body");
	assert_eq!(body, "ok\n");

	web_handle.abort();
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

/// The next route and whether it is active.
async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
	match announced.next().await? {
		moq_net::announce::Event::Start(route)
		| moq_net::announce::Event::Update(route)
		| moq_net::announce::Event::Restart(route)
		| moq_net::announce::Event::Takeover { announce: route, .. } => Some((route, true)),
		moq_net::announce::Event::End(route) => Some((route, false)),
	}
}

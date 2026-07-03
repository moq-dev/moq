//! End-to-end smoke test through a real moq-relay.
//!
//! Stands up the relay's actual axum + auth + cluster stack on a free port,
//! connects a publisher and a subscriber via WebSocket, and confirms that
//! a frame round-trips with the newest moq-lite version on both sides. The
//! version assertion is the regression guard for the
//! "axum-only-advertises-bare-`webtransport`" bug that silently downgraded
//! relay clients to moq-lite-02.

use std::{net::TcpListener, time::Duration};

use moq_native::moq_net::{self, Origin};
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig, Web, WebConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The newest moq-lite ALPN both sides should converge on. Derived from
/// `moq_net::ALPNS` so a future bump (e.g. lite-05 promoted out of WIP)
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

async fn build_web(port: u16, ws: bool) -> Web {
	// Crypto provider is process-global; reinstalls after the first one are
	// no-ops, but the test binary may run before any other moq code does.
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// AuthConfig with public Simple([""]) lets any path through. Simple is
	// deprecated but matches what `simple_public("")` in moq-relay's auth
	// tests uses, and the relay still honors it.
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth_config = AuthConfig::default();
	auth_config.public = Some(public);
	let auth = auth_config
		.init(&moq_native::tls::Client::default())
		.await
		.expect("auth init");

	let cluster = Cluster::new(ClusterConfig::default()).expect("cluster init");

	// moq_native::Server is needed for `tls_info`, even though we never
	// expose HTTPS or QUIC in this test. Binding QUIC to `[::]:0` picks an
	// unused UDP port that we ignore.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	let server = server_config.init().expect("server init");

	let mut web_config = WebConfig::default();
	web_config.ws = ws;
	web_config.http.listen = Some(format!("127.0.0.1:{port}").parse().expect("parse listen"));

	Web::new(auth, cluster, server.tls_info(), web_config)
}

fn free_tcp_port() -> u16 {
	// Pick a free port for HTTP, then immediately drop the probe listener
	// so axum_server can bind it. There's a tiny race window where the
	// kernel could hand the same port to another process, but on localhost
	// in a single-test process it's safe in practice.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

async fn wait_for_http(port: u16, server_result: &mut tokio::sync::oneshot::Receiver<anyhow::Result<()>>) {
	// Wait for axum_server to bind. A short poll is more reliable than a
	// fixed sleep when CI is slow.
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		match server_result.try_recv() {
			Ok(Ok(())) => panic!("relay web server exited before listening"),
			Ok(Err(err)) => panic!("relay web server failed before listening: {err:#}"),
			Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
			Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
				panic!("relay web server task ended before listening")
			}
		}
		if std::time::Instant::now() >= deadline {
			panic!("relay http listener never became ready on port {port}");
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

/// The shared bootstrap: stand up a relay listening on `127.0.0.1:<free-port>`
/// with fully public auth, and return the port plus an abort handle for the
/// spawned web server.
async fn spawn_relay() -> (u16, tokio::task::JoinHandle<()>) {
	let port = free_tcp_port();
	let web = build_web(port, true).await;

	let (server_result_tx, mut server_result_rx) = tokio::sync::oneshot::channel();
	let handle = tokio::spawn(async move {
		// `Web::run` only returns on error; in tests we abort it at teardown.
		let _ = server_result_tx.send(web.run().await);
	});

	wait_for_http(port, &mut server_result_rx).await;

	(port, handle)
}

fn client() -> moq_native::Client {
	client_version(None)
}

/// A client pinned to a single MoQ version, or all versions when `None`.
fn client_version(version: Option<moq_net::Version>) -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(true);
	// Zero head start so the WebSocket path runs immediately.
	config.websocket.delay = None;
	if let Some(version) = version {
		config.version = vec![version];
	}
	config.init().expect("client init")
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
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(TIMEOUT, client().with_publisher(&pub_origin).connect(url.clone()))
		.await
		.expect("publisher connect timeout")
		.expect("publisher connect failed");
	assert_eq!(
		pub_session.version(),
		expected_version,
		"publisher negotiated stale version"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();

	let sub_session = tokio::time::timeout(TIMEOUT, client().with_subscriber(sub_origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");
	assert_eq!(
		sub_session.version(),
		expected_version,
		"subscriber negotiated stale version"
	);

	// ── data path ───────────────────────────────────────────────────
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	// Auth root for `/smoke` is "smoke"; the broadcast "test" announces underneath.
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

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
	assert_eq!(&*frame, b"hello");

	// Hold the producers until after data is read; dropping them earlier
	// would close the publishing side of the broadcast.
	drop(track);
	drop(broadcast);

	drop(pub_session);
	drop(sub_session);
	web_handle.abort();
}

#[tokio::test]
async fn relay_web_serves_merged_routes() {
	tokio::time::pause();
	let port = free_tcp_port();
	let web = build_web(port, false).await;
	let app = web
		.routes()
		.route("/embedded", axum::routing::get(|| async { "embedded\n" }));

	let (server_result_tx, mut server_result_rx) = tokio::sync::oneshot::channel();
	let handle = tokio::spawn(async move {
		let _ = server_result_tx.send(web.serve(app).await);
	});

	wait_for_http(port, &mut server_result_rx).await;

	let body = reqwest::get(format!("http://127.0.0.1:{port}/embedded"))
		.await
		.expect("fetch embedded route")
		.text()
		.await
		.expect("read embedded response");
	assert_eq!(body, "embedded\n");

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
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client().with_publisher(pub_origin.consume()).connect(url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed (root-path WS upgrade)");

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();
	let sub_session = tokio::time::timeout(TIMEOUT, client().with_subscriber(sub_origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed (root-path WS upgrade)");

	// ── data path ───────────────────────────────────────────────────
	// The root auth scope is the empty path, so the broadcast announces at its
	// own name with no prefix.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

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
	assert_eq!(&*frame, b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_session);
	drop(sub_session);
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
	let pub_a = Origin::random().produce();
	let mut broadcast_a = pub_a.create_broadcast("alpha").expect("create broadcast a");
	let mut track_a = broadcast_a.create_track("video", None).expect("create track a");
	track_a
		.append_group()
		.expect("append group a")
		.write_frame_now(b"a".as_ref())
		.expect("write frame a");

	let pub_b = Origin::random().produce();
	let mut broadcast_b = pub_b.create_broadcast("beta").expect("create broadcast b");
	let mut track_b = broadcast_b.create_track("video", None).expect("create track b");
	track_b
		.append_group()
		.expect("append group b")
		.write_frame_now(b"b".as_ref())
		.expect("write frame b");

	let sess_a = tokio::time::timeout(TIMEOUT, client().with_publisher(pub_a.consume()).connect(url.clone()))
		.await
		.expect("publisher a connect timeout")
		.expect("publisher a connect failed");
	let sess_b = tokio::time::timeout(TIMEOUT, client().with_publisher(pub_b.consume()).connect(url.clone()))
		.await
		.expect("publisher b connect timeout")
		.expect("publisher b connect failed");

	// ── one subscriber should see broadcasts from both publish-only clients ──
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();
	let sub_session = tokio::time::timeout(TIMEOUT, client().with_subscriber(sub_origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	let mut seen = std::collections::HashSet::new();
	while seen.len() < 2 {
		let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
			.await
			.expect("announcement timeout")
			.expect("origin closed");
		if bc.broadcast().is_some() {
			seen.insert(path.as_str().to_owned());
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
	drop(sub_session);
	web_handle.abort();
}

/// Run the relay's accept loop over a stream-only server (no QUIC), the same path
/// `main.rs` uses. Authenticates through the shared [`Auth`], here with fully
/// public access (`--auth-public ""`) so no-JWT stream clients get the root.
async fn spawn_stream_relay(config: moq_native::ServerConfig) -> tokio::task::JoinHandle<()> {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut server = config.init().expect("server init");

	// Public Simple([""]) lets any no-JWT stream client through at the root.
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth_config = AuthConfig::default();
	auth_config.public = Some(public);
	let auth = auth_config
		.init(&moq_native::tls::Client::default())
		.await
		.expect("auth init");

	let cluster = Cluster::new(ClusterConfig::default()).expect("cluster init");

	tokio::spawn(async move {
		let mut id = 0;
		while let Some(request) = server.accept().await {
			let conn = Connection {
				id,
				request,
				cluster: cluster.clone(),
				auth: auth.clone(),
			};
			id += 1;
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	})
}

/// Stand up the relay listening only on a plain-TCP qmux `--server-bind` on a
/// free loopback port, with fully public auth (no-JWT => whole root). Returns
/// the port and an abort handle.
async fn spawn_internal_relay() -> (u16, tokio::task::JoinHandle<()>) {
	// Pick a free TCP port, then drop the probe so the listener can bind it.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	// Stream-only: a TCP listener with no `--server-bind`, so no QUIC.
	let mut config = moq_native::ServerConfig::default();
	config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	let handle = spawn_stream_relay(config).await;

	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		if std::time::Instant::now() >= deadline {
			panic!("internal listener never became ready on port {port}");
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

	(port, handle)
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
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client().with_publisher(pub_origin.consume()).connect(url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");
	assert_eq!(
		pub_session.version(),
		expected_version,
		"publisher should negotiate the newest moq-lite version in-band over TCP"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();
	let sub_session = tokio::time::timeout(TIMEOUT, client().with_subscriber(sub_origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	// The internal listener grants the empty root, so the broadcast announces
	// at its own name with no path prefix.
	let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

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
	assert_eq!(&*frame, b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_session);
	drop(sub_session);
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
	let mut config = moq_native::ServerConfig::default();
	config.unix.bind = Some(path.clone());
	let handle = spawn_stream_relay(config).await;

	// Wait for the socket file to appear.
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::UnixStream::connect(&path).await.is_ok() {
			break;
		}
		if std::time::Instant::now() >= deadline {
			panic!("internal Unix listener never became ready at {}", path.display());
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

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
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client().with_publisher(pub_origin.consume()).connect(url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");
	assert_eq!(
		pub_session.version(),
		expected_version,
		"publisher should negotiate the newest moq-lite version in-band over the Unix socket"
	);

	// ── subscriber ──────────────────────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();
	let sub_session = tokio::time::timeout(TIMEOUT, client().with_subscriber(sub_origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	// ── data path ───────────────────────────────────────────────────
	let (announced_path, bc) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(announced_path.as_str(), "test");
	let bc = bc.broadcast().expect("expected announce, got unannounce");

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
	assert_eq!(&*frame, b"hello");

	drop(track);
	drop(broadcast);
	drop(pub_session);
	drop(sub_session);
	handle.abort();
}

/// Every version whose SETUP carries a request path the server reads: moq-lite-05
/// (Setup Stream) and moq-transport 14-18 (the `Path` SETUP parameter, in-band on
/// the bidi stream for 14-16 and the uni Setup Stream for 17-18).
fn path_versions() -> Vec<moq_net::Version> {
	[
		"moq-lite-05-wip",
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
	let pub_origin = Origin::random().produce();
	let mut bc = pub_origin.create_broadcast(broadcast).expect("create broadcast");
	let mut track = bc.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group.write_frame_now(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_client = client_version(Some(version)).with_publisher(pub_origin.consume());
	let pub_session = tokio::time::timeout(TIMEOUT, pub_client.connect(pub_url))
		.await
		.expect("publisher connect timeout")
		.expect("publisher connect failed");

	let sub_origin = Origin::random().produce();
	let mut announcements = sub_origin.consume().announced();
	let sub_client = client_version(Some(version)).with_subscriber(sub_origin);
	let sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	let (path, _) = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");

	drop(track);
	drop(bc);
	drop(pub_session);
	drop(sub_session);
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

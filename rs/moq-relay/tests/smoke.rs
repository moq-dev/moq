//! End-to-end smoke test through a real moq-relay.
//!
//! Stands up the relay's actual axum + auth + cluster stack on a free port,
//! connects a publisher and a subscriber via WebSocket, and confirms that
//! a frame round-trips with the newest moq-lite version on both sides. The
//! version assertion is the regression guard for the
//! "axum-only-advertises-bare-`webtransport`" bug that silently downgraded
//! relay clients to moq-lite-02.

use std::{
	net::TcpListener,
	sync::atomic::AtomicU64,
	time::{Duration, Instant},
};

use moq_native::moq_net::{self, Origin};
use moq_relay::{Auth, AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig, Web, WebConfig, WebState};

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

/// The shared bootstrap: stand up a relay listening on `127.0.0.1:<free-port>`
/// with fully public auth, and return the port plus an abort handle for the
/// spawned web server.
async fn spawn_relay() -> (u16, tokio::task::JoinHandle<()>) {
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
	let auth = auth_config.init().await.expect("auth init");

	let cluster = Cluster::new(ClusterConfig::default());

	// moq_native::Server is needed for `tls_info`, even though we never
	// expose HTTPS or QUIC in this test. Binding QUIC to `[::]:0` picks an
	// unused UDP port that we ignore.
	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	let server = server_config.init().expect("server init");

	// Pick a free port for HTTP, then immediately drop the probe listener
	// so axum_server can bind it. There's a tiny race window where the
	// kernel could hand the same port to another process, but on localhost
	// in a single-test process it's safe in practice.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	let mut web_config = WebConfig::default();
	web_config.ws = true;
	web_config.http.listen = Some(format!("127.0.0.1:{port}").parse().expect("parse listen"));

	let web = Web::new(
		WebState {
			auth,
			cluster,
			tls_info: server.tls_info(),
			conn_id: AtomicU64::new(0),
			health: web_config.health.build(),
		},
		web_config,
	);

	let handle = tokio::spawn(async move {
		// `Web::run` only returns on error; in tests we abort it at teardown.
		let _ = web.run().await;
	});

	// Wait for axum_server to bind. A short poll is more reliable than a
	// fixed sleep when CI is slow.
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		if std::time::Instant::now() >= deadline {
			panic!("relay http listener never became ready on port {port}");
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

	(port, handle)
}

fn client() -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(true);
	// Zero head start so the WebSocket path runs immediately.
	config.websocket.delay = None;
	config.init().expect("client init")
}

/// A public [`Auth`] that lets any path through, matching [`spawn_relay`].
async fn public_auth() -> Auth {
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth_config = AuthConfig::default();
	auth_config.public = Some(public);
	auth_config.init().await.expect("auth init")
}

/// Stand up a real relay over WebTransport (QUIC) pinned to moq-lite-05-wip and
/// run its accept loop. Returns the `https://localhost:<port>` base URL and the
/// accept-loop handle. Lite-05 is required because the viewer count only rides
/// the wire on lite-05+, and (like the other lite-05 tests) lite-05-wip can only
/// be negotiated over WebTransport, not raw QUIC ALPN or WebSocket.
async fn spawn_quic_relay() -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let auth = public_auth().await;
	let cluster = Cluster::new(ClusterConfig::default());

	let mut server_config = moq_native::ServerConfig::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	server_config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	let mut server = server_config.init().expect("server init");
	let port = server.local_addr().expect("local addr").port();

	let handle = tokio::spawn(async move {
		let mut conn_id = 0;
		while let Some(request) = server.accept().await {
			let conn = Connection {
				id: conn_id,
				request,
				cluster: cluster.clone(),
				auth: auth.clone(),
			};
			conn_id += 1;
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	});

	(port, handle)
}

/// A lite-05-wip WebTransport client.
fn client_lite05() -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(true);
	config.version = vec!["moq-lite-05-wip".parse().unwrap()];
	config.init().expect("client init")
}

/// Subscribe to `broadcast`/`track` through the relay at `url`, returning the
/// session (keep it alive to hold the subscription) and the resolved subscriber.
async fn subscribe_through_relay(
	url: url::Url,
	broadcast: &str,
	track: &str,
) -> (moq_net::Session, moq_net::TrackSubscriber) {
	let origin = Origin::random().produce();
	let mut announced = origin.consume().announced();

	let session = tokio::time::timeout(TIMEOUT, client_lite05().with_consumer(origin).connect(url))
		.await
		.expect("subscriber connect timeout")
		.expect("subscriber connect failed");

	// Wait for the broadcast to be announced (relayed from the publisher).
	let bc = loop {
		let (path, bc) = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.expect("announce timeout")
			.expect("origin closed");
		if path.as_str() == broadcast {
			break bc.broadcast().expect("expected announce, got unannounce");
		}
	};

	let track_sub = tokio::time::timeout(TIMEOUT, bc.track(track).unwrap().subscribe(None).unwrap())
		.await
		.expect("subscribe timeout")
		.expect("subscribe failed");

	(session, track_sub)
}

/// Poll the producer's aggregate viewer count until it reaches `expected`.
async fn await_downstream(track: &moq_net::TrackProducer, expected: u64) {
	let deadline = Instant::now() + TIMEOUT;
	loop {
		let count = track.subscription().map_or(0, |s| s.downstream);
		if count == expected {
			return;
		}
		assert!(
			Instant::now() < deadline,
			"viewer count never reached {expected}; last saw {count}"
		);
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
}

/// Two subscribers behind one relay must telescope to a single upstream count of
/// `2` at the publisher, not be counted per-hop. Dropping one returns it to `1`.
#[tokio::test]
async fn relay_telescopes_downstream_viewer_count() {
	let (port, relay_handle) = spawn_quic_relay().await;
	let url: url::Url = format!("https://localhost:{port}/room").parse().expect("parse url");

	// ── publisher (a client serving a broadcast through the relay) ──────
	let pub_origin = Origin::random().produce();
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	// Hold the track producer in scope so we can read its aggregate subscription.
	let track = broadcast.create_track("video", None).expect("create track");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client_lite05().with_publisher(pub_origin.clone()).connect(url.clone()),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	// No subscribers yet.
	assert_eq!(track.subscription().map_or(0, |s| s.downstream), 0);

	// ── first viewer ───────────────────────────────────────────────────
	let (sub1, _track1) = subscribe_through_relay(url.clone(), "test", "video").await;
	await_downstream(&track, 1).await;

	// ── second viewer: the relay dedups to one upstream subscription, so
	//    the publisher sees a single count of 2 (telescoped), not 2 hops. ─
	let (sub2, track2) = subscribe_through_relay(url.clone(), "test", "video").await;
	await_downstream(&track, 2).await;

	// ── one viewer leaves: count telescopes back down to 1. ─────────────
	drop(track2);
	drop(sub2);
	await_downstream(&track, 1).await;

	drop(track);
	drop(broadcast);
	drop(sub1);
	drop(pub_session);
	relay_handle.abort();
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
	group.write_frame(b"hello".as_ref()).expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client().with_publisher(pub_origin.clone()).connect(url.clone()),
	)
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

	let sub_session = tokio::time::timeout(TIMEOUT, client().with_consumer(sub_origin).connect(url))
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

	let mut track_sub = bc
		.track("video")
		.unwrap()
		.subscribe(None)
		.unwrap()
		.await
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
	assert_eq!(&*frame, b"hello");

	// Hold the producers until after data is read; dropping them earlier
	// would close the publishing side of the broadcast.
	drop(track);
	drop(broadcast);

	drop(pub_session);
	drop(sub_session);
	web_handle.abort();
}

/// With no thresholds configured, `/health` is a pure liveness probe that
/// returns `200 ok`.
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

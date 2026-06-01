//! End-to-end smoke test through a real moq-relay.
//!
//! Stands up the relay's actual axum + auth + cluster stack on a free port,
//! connects a publisher and a subscriber via WebSocket, and confirms that
//! a frame round-trips with the newest moq-lite version on both sides. The
//! version assertion is the regression guard for the
//! "axum-only-advertises-bare-`webtransport`" bug that silently downgraded
//! relay clients to moq-lite-02.

use std::{net::TcpListener, sync::atomic::AtomicU64, time::Duration};

use moq_native::moq_net::{self, Origin, Track};
use moq_relay::{AuthConfig, Cluster, ClusterConfig, PublicConfig, Web, WebConfig, WebState};

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
	let mut track = broadcast.create_track(Track::new("video")).expect("create track");
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
		.consume_track("video")
		.subscribe(moq_native::moq_net::Subscription::default())
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

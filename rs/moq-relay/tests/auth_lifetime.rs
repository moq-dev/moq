//! End-to-end test of the session credential bound through a real moq-relay.
//!
//! Stands up the relay's axum WebSocket path (`serve_ws`), connects a publisher
//! and a subscriber with JWTs, confirms media flows, then asserts the relay
//! closes the live sessions once the credential stops being valid.

use std::{net::TcpListener, time::Duration};

use moq_relay::{AuthConfig, Cluster, ClusterConfig, Web, WebConfig};
use moq_token::{Algorithm, Key};
use moq_tokio::moq_net::{self, Origin};

const TIMEOUT: Duration = Duration::from_secs(10);

/// An auth stack verifying JWTs with `key` alone. The key file is re-read per
/// connection, so the returned guard must outlive the relay.
async fn build_auth(key: &Key) -> (moq_relay::Auth, tempfile::NamedTempFile) {
	let key_file = tempfile::NamedTempFile::new().expect("temp key file");
	key.to_file(key_file.path()).expect("write key");
	let mut auth_config = AuthConfig::default();
	auth_config.key = Some(key_file.path().to_string_lossy().into_owned());
	let auth = auth_config
		.init(&moq_tokio::tls::Connect::default())
		.await
		.expect("auth init");
	(auth, key_file)
}

/// Wait for a TCP listener to become dialable, or panic.
async fn wait_for_listener(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
		assert!(
			std::time::Instant::now() < deadline,
			"relay listener never became ready on port {port}"
		);
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

/// Stand up the relay's axum web stack with WebSocket enabled and return the
/// port plus an abort handle.
async fn spawn_ws_relay(auth: moq_relay::Auth) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	let cluster = Cluster::new(ClusterConfig::default()).expect("cluster init");

	// Stream listeners bind lazily, so this server never opens a socket; only
	// its certificate handle is used.
	let mut server_config = moq_tokio::listen::Config::default();
	server_config.bind = Some("[::]:0".to_string());
	server_config.tls.generate = vec!["localhost".into()];
	let certificates = server_config
		.init(Default::default())
		.expect("server init")
		.certificates();

	let mut web_config = WebConfig::default();
	web_config.ws = Some(true);
	web_config.http.listen = Some(format!("127.0.0.1:{port}").parse().expect("parse listen"));
	let web = Web::new(auth, cluster, certificates, web_config);

	let handle = tokio::spawn(async move {
		let _ = web.run().await;
	});

	wait_for_listener(port).await;

	(port, handle)
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.once = Some(true);
	config.websocket.delay = Duration::ZERO.into();
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

/// Full access under the `room` root.
fn room_claims() -> moq_token::Claims {
	moq_token::Claims::default()
		.with_root("room")
		.with_subscribe([""])
		.with_publish([""])
}

/// A relay URL for the `/room` root carrying a freshly minted JWT.
fn room_url(scheme: &str, port: u16, key: &Key, claims: &moq_token::Claims) -> url::Url {
	let jwt = key.sign(claims).expect("sign token");
	format!("{scheme}://127.0.0.1:{port}/room?jwt={jwt}")
		.parse()
		.expect("parse url")
}

/// Connect a publisher and a subscriber to `url` and prove one frame
/// round-trips. Returns both sessions so the caller can watch them close.
async fn connect_and_round_trip(url: &url::Url) -> (moq_tokio::Connection, moq_tokio::Connection) {
	let pub_origin = moq_tokio::origin::spawn(Origin::random());
	let mut broadcast = pub_origin
		.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_publisher(pub_origin.consume())
			.with_reconnect(false)
			.connect(url.clone())
			.established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	let sub_origin = moq_tokio::origin::spawn(Origin::random());
	let mut announcements = sub_origin.consume().announced();
	let sub_session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_subscriber(sub_origin)
			.with_reconnect(false)
			.connect(url.clone())
			.established(),
	)
	.await
	.expect("subscriber connect timeout")
	.expect("subscriber connect failed");

	let moq_net::announce::Update { path, broadcast: bc } = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(path.as_str(), "test");
	let bc = bc.expect("expected announce, got unannounce");

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

	(pub_session, sub_session)
}

/// WebSocket sessions close at their token's `exp`.
#[tokio::test]
async fn ws_expired_token_closes_live_sessions() {
	let key = Key::generate(Algorithm::HS256, None).expect("generate key");
	let (auth, _key_file) = build_auth(&key).await;
	let (port, relay) = spawn_ws_relay(auth).await;

	let claims = room_claims().with_expires(std::time::SystemTime::now() + Duration::from_secs(2));
	let admitted = std::time::Instant::now();
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("ws", port, &key, &claims)).await;

	let _ = tokio::time::timeout(Duration::from_secs(6), pub_session.closed())
		.await
		.expect("relay should close the publisher WS session once its token expires");
	let _ = tokio::time::timeout(Duration::from_secs(6), sub_session.closed())
		.await
		.expect("relay should close the subscriber WS session once its token expires");

	// `exp` has whole-second granularity, so the close lands in roughly [1.5s, 2.5s].
	let elapsed = admitted.elapsed();
	assert!(
		elapsed >= Duration::from_secs(1),
		"sessions closed before the token's exp: {elapsed:?}"
	);

	relay.abort();
}

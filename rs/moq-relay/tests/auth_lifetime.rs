//! End-to-end tests of the session credential bound through a real moq-relay.
//!
//! Stands up the relay's native accept loop (`Connection::run` over `tcp://`)
//! or its axum WebSocket path (`serve_ws` over `ws://`), connects a publisher
//! and a subscriber with JWTs, confirms media flows, then asserts the relay
//! closes the live sessions once the credential stops being valid.

use std::{net::TcpListener, time::Duration};

use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, Web, WebConfig};
use moq_token::{Algorithm, Key, KeyId};
use moq_tokio::moq_net::{self, Hop};
use wiremock::matchers::{method, path as path_matcher, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TIMEOUT: Duration = Duration::from_secs(10);
const KID: &str = "session-key";

/// The stub auth API's 200 response: the verifying key for [`KID`], cached for
/// one second so revalidation re-checks on that cadence.
async fn mount_valid_key(server: &MockServer, key: &Key) {
	Mock::given(method("GET"))
		.and(path_matcher("/auth"))
		.and(query_param("kid", KID))
		.respond_with(
			ResponseTemplate::new(200)
				.insert_header("Cache-Control", "max-age=1")
				.set_body_string(format!(r#"{{"key":{}}}"#, serde_json::to_string(key).unwrap())),
		)
		.mount(server)
		.await;
}

/// An auth stack on the stub auth API with revalidation enabled.
async fn build_api_auth(api: &MockServer) -> moq_relay::Auth {
	let mut auth_config = AuthConfig::default();
	auth_config.auth_api = Some(format!("{}/auth", api.uri()));
	auth_config
		.init(&moq_tokio::tls::Connect::default())
		.await
		.expect("auth init")
}

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

/// Stand up the relay's accept loop on a plain-TCP qmux listener and return the
/// port plus an abort handle.
async fn spawn_relay(auth: moq_relay::Auth) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	let server = config.init(Default::default()).expect("server init");
	let mut server = server.listen().await.expect("listen");
	let cluster = Cluster::new(ClusterConfig::default()).expect("cluster init");

	let handle = tokio::spawn(async move {
		let mut id = 0;
		while let Some(request) = server.accept().await {
			let conn = Connection::new(request, cluster.clone(), auth.clone()).with_id(id);
			id += 1;
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	});

	wait_for_listener(port).await;

	(port, handle)
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

/// The stub auth API's anonymous 200: full public access under `room`, cached
/// for one second so revalidation re-checks on that cadence.
async fn mount_public(server: &MockServer) {
	Mock::given(method("GET"))
		.and(path_matcher("/auth"))
		.respond_with(
			ResponseTemplate::new(200)
				.insert_header("Cache-Control", "max-age=1")
				.set_body_string(r#"{"public":{"subscribe":[""],"publish":[""]}}"#),
		)
		.mount(server)
		.await;
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
	let pub_origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let _announce_broadcast = pub_origin
		.announce("test", Default::default())
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

	let sub_origin = moq_tokio::origin::spawn(Hop::random());
	let sub_consumer = sub_origin.consume();
	let mut announcements = sub_consumer.announced();
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

	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_path().as_str(), "test");
	assert!(update.active, "expected announce, got retraction");
	let bc = sub_consumer
		.request_broadcast("test")
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

/// Withdrawing the grant closes live sessions on the next re-check.
#[tokio::test]
async fn revoked_grant_closes_live_sessions() {
	let api = MockServer::start().await;
	let key = Key::generate(Algorithm::HS256, Some(KeyId::decode(KID).unwrap())).expect("generate key");
	mount_valid_key(&api, &key).await;

	let (port, relay) = spawn_relay(build_api_auth(&api).await).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port, &key, &room_claims())).await;

	// Every subsequent auth-API call gets wiremock's 404.
	api.reset().await;

	let _ = tokio::time::timeout(Duration::from_secs(5), pub_session.closed())
		.await
		.expect("relay should close the publisher session after the grant is revoked");
	let _ = tokio::time::timeout(Duration::from_secs(5), sub_session.closed())
		.await
		.expect("relay should close the subscriber session after the grant is revoked");

	relay.abort();
}

/// WebSocket sessions close on revocation like the native accept path.
#[tokio::test]
async fn ws_revoked_grant_closes_live_sessions() {
	let api = MockServer::start().await;
	let key = Key::generate(Algorithm::HS256, Some(KeyId::decode(KID).unwrap())).expect("generate key");
	mount_valid_key(&api, &key).await;

	let (port, relay) = spawn_ws_relay(build_api_auth(&api).await).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("ws", port, &key, &room_claims())).await;

	api.reset().await;

	let _ = tokio::time::timeout(Duration::from_secs(5), pub_session.closed())
		.await
		.expect("relay should close the publisher WS session after the grant is revoked");
	let _ = tokio::time::timeout(Duration::from_secs(5), sub_session.closed())
		.await
		.expect("relay should close the subscriber WS session after the grant is revoked");

	relay.abort();
}

/// An ANONYMOUS session is revoked by the same re-check.
///
/// This is the case a "does the kid still resolve" check could never cover: a
/// public grant is built from claims with no `exp` at all, so before this the
/// relay held such a session until the peer hung up. Gating a project stopped
/// new admissions while its existing tokenless viewers kept drawing traffic.
#[tokio::test]
async fn revoked_public_grant_closes_anonymous_sessions() {
	let api = MockServer::start().await;
	mount_public(&api).await;

	let (port, relay) = spawn_relay(build_api_auth(&api).await).await;
	let url: url::Url = format!("tcp://127.0.0.1:{port}/room").parse().expect("parse url");
	let (pub_session, sub_session) = connect_and_round_trip(&url).await;

	// Withdraw `public`, exactly as a gated project's auth API does.
	api.reset().await;
	Mock::given(method("GET"))
		.and(path_matcher("/auth"))
		.respond_with(ResponseTemplate::new(200).set_body_string(r#"{"alias":"room"}"#))
		.mount(&api)
		.await;

	let _ = tokio::time::timeout(Duration::from_secs(5), pub_session.closed())
		.await
		.expect("relay should close the anonymous publisher once public access is withdrawn");
	let _ = tokio::time::timeout(Duration::from_secs(5), sub_session.closed())
		.await
		.expect("relay should close the anonymous subscriber once public access is withdrawn");

	relay.abort();
}

/// Replacing a key's MATERIAL under the same `kid` closes the sessions the old
/// key admitted, because the re-check reverifies the retained JWT against
/// whatever the API now returns. Checking only that some key exists for the kid
/// would leave every session the compromised key admitted running until `exp`.
#[tokio::test]
async fn rotated_key_closes_live_sessions() {
	let api = MockServer::start().await;
	let key = Key::generate(Algorithm::HS256, Some(KeyId::decode(KID).unwrap())).expect("generate key");
	mount_valid_key(&api, &key).await;

	let (port, relay) = spawn_relay(build_api_auth(&api).await).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port, &key, &room_claims())).await;

	// Same kid, different material.
	let replacement = Key::generate(Algorithm::HS256, Some(KeyId::decode(KID).unwrap())).expect("generate key");
	api.reset().await;
	mount_valid_key(&api, &replacement).await;

	let _ = tokio::time::timeout(Duration::from_secs(5), pub_session.closed())
		.await
		.expect("relay should close the publisher once its key is replaced");
	let _ = tokio::time::timeout(Duration::from_secs(5), sub_session.closed())
		.await
		.expect("relay should close the subscriber once its key is replaced");

	relay.abort();
}

//! Public rules are rooted at `/`, like a token with an empty root, through a real
//! relay: on the relay's own `--auth-public` and on `moq auth serve --public-*`
//! behind `--auth-url`.
//!
//! Every other test grants bare `**`, which reads the same whether rooted at `/` or
//! at the dialed path. These use `anon/**` and `event/**`, which do not: rooted at
//! the dialed path, `anon/**` at `/anon` meant `anon/anon/**`, and at `/rooms/123`
//! it let an anonymous client into `rooms/123/anon/**`.

use std::time::Duration;

use moq_relay::{auth, cluster, web};
use moq_tokio::moq_net;

const TIMEOUT: Duration = Duration::from_secs(10);

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.once = Some(true);
	config.websocket.delay = Duration::ZERO;
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

/// The relay's web stack with WebSocket and the HTTP routes, admitting through `auth`.
async fn spawn_relay(auth: auth::Config) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let auth = auth
		.init("test", &moq_tokio::tls::Connect::default())
		.expect("auth init");
	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");

	// Only the certificate handle is used; stream listeners bind lazily.
	let mut server_config = moq_tokio::listen::Config::default();
	server_config.bind = Some("[::]:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	let certificates = server_config
		.init(Default::default())
		.expect("server init")
		.certificates();

	let mut web_config = web::Config::default();
	web_config.ws = true;
	web_config.http.listen = Some("127.0.0.1:0".parse().expect("parse listen"));
	let web = web::Web::new(auth, cluster, certificates, web_config)
		.bind()
		.expect("bind web listener");
	let port = web.addrs().http.expect("HTTP listener is configured").port();
	let handle = tokio::spawn(async move {
		let _ = web.run().await;
	});
	(port, handle)
}

fn url(port: u16, path: &str) -> url::Url {
	format!("ws://127.0.0.1:{port}{path}").parse().expect("parse url")
}

/// Publish `name` at `url` with one open group holding `hello`. Keep the returned
/// handles alive for as long as the broadcast should stay announced.
async fn publish(url: url::Url, name: &str) -> Box<dyn std::any::Any> {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast(name).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	let session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_publisher(origin.consume())
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");
	Box::new((session, broadcast, track, group))
}

/// Subscribe at `url` and return the first broadcast announced, after reading one
/// frame of its `video` track.
async fn first_broadcast(url: url::Url) -> String {
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let mut announcements = consumer.announced();
	let _session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_subscriber(origin)
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("subscriber connect timeout")
	.expect("subscriber connect failed");

	let update = tokio::time::timeout(TIMEOUT, async {
		match announcements.next().await.expect("origin closed") {
			moq_net::announce::Event::Start(update) => update,
			other => panic!("expected announce, got {other:?}"),
		}
	})
	.await
	.expect("announcement timeout");
	let name = update.prefix.to_string();

	let broadcast = consumer.request_broadcast(&name).await.expect("broadcast resolves");
	let mut track = broadcast
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("subscribe");
	let mut group = tokio::time::timeout(TIMEOUT, track.recv_group())
		.await
		.expect("group timeout")
		.expect("group failed")
		.expect("track closed");
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timeout")
		.expect("frame failed")
		.expect("group closed");
	assert_eq!(&frame.payload[..], b"hello");
	name
}

/// A session the relay refuses never carries media: the transport may finish its
/// handshake before the verdict, so an established one has to close right away.
async fn assert_refused(url: url::Url) {
	let origin = moq_tokio::origin::spawn();
	let result = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_subscriber(origin)
			.with_reconnect(false)
			.connect(url.clone())
			.established(),
	)
	.await
	.expect("connect timeout");
	if let Ok(session) = result {
		let closed = tokio::time::timeout(Duration::from_secs(3), session.closed())
			.await
			.unwrap_or_else(|_| panic!("the relay admitted {url}"));
		assert!(closed.is_err(), "a refused session at {url} closed cleanly");
	}
}

fn anon() -> auth::Config {
	let mut config = auth::Config::default();
	config.public = vec!["anon/**".parse().unwrap()];
	config
}

#[tokio::test]
async fn anon_rules_admit_under_anon_and_refuse_elsewhere() {
	let (port, relay) = spawn_relay(anon()).await;

	let _publisher = publish(url(port, "/anon"), "test.hang").await;
	assert_eq!(first_broadcast(url(port, "/")).await, "anon/test.hang");
	assert_eq!(first_broadcast(url(port, "/anon")).await, "test.hang");

	// Rooted at the dialed path, this was `rooms/123/anon/**`.
	assert_refused(url(port, "/rooms/123")).await;
	assert_refused(url(port, "/other")).await;

	relay.abort();
}

#[tokio::test]
async fn a_leading_wildcard_does_not_reach_into_rooms() {
	let mut config = auth::Config::default();
	config.public_subscribe = vec!["*".parse().unwrap()];
	let (port, relay) = spawn_relay(config).await;

	// Rooted at the dialed path, `*` at `/rooms/123` was every broadcast in the room.
	assert_refused(url(port, "/rooms/123")).await;

	relay.abort();
}

#[tokio::test]
async fn http_routes_follow_the_anon_rules() {
	let (port, relay) = spawn_relay(anon()).await;
	let _publisher = publish(url(port, "/anon/bbb"), "cam").await;
	// The HTTP routes report only what has reached the relay.
	assert_eq!(first_broadcast(url(port, "/anon")).await, "bbb/cam");

	let http = reqwest::Client::new();
	let get = |path: &str| http.get(format!("http://127.0.0.1:{port}{path}")).send();

	let announced = get("/announced/anon").await.expect("announced request");
	assert_eq!(announced.status(), 200);
	assert_eq!(announced.text().await.expect("announced body").trim(), "bbb/cam");

	let mut fetch = get("/fetch/anon/bbb/cam/video").await.expect("fetch request");
	assert_eq!(fetch.status(), 200);
	let first = tokio::time::timeout(TIMEOUT, fetch.chunk())
		.await
		.expect("fetch timeout")
		.expect("fetch body")
		.expect("fetch body ended early");
	assert_eq!(&first[..], b"hello");

	for path in ["/announced/other", "/fetch/other/cam/video"] {
		assert_eq!(get(path).await.expect("request").status(), 401, "{path}");
	}

	relay.abort();
}

/// The reported upgrade: `moq auth serve --public-subscribe 'event/**'` behind
/// `--auth-url`, with a viewer at `/event` watching a camera published under a
/// `root=event` token. Rooted at the dialed path, the viewer saw `event/event/**`.
#[tokio::test]
async fn auth_server_public_rules_are_rooted_at_slash() {
	let dir = tempfile::tempdir().expect("tempdir");
	let key = moq_auth::Key::generate(moq_auth::Algorithm::HS256, None).expect("generate key");
	let key_path = dir.path().join("key.jwk");
	key.to_file(&key_path).expect("write key");

	let mut policy = moq_auth::serve::Policy::default();
	policy.keys = Some(moq_auth::serve::Keys::File(key_path));
	policy.public = moq_auth::Permissions::new(Default::default(), ["event/**".parse().unwrap()].into_iter().collect());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let server_url: url::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
	let server = moq_auth::serve::Server::new(policy).unwrap();
	tokio::spawn(async move { server.serve(listener).await });

	let mut config = auth::Config::default();
	config.url = Some(server_url);
	let (port, relay) = spawn_relay(config).await;

	let claims = moq_auth::Claims::default()
		.with_root("event")
		.with_publish(["**".parse().unwrap()]);
	let jwt = key.sign(&claims).expect("sign");
	let mut camera = url(port, "/event");
	camera.query_pairs_mut().append_pair("jwt", &jwt);
	let _publisher = publish(camera, "cam1.hang").await;

	assert_eq!(first_broadcast(url(port, "/event")).await, "cam1.hang");
	assert_refused(url(port, "/rooms/123")).await;

	relay.abort();
}

/// Public rules grant a certificate what they grant anyone, so a client CA on a
/// public-only relay is refused rather than left to verify certificates for nothing.
#[tokio::test]
async fn a_client_ca_needs_an_auth_server() {
	for web in [false, true] {
		let mut config = moq_relay::Config::default();
		config.auth = anon();
		match web {
			false => config.listen.tls.root = vec!["ca.pem".into()],
			true => config.web.https.root = vec!["ca.pem".into()],
		}
		let error = match moq_relay::Relay::load(config).await {
			Ok(_) => panic!("a client CA was accepted under --auth-public"),
			Err(error) => error.to_string(),
		};
		assert!(error.contains("--auth-public ignores"), "{error}");
	}
}

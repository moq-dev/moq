//! End-to-end tests of the lease a session holds, through a real moq-relay.
//!
//! Stands up the relay's native accept loop (`Connection::run` over `tcp://`
//! or QUIC) or its axum WebSocket path (`serve_ws` over `ws://`), points it at a
//! scripted auth server, connects a publisher and a subscriber, confirms media
//! flows, then asserts the relay follows the server's word: a re-check that moves
//! the tier keeps the session, a narrower grant or a refusal closes it, an outage
//! keeps it until `expires`, and every close reports `end` with what it moved.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_auth::{Event, Grant, Pattern, Patterns, Request};
use moq_relay::{AuthConfig, Cluster, ClusterOptions, Connection, Web, WebConfig};
use moq_tokio::moq_net::{self, Hop};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the scripted server answers next, per event.
#[derive(Clone)]
enum Answer {
	Grant(Grant),
	Status(u16),
}

/// A server whose answers the test changes mid-session, recording every request.
#[derive(Clone)]
struct Script {
	connect: Arc<Mutex<Answer>>,
	revalidate: Arc<Mutex<Answer>>,
	/// A re-check answer for one-shot HTTP sessions only, so a test can move a
	/// fetch without touching the sessions serving it.
	revalidate_http: Arc<Mutex<Option<Answer>>>,
	seen: Arc<Mutex<Vec<Request>>>,
}

impl Script {
	fn new(grant: Grant) -> Self {
		Self {
			connect: Arc::new(Mutex::new(Answer::Grant(grant.clone()))),
			revalidate: Arc::new(Mutex::new(Answer::Grant(grant))),
			revalidate_http: Arc::new(Mutex::new(None)),
			seen: Arc::new(Mutex::new(Vec::new())),
		}
	}

	fn on_connect(&self, answer: Answer) {
		*self.connect.lock().unwrap() = answer;
	}

	fn on_revalidate(&self, answer: Answer) {
		*self.revalidate.lock().unwrap() = answer;
	}

	fn on_revalidate_http(&self, answer: Answer) {
		*self.revalidate_http.lock().unwrap() = Some(answer);
	}

	fn ends(&self) -> Vec<Request> {
		self.seen
			.lock()
			.unwrap()
			.iter()
			.filter(|r| matches!(r.event, Event::End { .. }))
			.cloned()
			.collect()
	}

	async fn handle(State(script): State<Script>, Json(request): Json<Request>) -> Response {
		script.seen.lock().unwrap().push(request.clone());
		let answer = match request.event {
			Event::Connect => script.connect.lock().unwrap().clone(),
			Event::Revalidate if request.transport == moq_auth::Transport::Http => script
				.revalidate_http
				.lock()
				.unwrap()
				.clone()
				.unwrap_or_else(|| script.revalidate.lock().unwrap().clone()),
			Event::Revalidate => script.revalidate.lock().unwrap().clone(),
			Event::End { .. } => return StatusCode::NO_CONTENT.into_response(),
		};
		match answer {
			Answer::Grant(grant) => Json(grant).into_response(),
			Answer::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
		}
	}

	/// Serve on a loopback port for the test's lifetime, returning the URL.
	async fn spawn(&self) -> url::Url {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind auth");
		let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
		let app = Router::new().route("/", post(Self::handle)).with_state(self.clone());
		tokio::spawn(async move { axum::serve(listener, app).await });
		url
	}
}

fn all() -> Patterns {
	[Pattern::all()].into_iter().collect()
}

/// Everything under the dialed path, re-checked every second, good for `expires_in`.
fn grant(expires_in: Duration) -> Grant {
	let mut grant = Grant::new(all(), all());
	grant.expires = Some(SystemTime::now() + expires_in);
	grant.revalidate = Some(Duration::from_secs(1));
	grant
}

/// An `Auth` asking the server at `url`.
fn build_auth(url: url::Url) -> moq_relay::Auth {
	let mut config = AuthConfig::default();
	config.url = Some(url);
	config
		.init("test-relay", &moq_tokio::tls::Connect::default())
		.expect("auth init")
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

fn free_port() -> u16 {
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	probe.local_addr().expect("local addr").port()
}

/// Stand up the relay's accept loop on a plain-TCP qmux listener and return the
/// port plus an abort handle.
async fn spawn_relay(auth: moq_relay::Auth) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let port = free_port();

	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	let server = config.init(Default::default()).expect("server init");
	let mut server = server.listen().await.expect("listen");
	let cluster = Cluster::new(ClusterOptions::default()).expect("cluster init");

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
	let port = free_port();
	let cluster = Cluster::new(ClusterOptions::default()).expect("cluster init");

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
	web_config.ws = true;
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

fn room_url(scheme: &str, port: u16) -> url::Url {
	format!("{scheme}://127.0.0.1:{port}/room?jwt=token")
		.parse()
		.expect("parse url")
}

/// Connect a publisher and a subscriber to `url` and prove one frame
/// round-trips. Returns both sessions so the caller can watch them close.
async fn connect_and_round_trip(url: &url::Url) -> (moq_tokio::Connection, moq_tokio::Connection) {
	let pub_origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
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
	assert_eq!(update.pattern.as_prefix().expect("prefix announcement"), "test");
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

/// A connect the server refuses, or cannot answer, never carries a session: the
/// transport may complete its handshake before the relay's verdict, so a session
/// that establishes has to close right away.
async fn assert_refused(url: &url::Url) {
	assert_refused_with(client(), url).await;
}

async fn assert_refused_with(client: moq_tokio::Client, url: &url::Url) {
	let origin = moq_tokio::origin::spawn(Hop::random());
	let result = tokio::time::timeout(
		TIMEOUT,
		client
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
			.expect("the relay admitted a session the server refused");
		assert!(closed.is_err(), "a refused session closed cleanly");
	}
}

/// A QUIC relay, verifying client certificates against `root` when given.
async fn spawn_quic_relay(
	auth: moq_relay::Auth,
	root: Option<std::path::PathBuf>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".to_string());
	config.tls.generate = vec!["localhost".into()];
	config.tls.root = root.into_iter().collect();
	let server = config.init(Default::default()).expect("server init");
	let addr = server.local_addr().expect("quic addr");
	let mut server = server.listen().await.expect("listen");
	let cluster = Cluster::new(ClusterOptions::default()).expect("cluster init");
	let handle = tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			let conn = Connection::new(request, cluster.clone(), auth.clone());
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	});
	(addr, handle)
}

async fn assert_closed(session: moq_tokio::Connection, within: Duration, what: &str) {
	let _ = tokio::time::timeout(within, session.closed())
		.await
		.unwrap_or_else(|_| panic!("relay should close the {what} session"));
}

/// The scripted server admits, and every request carries what the relay knows.
#[tokio::test]
async fn admits_and_reports_the_session() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	let seen = script.seen.lock().unwrap().clone();
	let connect = seen.iter().find(|r| r.event == Event::Connect).expect("a connect");
	assert_eq!(connect.node, "test-relay");
	assert_eq!(connect.transport, moq_auth::Transport::Tcp);
	assert_eq!(connect.path, "/room");
	assert_eq!(connect.query.as_deref(), Some("jwt=token"));
	assert!(connect.remote.is_some_and(|addr| addr.ip().is_loopback()));
	assert!(connect.local.is_some_and(|addr| addr.port() == port));
	assert!(connect.tls.is_none());

	drop(pub_session);
	drop(sub_session);
	relay.abort();
}

/// A refusal at connect, a 5xx, and a garbage reply all refuse the session.
#[tokio::test]
async fn refusals_and_outages_refuse_at_connect() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;

	for status in [403, 500, 503] {
		script.on_connect(Answer::Status(status));
		assert_refused(&room_url("tcp", port)).await;
	}
	script.on_connect(Answer::Grant(Grant::default()));
	assert_refused(&room_url("tcp", port)).await;

	relay.abort();
}

/// A re-check that moves the tier keeps the session: the stats carriers resolve
/// their counters once at admission, so the new tier applies to the next one.
#[tokio::test]
async fn a_moved_tier_keeps_the_session() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	let mut moved = grant(Duration::from_secs(3600));
	moved.tier = Some("moved".into());
	script.on_revalidate(Answer::Grant(moved));

	tokio::time::sleep(Duration::from_millis(2500)).await;
	assert!(
		script.seen.lock().unwrap().iter().any(|r| r.event == Event::Revalidate),
		"the relay re-checked"
	);
	assert!(
		tokio::time::timeout(Duration::from_millis(200), pub_session.closed())
			.await
			.is_err(),
		"a tier change must not close the publisher"
	);
	assert!(
		tokio::time::timeout(Duration::from_millis(200), sub_session.closed())
			.await
			.is_err(),
		"a tier change must not close the subscriber"
	);

	relay.abort();
}

/// A narrower grant closes the session on the next re-check, over TCP and WebSocket.
#[tokio::test]
async fn a_narrower_grant_closes_live_sessions() {
	for scheme in ["tcp", "ws"] {
		let script = Script::new(grant(Duration::from_secs(3600)));
		let auth = build_auth(script.spawn().await);
		let (port, relay) = match scheme {
			"tcp" => spawn_relay(auth).await,
			_ => spawn_ws_relay(auth).await,
		};
		let (pub_session, sub_session) = connect_and_round_trip(&room_url(scheme, port)).await;

		let mut narrow = grant(Duration::from_secs(3600));
		narrow.publish = ["nobody/**".parse().unwrap()].into_iter().collect();
		script.on_revalidate(Answer::Grant(narrow));

		assert_closed(pub_session, Duration::from_secs(5), &format!("{scheme} publisher")).await;
		assert_closed(sub_session, Duration::from_secs(5), &format!("{scheme} subscriber")).await;
		relay.abort();
	}
}

/// A refusal on re-check closes the session, and the `end` says why.
#[tokio::test]
async fn a_refusal_closes_live_sessions() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	script.on_revalidate(Answer::Status(403));

	assert_closed(pub_session, Duration::from_secs(5), "publisher").await;
	assert_closed(sub_session, Duration::from_secs(5), "subscriber").await;

	tokio::time::sleep(Duration::from_millis(200)).await;
	let ends = script.ends();
	assert!(ends.len() >= 2, "an end per session, got {}", ends.len());
	for end in &ends {
		let Event::End { reason, .. } = &end.event else {
			unreachable!()
		};
		assert_eq!(*reason, moq_auth::lease::Reason::Refused);
	}

	relay.abort();
}

/// The one-shot HTTP routes are sessions of their own: `/announced` is admitted
/// as `http` and ended when it answers, and `/fetch` holds its lease for as long
/// as the body streams, so a refusal on re-check cuts the transfer and the `end`
/// says why.
#[tokio::test]
async fn http_routes_hold_a_lease() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_ws_relay(build_auth(script.spawn().await)).await;

	// A publisher whose group stays open, so a fetch of it keeps streaming.
	let pub_origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	let pub_session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_publisher(pub_origin.consume())
			.with_reconnect(false)
			.connect(room_url("ws", port))
			.established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	// Wait until the announcement reaches the relay before asking over HTTP:
	// the /announced handler only reports what has arrived so far.
	let sub_origin = moq_tokio::origin::spawn(Hop::random());
	let mut announcements = sub_origin.consume().announced();
	let _sub_session = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_subscriber(sub_origin)
			.with_reconnect(false)
			.connect(room_url("ws", port))
			.established(),
	)
	.await
	.expect("subscriber connect timeout")
	.expect("subscriber connect failed");
	let update = tokio::time::timeout(TIMEOUT, announcements.next())
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.pattern.as_prefix().expect("prefix announcement"), "test");
	assert!(update.active, "expected announce, got retraction");

	let http = reqwest::Client::new();
	let announced = http
		.get(format!("http://127.0.0.1:{port}/announced/room?jwt=token"))
		.send()
		.await
		.expect("announced request");
	assert_eq!(announced.status(), 200);
	assert_eq!(announced.text().await.expect("announced body").trim(), "test");

	let connects: Vec<Request> = script
		.seen
		.lock()
		.unwrap()
		.iter()
		.filter(|r| r.event == Event::Connect && r.transport == moq_auth::Transport::Http)
		.cloned()
		.collect();
	assert_eq!(connects.len(), 1, "one http session for the announced request");
	assert_eq!(connects[0].path, "/room");
	assert_eq!(connects[0].query.as_deref(), Some("jwt=token"));
	assert!(connects[0].remote.is_some_and(|addr| addr.ip().is_loopback()));
	let ends = |script: &Script| -> Vec<Request> {
		script
			.ends()
			.into_iter()
			.filter(|r| r.transport == moq_auth::Transport::Http)
			.collect()
	};
	let end_reason = |request: &Request| match &request.event {
		Event::End { reason, .. } => reason.clone(),
		_ => unreachable!(),
	};
	tokio::time::sleep(Duration::from_millis(200)).await;
	let done = ends(&script);
	assert_eq!(done.len(), 1, "the announced session ended");
	assert_eq!(end_reason(&done[0]), "done".into());

	let mut fetch = http
		.get(format!("http://127.0.0.1:{port}/fetch/room/test/video?jwt=token"))
		.send()
		.await
		.expect("fetch request");
	assert_eq!(fetch.status(), 200);
	let first = tokio::time::timeout(TIMEOUT, fetch.chunk())
		.await
		.expect("first frame timeout")
		.expect("first frame")
		.expect("body ended early");
	assert_eq!(&first[..], b"hello");

	// The body is still streaming, so the fetch's lease is still held.
	tokio::time::sleep(Duration::from_millis(200)).await;
	assert_eq!(ends(&script).len(), 1, "the fetch must not end while its body streams");

	// A refusal on the fetch's re-check cuts the transfer; the publisher is untouched.
	script.on_revalidate_http(Answer::Status(403));
	let cut = tokio::time::timeout(Duration::from_secs(5), async {
		loop {
			match fetch.chunk().await {
				Ok(Some(_)) => continue,
				other => break other,
			}
		}
	})
	.await
	.expect("the refused fetch kept streaming");
	assert!(cut.is_err(), "a refused fetch must not end cleanly: {cut:?}");

	tokio::time::sleep(Duration::from_millis(200)).await;
	let ended = ends(&script);
	assert_eq!(ended.len(), 2, "the fetch session ended");
	assert_eq!(end_reason(&ended[1]), moq_auth::lease::Reason::Refused);
	assert!(
		tokio::time::timeout(Duration::from_millis(200), pub_session.closed())
			.await
			.is_err(),
		"refusing the fetch must not close the publisher"
	);

	drop(group);
	drop(track);
	drop(broadcast);
	relay.abort();
}

/// An outage keeps the session until `expires`, then closes it as expired.
#[tokio::test]
async fn an_outage_keeps_the_session_until_expires() {
	let script = Script::new(grant(Duration::from_secs(4)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
	let admitted = std::time::Instant::now();
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	script.on_revalidate(Answer::Status(503));

	// Well into the outage the session is still up...
	tokio::time::sleep(Duration::from_millis(2000)).await;
	assert!(
		tokio::time::timeout(Duration::from_millis(100), pub_session.closed())
			.await
			.is_err(),
		"an outage must not close the publisher before expires"
	);

	// ...and it closes once the grant expires, not later.
	assert_closed(pub_session, Duration::from_secs(4), "publisher").await;
	assert_closed(sub_session, Duration::from_secs(4), "subscriber").await;
	let elapsed = admitted.elapsed();
	assert!(elapsed >= Duration::from_secs(3), "closed before expires: {elapsed:?}");

	tokio::time::sleep(Duration::from_millis(200)).await;
	for end in script.ends() {
		let Event::End { reason, .. } = &end.event else {
			unreachable!()
		};
		assert_eq!(*reason, moq_auth::lease::Reason::Expired);
	}
	relay.abort();
}

/// A session the client closes reports `end` with its duration and byte counters.
/// Over QUIC, the one transport whose connection reports its totals.
#[tokio::test]
async fn the_end_carries_duration_and_bytes() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (addr, relay) = spawn_quic_relay(build_auth(script.spawn().await), None).await;
	let url: url::Url = format!("moql://127.0.0.1:{}/room?jwt=token", addr.port())
		.parse()
		.unwrap();
	let (pub_session, sub_session) = connect_and_round_trip(&url).await;

	tokio::time::sleep(Duration::from_millis(300)).await;
	drop(pub_session);
	drop(sub_session);

	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	let ends = loop {
		let ends = script.ends();
		if ends.len() >= 2 {
			break ends;
		}
		assert!(
			std::time::Instant::now() < deadline,
			"ends never arrived: {}",
			ends.len()
		);
		tokio::time::sleep(Duration::from_millis(50)).await;
	};
	for end in &ends {
		let Event::End { duration, bytes, .. } = &end.event else {
			unreachable!()
		};
		assert!(*duration >= Duration::from_millis(200), "duration {duration:?}");
		assert!(bytes.sent > 0 && bytes.received > 0, "bytes {bytes:?}");
	}

	relay.abort();
}

/// A certificate is a fact: with no server grant for it, a verified peer is
/// refused; with a narrow one, it is scoped to that and nothing more. Over QUIC
/// with a client certificate, and over WebSocket where none can be presented.
#[tokio::test]
async fn a_certificate_admits_only_what_the_server_grants() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let (root, client_cert, client_key) = signed_client(dir.path());

	let policy = |rules: moq_auth::serve::Rules| {
		let mut policy = moq_auth::serve::Policy::default();
		policy.mtls = rules;
		policy
	};
	let serve = |policy: moq_auth::serve::Policy| async move {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url: url::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
		let server = moq_auth::serve::Server::new(policy);
		tokio::spawn(async move { server.serve(listener).await });
		url
	};

	let mtls_client = || {
		let mut config = moq_tokio::connect::Config::default();
		config.tls.insecure = Some(true);
		config.once = Some(true);
		config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		config.tls.cert = Some(client_cert.clone());
		config.tls.key = Some(client_key.clone());
		config.init(Default::default()).expect("client init")
	};
	// No grant for certificates: refused, over QUIC with a certificate and over
	// WebSocket without one.
	let none = serve(policy(moq_auth::serve::Rules::default())).await;
	let (addr, relay) = spawn_quic_relay(build_auth(none.clone()), Some(root.clone())).await;
	let url: url::Url = format!("moql://127.0.0.1:{}/room", addr.port()).parse().unwrap();
	assert_refused_with(mtls_client(), &url).await;
	relay.abort();
	let (port, relay) = spawn_ws_relay(build_auth(none)).await;
	assert_refused(&format!("ws://127.0.0.1:{port}/room").parse().unwrap()).await;
	relay.abort();

	// A narrow grant: the certificate publishes under `mine/**` and nothing else.
	let narrow = serve(policy(moq_auth::serve::Rules::new(
		["mine/**".parse().unwrap()].into_iter().collect(),
		Patterns::new(),
	)))
	.await;
	let (addr, relay) = spawn_quic_relay(build_auth(narrow), Some(root.clone())).await;
	let url: url::Url = format!("moql://127.0.0.1:{}/room", addr.port()).parse().unwrap();
	let origin = moq_tokio::origin::spawn(Hop::random());
	let session = tokio::time::timeout(
		TIMEOUT,
		mtls_client()
			.with_publisher(origin.consume())
			.with_reconnect(false)
			.connect(url.clone())
			.established(),
	)
	.await
	.expect("connect timeout")
	.expect("a scoped certificate is admitted");
	assert!(
		tokio::time::timeout(Duration::from_secs(2), session.closed())
			.await
			.is_err(),
		"the scoped publisher stays admitted"
	);
	// A subscribe-only client has nothing granted, so it is refused at the handshake.
	assert_refused_with(mtls_client(), &url).await;
	drop(session);
	relay.abort();
}

fn signed_client(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
	let ca_key = rcgen::KeyPair::generate().expect("ca keypair");
	let mut ca_params = rcgen::CertificateParams::new(Vec::new()).expect("ca params");
	ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
	ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
	ca_params
		.distinguished_name
		.push(rcgen::DnType::CommonName, "moq test ca");
	let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("self-signed ca");

	let key = rcgen::KeyPair::generate().expect("client keypair");
	let mut params = rcgen::CertificateParams::new(vec!["client.localhost".to_string()]).expect("client params");
	params.use_authority_key_identifier_extension = true;
	params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
	let cert = params.signed_by(&key, &ca).expect("signed client cert");

	let root_path = dir.join("ca.pem");
	let cert_path = dir.join("client.pem");
	let key_path = dir.join("client.key.pem");
	std::fs::write(&root_path, ca.pem()).expect("write ca");
	std::fs::write(&cert_path, cert.pem()).expect("write client cert");
	std::fs::write(&key_path, key.serialize_pem()).expect("write client key");
	(root_path, cert_path, key_path)
}

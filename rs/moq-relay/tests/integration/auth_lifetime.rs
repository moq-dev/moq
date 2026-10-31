//! End-to-end tests of the lease a session holds, through a real moq-relay.
//!
//! Stands up the relay's native accept loop (`Connection::run` over `tcp://`
//! or QUIC) or its axum WebSocket path (`serve_ws` over `ws://`), points it at a
//! scripted auth server, connects a publisher and a subscriber, confirms media
//! flows, then asserts the relay follows the server's word: a re-check that moves
//! the tier retags the live session's stats, a changed grant re-authorizes it in
//! place, a moved root or a refusal closes it, and every close reports `end` with
//! what it moved.
//! The last tests swap the server for an in-process decider answering
//! `Admissions`, and prove the lease it drives reaches the session the same way,
//! including an outage that keeps it until exactly `expires`, on a clock only
//! the test moves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_auth::{Event, Grant, Pattern, Patterns, Request};
use moq_relay::{Config, Connection, Relay, auth, cluster, web};
use moq_tokio::moq_net;
use moq_tokio::moq_net::stats;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the scripted server answers next, per event.
#[derive(Clone)]
enum Answer {
	Grant(Grant),
	Status(u16),
	Token(Grant, moq_auth::Token),
	/// A 403 carrying a typed refusal.
	Refuse(moq_auth::refusal::Reason),
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
	/// Answers each `end` only once a permit is added, when [`Self::hold_ends`] set it.
	end_gate: Arc<Mutex<Option<Arc<tokio::sync::Semaphore>>>>,
	/// Notified as each `end` arrives.
	end_arrived: Arc<tokio::sync::Notify>,
}

impl Script {
	fn new(grant: Grant) -> Self {
		Self {
			connect: Arc::new(Mutex::new(Answer::Grant(grant.clone()))),
			revalidate: Arc::new(Mutex::new(Answer::Grant(grant))),
			revalidate_http: Arc::new(Mutex::new(None)),
			seen: Arc::new(Mutex::new(Vec::new())),
			end_gate: Arc::default(),
			end_arrived: Arc::default(),
		}
	}

	/// Hold every `end` reply until the returned gate gets a permit.
	fn hold_ends(&self) -> Arc<tokio::sync::Semaphore> {
		let gate = Arc::new(tokio::sync::Semaphore::new(0));
		*self.end_gate.lock().unwrap() = Some(gate.clone());
		gate
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
			Event::End { .. } => {
				script.end_arrived.notify_one();
				let gate = script.end_gate.lock().unwrap().clone();
				if let Some(gate) = gate {
					drop(gate.acquire().await);
				}
				return StatusCode::NO_CONTENT.into_response();
			}
		};
		match answer {
			Answer::Grant(grant) => Json(grant).into_response(),
			Answer::Token(grant, token) if request.token.as_ref() == Some(&token) => Json(grant).into_response(),
			Answer::Token(..) => StatusCode::UNAUTHORIZED.into_response(),
			Answer::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
			Answer::Refuse(reason) => {
				let mut refusal = moq_auth::Refusal::default();
				refusal.reason = reason;
				(StatusCode::FORBIDDEN, Json(refusal)).into_response()
			}
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
	grant.expires = Some(moq_clock::wall() + expires_in);
	grant.revalidate = Some(Duration::from_secs(1));
	grant
}

/// An `Auth` asking the server at `url`.
fn build_auth(url: url::Url) -> moq_relay::auth::Auth {
	let mut config = auth::Config::default();
	config.url = Some(url);
	config
		.init("test-relay", &moq_tokio::tls::Connect::default(), false)
		.expect("auth init")
}

/// Stand up the relay's accept loop on a plain-TCP qmux listener and return the
/// port plus an abort handle.
async fn spawn_relay(auth: moq_relay::auth::Auth) -> (u16, tokio::task::JoinHandle<()>) {
	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	spawn_relay_with(auth, cluster).await
}

/// [`spawn_relay`] serving `cluster`.
async fn spawn_relay_with(
	auth: moq_relay::auth::Auth,
	cluster: cluster::Cluster,
) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut config = moq_tokio::listen::Config::default();
	config.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	// Opt in to lite-07 beside the defaults, so a client pinned to it can reach the AUTH stream.
	config.version = moq_net::Versions::all()
		.iter()
		.copied()
		.chain(["moq-lite-07-wip".parse().unwrap()])
		.collect();
	let server = config.init(Default::default()).expect("server init");
	let mut server = server.listen().await.expect("listen");
	let port = server.tcp_local_addr().expect("TCP listener is configured").port();

	let handle = tokio::spawn(async move {
		let mut id = 0;
		while let Some(request) = server.accept().await {
			let conn = Connection::new(request, cluster.clone(), auth.clone()).with_id(id);
			id += 1;
			tokio::spawn(async move {
				conn.run().await;
			});
		}
	});

	(port, handle)
}

/// Stand up the relay's axum web stack with WebSocket enabled and return the
/// port plus an abort handle.
async fn spawn_ws_relay(auth: moq_relay::auth::Auth) -> (u16, tokio::task::JoinHandle<()>) {
	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	spawn_ws_relay_with(auth, cluster).await
}

async fn spawn_ws_relay_with(
	auth: moq_relay::auth::Auth,
	cluster: cluster::Cluster,
) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// Stream listeners bind lazily, so this server never opens a socket; only
	// its certificate handle is used.
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

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	#[cfg(feature = "websocket")]
	{
		config.websocket.delay = Some((Duration::ZERO).into());
	}
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
}

fn room_url(scheme: &str, port: u16) -> url::Url {
	format!("{scheme}://127.0.0.1:{port}/room?jwt=token")
		.parse()
		.expect("parse url")
}

/// Connect a publisher and a subscriber to `url` and prove one frame
/// round-trips. Returns both sessions so the caller can watch them close.
async fn connect_and_round_trip(url: &url::Url) -> (moq_tokio::Connection, moq_tokio::Connection) {
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");
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

	let sub_origin = moq_tokio::origin::spawn();
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

	let (update, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(active, "expected announce, got retraction");
	let bc = sub_consumer
		.request("test", None)
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
	let origin = moq_tokio::origin::spawn();
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
	auth: moq_relay::auth::Auth,
	root: Option<std::path::PathBuf>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	spawn_quic_relay_with(auth, root, cluster).await
}

/// [`spawn_quic_relay`] serving `cluster`.
async fn spawn_quic_relay_with(
	auth: moq_relay::auth::Auth,
	root: Option<std::path::PathBuf>,
	cluster: cluster::Cluster,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	config.tls.root = root.into_iter().collect();
	let server = config.init(Default::default()).expect("server init");
	let addr = server.local_addr().expect("quic addr");
	let mut server = server.listen().await.expect("listen");
	let handle = tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			let conn = Connection::new(request, cluster.clone(), auth.clone());
			tokio::spawn(async move {
				conn.run().await;
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
	assert!(connect.token.is_none(), "moq-lite carries no SETUP token");

	drop(pub_session);
	drop(sub_session);
	relay.abort();
}

/// Write all of `data` to a raw qmux stream.
async fn write_all(send: &mut qmux::SendStream, mut data: &[u8]) {
	use moq_net::transport::poll::SendStream as _;
	while !data.is_empty() {
		let n = send.write(data).await.expect("write");
		data = &data[n..];
	}
}

/// A moq-transport client's SETUP token reaches the auth server byte for byte.
#[tokio::test]
async fn forwards_the_setup_token() {
	use moq_net::transport::poll::{RecvStream as _, Session as _};

	for websocket in [false, true] {
		let value = [0x00, 0xff, 0x80, b'x'];
		let token = moq_auth::Token {
			kind: moq_auth::Token::OUT_OF_BAND,
			value: value.to_vec(),
		};
		let script = Script::new(grant(Duration::from_secs(3600)));
		// Anonymous admission fails, so a SERVER_SETUP proves the token's grant was used.
		script.on_connect(Answer::Token(grant(Duration::from_secs(3600)), token.clone()));
		let auth = build_auth(script.spawn().await);
		let (port, relay) = if websocket {
			spawn_ws_relay(auth).await
		} else {
			spawn_relay(auth).await
		};

		// Draft-18 SETUP (0x2F00): AUTHORIZATION TOKEN (3), USE_VALUE (3), Token Type 0.
		let encoded = [&[0x03, 0x00][..], &value].concat();
		let params = [&[0x03, encoded.len() as u8][..], &encoded].concat();
		let setup = [&[0xaf, 0x00][..], &(params.len() as u16).to_be_bytes(), &params].concat();
		let session = if websocket {
			qmux::ws::Client::new()
				.with_protocol("moqt-18", &[qmux::Version::QMux01])
				.connect(&format!("ws://127.0.0.1:{port}/room?source=setup"))
				.await
				.expect("connect WebSocket")
		} else {
			qmux::tcp::connect(("127.0.0.1", port), {
				let mut config = qmux::Config::new(qmux::Version::QMux01);
				config.protocol = qmux::Protocol::Negotiate(vec!["moqt-18".into()]);
				config
			})
			.await
			.expect("connect TCP")
		};
		let mut send = session
			.open_uni(Default::default())
			.await
			.expect("open the setup stream");
		write_all(&mut send, &setup).await;
		let mut recv = tokio::time::timeout(TIMEOUT, session.accept_uni())
			.await
			.expect("SETUP timeout")
			.expect("the server's setup stream");
		let reply = tokio::time::timeout(TIMEOUT, recv.read_chunk(1))
			.await
			.expect("SETUP timeout")
			.expect("SETUP");
		assert_eq!(reply.map(|chunk| chunk.len()), Some(1));
		let seen = script.seen.lock().unwrap().clone();
		let connect = seen.iter().find(|r| r.event == Event::Connect).expect("a connect");
		assert_eq!(connect.token, Some(token));
		if websocket {
			assert_eq!(connect.transport, moq_auth::Transport::WebSocket);
			assert_eq!(connect.path, "/room");
			assert_eq!(connect.query.as_deref(), Some("source=setup"));
			// Plain `ws://` has no SNI; the `Host` header never stands in for one.
			assert_eq!(connect.server_name, None);
			assert!(connect.remote.is_some_and(|addr| addr.ip().is_loopback()));
			assert_eq!(connect.alpn.as_deref(), Some("qmux-01.moqt-18"));
		}
		relay.abort();
	}
}

/// A configured token is the connection credential: on lite-07 it reaches the auth server
/// as the SETUP token with nothing in the query, and on lite-06, which has no SETUP token,
/// in the query as a `?jwt=`.
#[tokio::test]
async fn a_configured_token_is_the_connection_credential() {
	let token = moq_auth::Token {
		kind: moq_auth::Token::OUT_OF_BAND,
		value: b"credential".to_vec(),
	};
	for (versions, setup, query) in [
		(vec!["moq-lite-07-wip"], Some(token.clone()), None),
		(vec!["moq-lite-06"], None, Some("jwt=credential")),
	] {
		let script = Script::new(grant(Duration::from_secs(3600)));
		// Over SETUP, anonymous admission fails, so an established session proves the token
		// was used. The scripted server cannot check a query token, so that case only asserts
		// what it was sent.
		if let Some(token) = &setup {
			script.on_connect(Answer::Token(grant(Duration::from_secs(3600)), token.clone()));
		}
		let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;

		let mut config = moq_tokio::connect::Config::default();
		config.version = versions.iter().map(|version| version.parse().unwrap()).collect();
		config.tokens = vec!["credential".parse().unwrap()];
		let client = config.init(Default::default()).unwrap();
		let url: url::Url = format!("tcp://127.0.0.1:{port}/room").parse().unwrap();
		let session = tokio::time::timeout(
			TIMEOUT,
			client
				.with_reconnect(false)
				.with_subscriber(moq_tokio::origin::spawn())
				.connect(url)
				.established(),
		)
		.await
		.expect("connect timeout")
		.expect("the configured token was refused");
		if setup.is_some() {
			// Lite-07 answers the SETUP token over AUTH, so its grant proves admission.
			let mut grant = session.auth().grant();
			tokio::time::timeout(TIMEOUT, async {
				while grant.peek().is_none() {
					grant.changed().await.expect("the relay refused the configured token");
				}
			})
			.await
			.expect("grant timeout");
		} else {
			// Lite-06 has no AUTH answer and a client is live before admission, so give a
			// refusal time to land.
			assert!(
				tokio::time::timeout(Duration::from_millis(500), session.closed())
					.await
					.is_err(),
				"{versions:?}: the relay refused the configured token"
			);
		}

		let seen = script.seen.lock().unwrap().clone();
		let connect = seen.iter().find(|r| r.event == Event::Connect).expect("a connect");
		assert_eq!(connect.token, setup, "{versions:?}");
		assert_eq!(connect.query.as_deref(), query, "{versions:?}");
		session.abort(moq_net::Error::Cancel);
		relay.abort();
	}
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

/// A re-check that moves the tier and stats label keeps the session and retags its
/// stats live: both sessions' presence leaves the old tier and label for the new
/// ones, and media sent afterwards records there in both directions.
#[tokio::test]
async fn a_moved_tier_retags_the_live_session() {
	let mut labeled = grant(Duration::from_secs(3600));
	labeled.label = Some("project/key/1".into());
	let script = Script::new(labeled);
	let mut cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	cluster.stats = stats::Registry::new(stats::Config::new());
	let registry = cluster.stats.clone();
	let (port, relay) = spawn_relay_with(build_auth(script.spawn().await), cluster).await;
	let url = room_url("tcp", port);

	let moved = stats::Tier::new(moq_net::Path::from_static("moved"));
	let active = |tier: &stats::Tier| {
		registry
			.snapshot()
			.sessions()
			.into_iter()
			.find(|(t, _)| t == tier)
			.map_or(0, |(_, presence)| presence.active())
	};
	let bytes = |tier: &stats::Tier, role: stats::Role| {
		registry
			.snapshot()
			.traffic()
			.into_iter()
			.find(|(t, r, _)| t == tier && *r == role)
			.map_or(0, |(_, _, traffic)| traffic.bytes)
	};
	// Each label's (active sessions, sent bytes, received bytes), from one report.
	let labels = || {
		let mut report = stats::Report::default();
		registry.report(&mut report);
		report
			.sessions
			.into_iter()
			.map(|entry| {
				let presence = entry.presence;
				(
					(entry.tier, entry.label.to_string()),
					(presence.active(), presence.sent_bytes, presence.received_bytes),
				)
			})
			.collect::<HashMap<_, _>>()
	};

	// A publisher whose track stays open across the move, and a subscriber to it.
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");
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

	let sub_origin = moq_tokio::origin::spawn();
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
	let (update, announced) = tokio::time::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(announced, "expected announce, got retraction");
	let bc = sub_consumer
		.request("test", None)
		.await
		.expect("announced broadcast resolves");
	let mut track_sub = bc.track("video").unwrap().subscribe(None).await.expect("subscribe");

	let mut round_trip = async |payload: &'static [u8]| {
		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, payload)
			.expect("write frame");
		group.finish().expect("finish group");
		let mut group = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
			.await
			.expect("recv_group timeout")
			.expect("recv_group failed")
			.expect("track closed prematurely");
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read_frame timeout")
			.expect("read_frame failed")
			.expect("group closed prematurely");
		assert_eq!(&frame.payload[..], payload);
	};

	round_trip(b"before").await;
	assert_eq!(active(&stats::Tier::default()), 2);
	assert_eq!(active(&moved), 0);
	assert_eq!(
		labels()[&(stats::Tier::default(), "project/key/1".to_string())],
		(2, 6, 6),
		"sessions and bytes count under the grant's label"
	);

	let mut moved_grant = grant(Duration::from_secs(3600));
	moved_grant.tier = Some("moved".into());
	moved_grant.label = Some("project/key/2".into());
	script.on_revalidate(Answer::Grant(moved_grant));

	let deadline = tokio::time::Instant::now() + TIMEOUT;
	while active(&moved) < 2 {
		assert!(
			tokio::time::Instant::now() < deadline,
			"the re-checked tier never applied"
		);
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	assert_eq!(active(&stats::Tier::default()), 0, "presence left the old tier");

	let before = (
		bytes(&stats::Tier::default(), stats::Role::Publisher),
		bytes(&stats::Tier::default(), stats::Role::Subscriber),
	);
	round_trip(b"after").await;
	assert_eq!(
		(
			bytes(&stats::Tier::default(), stats::Role::Publisher),
			bytes(&stats::Tier::default(), stats::Role::Subscriber),
		),
		before,
		"nothing more records under the old tier"
	);
	assert_eq!(bytes(&moved, stats::Role::Subscriber), 5, "ingress from the publisher");
	assert_eq!(bytes(&moved, stats::Role::Publisher), 5, "egress to the subscriber");
	let labels = labels();
	assert_eq!(
		labels[&(stats::Tier::default(), "project/key/1".to_string())],
		(0, 6, 6),
		"earlier bytes stay under the old label"
	);
	assert_eq!(
		labels[&(moved.clone(), "project/key/2".to_string())],
		(2, 5, 5),
		"later presence and bytes move to the new label"
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

/// A re-check resizes the live session in place, over TCP and WebSocket: a narrower
/// grant resets the deafened path with `Unauthorized` while a sibling under the same
/// prefix keeps flowing, a wider one brings the path back, and neither session closes.
#[tokio::test]
async fn a_rechecked_grant_resizes_live_sessions() {
	// Each scheme waits out two re-check cadences, so run them side by side.
	futures::future::join_all(["tcp", "ws"].map(|scheme| async move {
		let script = Script::new(grant(Duration::from_secs(3600)));
		let auth = build_auth(script.spawn().await);
		let (port, relay) = match scheme {
			"tcp" => spawn_relay(auth).await,
			_ => spawn_ws_relay(auth).await,
		};
		let url = room_url(scheme, port);

		let pub_origin = moq_tokio::origin::spawn();
		let mut tracks = Vec::new();
		for path in ["alice/audio", "alice/video"] {
			let broadcast = pub_origin.create_broadcast(path).expect("create broadcast");
			let track = broadcast.create_track("media", timed()).expect("create track");
			broadcast.announce(Default::default()).expect("announce broadcast");
			tracks.push((broadcast, track));
		}
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

		let sub_origin = moq_tokio::origin::spawn();
		let sub_consumer = sub_origin.consume();
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

		let mut subs = Vec::new();
		for path in ["alice/audio", "alice/video"] {
			let broadcast = tokio::time::timeout(TIMEOUT, sub_consumer.follow(path, None).unwrap().available())
				.await
				.expect("announcement timeout")
				.expect("announced broadcast resolves");
			subs.push(
				broadcast
					.track("media")
					.unwrap()
					.subscribe(None)
					.await
					.expect("subscribe"),
			);
		}
		let send = |index: usize, sequence: u64| {
			let mut group = tracks[index].1.append_group().expect("append group");
			group
				.write_frame(moq_net::Timestamp::ZERO, b"media".as_ref())
				.expect("write frame");
			group.finish().expect("finish group");
			assert_eq!(group.sequence, sequence);
		};
		for (index, sub) in subs.iter_mut().enumerate() {
			send(index, 0);
			tokio::time::timeout(TIMEOUT, sub.recv_group())
				.await
				.expect("recv_group timeout")
				.expect("recv_group failed")
				.expect("track closed prematurely");
		}

		// Deafen alice's audio: everything else stays as admitted.
		let mut narrow = grant(Duration::from_secs(3600));
		narrow.subscribe = ["alice/video/**".parse().unwrap()].into_iter().collect();
		script.on_revalidate(Answer::Grant(narrow));

		let err = loop {
			match tokio::time::timeout(TIMEOUT, subs[0].recv_group())
				.await
				.expect("the deafened track never ended")
			{
				Ok(Some(_)) => continue,
				Ok(None) => panic!("{scheme}: the deafened track finished instead of ending"),
				Err(err) => break err,
			}
		};
		assert!(
			matches!(
				err,
				moq_net::Error::Unauthorized | moq_net::Error::Stream(moq_net::StreamError::Unauthorized)
			),
			"{scheme}: {err:?}"
		);

		send(1, 1);
		let group = tokio::time::timeout(TIMEOUT, subs[1].recv_group())
			.await
			.expect("recv_group timeout")
			.expect("recv_group failed")
			.expect("the sibling track closed");
		assert_eq!(group.sequence, 1, "{scheme}");

		// Undeafen: the path is announced again and a new subscription flows.
		script.on_revalidate(Answer::Grant(grant(Duration::from_secs(3600))));
		let broadcast = tokio::time::timeout(TIMEOUT, sub_consumer.follow("alice/audio", None).unwrap().available())
			.await
			.expect("the undeafened path was never announced again")
			.expect("announced broadcast resolves");
		let mut audio = broadcast
			.track("media")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		send(0, 1);
		let group = tokio::time::timeout(TIMEOUT, audio.recv_group())
			.await
			.expect("recv_group timeout")
			.expect("recv_group failed")
			.expect("the undeafened track closed");
		assert!(group.sequence >= 1, "{scheme}");

		for (session, what) in [(pub_session, "publisher"), (sub_session, "subscriber")] {
			assert!(
				tokio::time::timeout(Duration::from_millis(200), session.closed())
					.await
					.is_err(),
				"{scheme}: a resize must not close the {what}"
			);
		}
		relay.abort();
	}))
	.await;
}

/// A re-check that moves the root still closes the session: nothing it holds is
/// named the same way under the new one.
#[tokio::test]
async fn a_moved_root_closes_live_sessions() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	let mut moved = grant(Duration::from_secs(3600));
	moved.root = Some("elsewhere".into());
	script.on_revalidate(Answer::Grant(moved));

	assert_closed(pub_session, Duration::from_secs(5), "publisher").await;
	assert_closed(sub_session, Duration::from_secs(5), "subscriber").await;
	relay.abort();
}

/// A refusal on re-check closes the session, and the `end` says why.
/// 403, 401, and an empty grant are all a no.
#[tokio::test]
async fn a_refusal_closes_live_sessions() {
	// Each answer waits out a re-check cadence, so run them side by side.
	let answers = [
		("403", Answer::Status(403)),
		("401", Answer::Status(401)),
		("empty grant", Answer::Grant(Grant::default())),
	];
	futures::future::join_all(answers.map(|(label, answer)| async move {
		let script = Script::new(grant(Duration::from_secs(3600)));
		let (port, relay) = spawn_relay(build_auth(script.spawn().await)).await;
		let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

		script.on_revalidate(answer);

		assert_closed(pub_session, Duration::from_secs(5), &format!("{label} publisher")).await;
		assert_closed(sub_session, Duration::from_secs(5), &format!("{label} subscriber")).await;

		tokio::time::sleep(Duration::from_millis(200)).await;
		let ends = script.ends();
		assert!(ends.len() >= 2, "{label}: an end per session, got {}", ends.len());
		for end in &ends {
			let Event::End { reason, .. } = &end.event else {
				unreachable!()
			};
			assert_eq!(*reason, moq_auth::lease::Reason::Refused, "{label}");
		}

		relay.abort();
	}))
	.await;
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
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast.create_track("video", timed()).expect("create track");
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
	let sub_origin = moq_tokio::origin::spawn();
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
	let (update, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announcements))
		.await
		.expect("announcement timeout")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "test");
	assert!(active, "expected announce, got retraction");

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

/// A session the drain deadline force-closes reports its `end` to the auth server,
/// and the drain waits for that report rather than exiting under it.
#[tokio::test]
async fn the_drain_waits_for_a_closed_session_to_report_its_end() {
	let clock = manual();
	let script = Script::new(grant(Duration::from_secs(3600)));
	let gate = script.hold_ends();
	let auth = build_auth(script.spawn().await);
	let drain_timeout = Duration::from_millis(100);
	let (trigger, observer) = moq_relay::shutdown::Observer::new(drain_timeout);

	let lease = auth
		.admit(auth.request(moq_auth::Transport::Quic, "/room"))
		.await
		.expect("admitted before draining");
	let (client, server) = memory_sessions().await;
	let supervised = tokio::spawn(moq_relay::supervise(server, lease, observer.serve(), None));

	// Keep the deadline from racing the client receiving GOAWAY under load.
	tokio::time::pause();
	trigger.start();
	tokio::time::timeout(TIMEOUT, moq_net::kio::wait(|waiter| client.draining().poll(waiter)))
		.await
		.expect("GOAWAY timeout")
		.expect("a GOAWAY");
	let drained = tokio::spawn({
		let observer = observer.clone();
		async move { observer.drained().await }
	});
	clock.advance(drain_timeout);
	tokio::time::advance(drain_timeout).await;
	// The end report crosses HTTP, so resume real time after forcing the close.
	tokio::time::resume();
	tokio::time::timeout(TIMEOUT, script.end_arrived.notified())
		.await
		.expect("the end never reached the server");
	tokio::task::yield_now().await;
	assert!(
		!drained.is_finished(),
		"the drain ended before the server answered the end"
	);

	gate.add_permits(1);
	tokio::time::timeout(TIMEOUT, drained)
		.await
		.expect("the drain outlived the end")
		.unwrap();
	supervised.await.expect("supervisor task").expect("drained session");
	let ends = script.ends();
	let [end] = ends.as_slice() else {
		panic!("one end per session: {ends:?}")
	};
	assert!(
		matches!(
			&end.event,
			Event::End {
				reason: moq_auth::lease::Reason::Shutdown,
				..
			}
		),
		"{end:?}"
	);
}

/// A certificate is a fact: with no server grant for it, a verified peer is
/// refused; with a narrow one, it is scoped to that and nothing more. Over QUIC
/// with a client certificate, and over WebSocket where none can be presented.
#[tokio::test]
async fn a_certificate_admits_only_what_the_server_grants() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let (root, client_cert, client_key) = signed_client(dir.path());

	let policy = |rules: moq_auth::Permissions| {
		let mut policy = moq_auth::serve::Policy::default();
		policy.mtls = rules;
		policy
	};
	let serve = |policy: moq_auth::serve::Policy| async move {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url: url::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
		let server = moq_auth::serve::Server::new(policy).unwrap();
		tokio::spawn(async move { server.serve(listener).await });
		url
	};

	let mtls_client = || {
		let mut config = moq_tokio::connect::Config::default();
		config.tls.insecure = Some(true);
		config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		config.tls.cert = Some(client_cert.clone());
		config.tls.key = Some(client_key.clone());
		config
			.init(Default::default())
			.expect("client init")
			.with_reconnect(false)
	};
	// No grant for certificates: refused, over QUIC with a certificate and over
	// WebSocket without one.
	let none = serve(policy(moq_auth::Permissions::default())).await;
	let (addr, relay) = spawn_quic_relay(build_auth(none.clone()), Some(root.clone())).await;
	let url: url::Url = format!("moql://127.0.0.1:{}/room", addr.port()).parse().unwrap();
	assert_refused_with(mtls_client(), &url).await;
	relay.abort();
	let (port, relay) = spawn_ws_relay(build_auth(none)).await;
	assert_refused(&format!("ws://127.0.0.1:{port}/room").parse().unwrap()).await;
	relay.abort();

	// A narrow grant: the certificate publishes under `mine/**` and nothing else.
	let narrow = serve(policy(moq_auth::Permissions::new(
		"mine/**".parse::<moq_auth::Pattern>().unwrap(),
		Patterns::new(),
	)))
	.await;
	let (addr, relay) = spawn_quic_relay(build_auth(narrow), Some(root.clone())).await;
	let url: url::Url = format!("moql://127.0.0.1:{}/mine", addr.port()).parse().unwrap();
	let origin = moq_tokio::origin::spawn();
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
	// The rules are rooted at `/`, so a certificate dialed outside `mine` gets nothing.
	let room: url::Url = format!("moql://127.0.0.1:{}/room", addr.port()).parse().unwrap();
	assert_refused_with(mtls_client(), &room).await;
	drop(session);
	relay.abort();
}

/// A relay dialing in with a certificate, admitted by `moq auth serve --mtls-link peer`,
/// is a cluster peer: what it announces entered the cluster elsewhere.
#[tokio::test]
async fn an_mtls_peer_is_a_cluster_peer() {
	let source = mtls_route_source(true).await;
	assert!(
		matches!(source, moq_net::origin::Source::Peer(_)),
		"an mTLS peer's route counted as {source:?}"
	);
}

/// Without `--mtls-link peer`, a certificate identifies a client ingesting here.
#[tokio::test]
async fn an_mtls_client_ingests_here() {
	assert_eq!(mtls_route_source(false).await, moq_net::origin::Source::Local);
}

/// The source the relay records for a broadcast announced over a certificate
/// session, admitted by `moq auth serve` with the peer link role when requested.
async fn mtls_route_source(mtls_peer: bool) -> moq_net::origin::Source {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let (root, client_cert, client_key) = signed_client(dir.path());

	let mut policy = moq_auth::serve::Policy::default();
	policy.mtls = moq_auth::Permissions::new(all(), all());
	policy.mtls_link = mtls_peer.then_some(moq_auth::serve::Link::Peer);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let auth_url: url::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
	let server = moq_auth::serve::Server::new(policy).unwrap();
	tokio::spawn(async move { server.serve(listener).await });

	let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
	let (addr, relay) = spawn_quic_relay_with(build_auth(auth_url), Some(root), cluster.clone()).await;

	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.tls.cert = Some(client_cert);
	config.tls.key = Some(client_key);
	let peer = moq_tokio::origin::spawn();
	let _forwarded = peer.create_broadcast("forwarded").expect("create");
	_forwarded.announce(Default::default()).expect("announce");
	let url: url::Url = format!("moql://127.0.0.1:{}/", addr.port()).parse().unwrap();
	let _session = tokio::time::timeout(
		TIMEOUT,
		config
			.init(Default::default())
			.expect("client init")
			.with_publisher(peer.consume())
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("connect timeout")
	.expect("the peer is admitted");

	let mut announced = cluster.origin.consume().announced();
	let (update, _) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
		.await
		.expect("timed out waiting for forwarded")
		.expect("origin closed");
	assert_eq!(update.prefix.as_str(), "forwarded");
	relay.abort();
	update.route.source()
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

/// An in-process decider: grants every admission `grant`, keeping the producers
/// so a test can revoke a session and the requests so it can check what arrived.
struct Decider {
	producers: Arc<Mutex<Vec<moq_auth::lease::Producer>>>,
	seen: Arc<Mutex<Vec<Request>>>,
}

impl Decider {
	fn spawn(mut admissions: moq_relay::auth::Admissions, grant: Grant) -> Self {
		let producers = Arc::new(Mutex::new(Vec::new()));
		let seen = Arc::new(Mutex::new(Vec::new()));
		let decider = Self {
			producers: producers.clone(),
			seen: seen.clone(),
		};
		tokio::spawn(async move {
			while let Some(admission) = admissions.next().await {
				seen.lock().unwrap().push(admission.request.clone());
				let (producer, consumer) = moq_auth::lease::Producer::new(grant.clone());
				producers.lock().unwrap().push(producer);
				admission.grant(consumer);
			}
		});
		decider
	}
}

/// The embedder's grant admits a session, its request carries what the relay
/// knows, and revoking the producer closes the session; once the decider is
/// gone, nobody is admitted.
#[tokio::test]
async fn an_embedded_decider_admits_and_revokes() {
	let (auth, admissions) = moq_relay::auth::Auth::embedded("test-relay");
	let decider = Decider::spawn(admissions, grant(Duration::from_secs(3600)));
	let (port, relay) = spawn_relay(auth.clone()).await;
	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;

	let seen = decider.seen.lock().unwrap().clone();
	assert_eq!(seen.len(), 2, "one admission per session");
	assert_eq!(seen[0].node, "test-relay");
	assert_eq!(seen[0].transport, moq_auth::Transport::Tcp);
	assert_eq!(seen[0].path, "/room");
	assert_eq!(seen[0].query.as_deref(), Some("jwt=token"));

	// The publisher was admitted first; revoking its lease closes it alone.
	let publisher = decider.producers.lock().unwrap().remove(0);
	publisher.revoke(moq_auth::lease::Reason::Refused);
	assert_closed(pub_session, Duration::from_secs(5), "publisher").await;
	assert!(
		tokio::time::timeout(Duration::from_millis(200), sub_session.closed())
			.await
			.is_err(),
		"revoking one lease must not close the other session"
	);

	// A refusal reaches the session's transport before it establishes.
	let (refusing, mut refusals) = moq_relay::auth::Auth::embedded("test-relay");
	tokio::spawn(async move {
		while let Some(admission) = refusals.next().await {
			admission.refuse(moq_relay::auth::Error::Refused(Default::default()));
		}
	});
	let (refused_port, refusing_relay) = spawn_relay(refusing).await;
	assert_refused(&room_url("tcp", refused_port)).await;

	// Nobody answering is an outage: no session gets through.
	let (orphaned, admissions) = moq_relay::auth::Auth::embedded("test-relay");
	drop(admissions);
	let (orphan_port, orphan_relay) = spawn_relay(orphaned).await;
	assert_refused(&room_url("tcp", orphan_port)).await;

	relay.abort();
	refusing_relay.abort();
	orphan_relay.abort();
}

/// A fixed lease has no driver, so the relay itself closes the session at the
/// grant's `expires`. The TCP and WebSocket paths hand their lease to the same
/// `supervise`, as `a_rechecked_grant_resizes_live_sessions` proves.
#[tokio::test(start_paused = true)]
async fn a_fixed_lease_still_expires() {
	let clock = manual();
	closes_at_expires(&clock, moq_auth::lease::Consumer::fixed(expiring())).await;
}

/// A connected `(client, server)` session pair over the in-memory transport, so a
/// test on Tokio's paused clock never waits on a socket: the clock only advances
/// once every message in flight has landed.
async fn memory_sessions() -> (moq_net::Session, moq_net::Session) {
	let mut config = moq_net::transport::memory::Config::default();
	config.protocol = Some(crate::memory::newest().alpn().to_string());
	let (client, server) = moq_net::transport::memory::pair(config);

	let now = moq_net::time::Instant::now();
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.connect(now, client)
			.await
			.expect("client handshake");
		tokio::spawn(moq_net::time::run(driver));
		session
	};
	let server = async {
		let (session, driver) = moq_net::Server::new()
			.accept(now, server)
			.await
			.expect("server handshake");
		tokio::spawn(moq_net::time::run(driver));
		session
	};
	tokio::join!(client, server)
}

/// An outage keeps the session until `expires`, then closes it as expired and
/// reports that as its end. A decider that never answers again is an outage as
/// far as the relay can tell, so the relay enforces `expires` itself; how an auth
/// server's outage keeps the grant is `moq_auth::Client`'s to test.
#[tokio::test(start_paused = true)]
async fn an_outage_keeps_the_session_until_expires() {
	let clock = manual();
	let (producer, consumer) = moq_auth::lease::Producer::new(expiring());
	closes_at_expires(&clock, consumer).await;
	let (reason, _) = producer.closed().await;
	assert_eq!(reason, moq_auth::lease::Reason::Expired);
}

/// How long after the manual clock's start [`expiring`] grants run out.
const EXPIRES: Duration = Duration::from_secs(3);

/// Make the test runtime's clock one that only the test moves, wall time included,
/// so a grant's life is measured exactly. Call it before anything reads the clock.
fn manual() -> moq_clock::Installed {
	moq_clock::Manual::new().install(&tokio::runtime::Handle::current())
}

/// Everything under the dialed path, until [`EXPIRES`] from now on the ambient clock.
fn expiring() -> Grant {
	let mut grant = Grant::new(all(), all());
	grant.expires = Some(moq_clock::wall() + EXPIRES);
	grant
}

/// Supervise an in-memory session admitted on `consumer`, whose grant nobody renews,
/// and assert it closes at exactly [`EXPIRES`] on `clock`.
async fn closes_at_expires(clock: &moq_clock::Installed, consumer: moq_auth::lease::Consumer) {
	let (auth, mut admissions) = moq_relay::auth::Auth::embedded("test-relay");
	let (lease, ()) = tokio::join!(auth.admit(auth.request(moq_auth::Transport::Tcp, "/room")), async {
		admissions.next().await.expect("an admission").grant(consumer);
	});
	let (client, server) = memory_sessions().await;
	let supervised = tokio::spawn(moq_relay::supervise(
		server,
		lease.expect("admitted"),
		moq_relay::shutdown::Observer::disabled().serve(),
		None,
	));

	clock.advance(EXPIRES - Duration::from_nanos(1));
	// Paused tokio fires this watchdog only once nothing else can run, and never
	// moves the manual clock.
	assert!(
		tokio::time::timeout(Duration::from_secs(1), client.closed())
			.await
			.is_err(),
		"closed before expires"
	);
	clock.advance(Duration::from_nanos(1));
	tokio::time::timeout(TIMEOUT, client.closed())
		.await
		.expect("closed at expires");
	supervised
		.await
		.expect("supervisor")
		.expect("a lease end is not a session error");
}

/// A relay whose config names no auth source is the embedder's to decide: `run`
/// refuses to start until the admissions are taken, and once they are, the
/// decider's grant admits sessions through the assembled relay.
#[tokio::test]
async fn a_relay_without_an_auth_source_is_decided_by_the_embedder() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let config = || {
		let mut config = Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
		// The sessions are gone by the time the trigger fires; no need to wait out the default window.
		config.drain_timeout = Some((Duration::from_millis(100)).into());
		config
	};

	let untaken = Relay::load(config()).await.expect("load relay").with_signals(false);
	let err = untaken.run().await.expect_err("nobody can authenticate");
	assert!(err.to_string().contains("nobody can authenticate"), "{err}");

	let mut relay = Relay::load(config()).await.expect("load relay").with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let admissions = relay.admissions().expect("an empty [auth] hands over the admissions");
	assert!(relay.admissions().is_none(), "taken once");
	let decider = Decider::spawn(admissions, grant(Duration::from_secs(3600)));
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());

	let (pub_session, sub_session) = connect_and_round_trip(&room_url("tcp", port)).await;
	assert_eq!(decider.seen.lock().unwrap().len(), 2, "one admission per session");
	drop(pub_session);
	drop(sub_session);

	trigger.start();
	tokio::time::timeout(TIMEOUT, running)
		.await
		.expect("run returned after the trigger")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

/// A relay deciding admissions in process, listening on TCP with `timeout` as its
/// handshake deadline, and a client connecting to it.
async fn connect_through_embedded_admission(
	timeout: Duration,
) -> (
	Relay,
	auth::Admissions,
	tokio::task::JoinHandle<moq_tokio::Result<moq_tokio::Connection>>,
) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut config = Config::default();
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	config.listen.timeout = Some((timeout).into());
	config.drain_timeout = Some((Duration::from_millis(100)).into());
	let mut relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener bound").port();
	let admissions = relay.admissions().expect("an empty [auth] hands over the admissions");
	let url = room_url("tcp", port);
	let connecting = tokio::spawn(async move { client().with_reconnect(false).connect(url).established().await });
	(relay, admissions, connecting)
}

/// `listen.timeout` bounds admission as part of the handshake: a decision that never
/// comes closes the connection at the deadline, not after a fixed cap of its own.
#[tokio::test]
async fn an_unanswered_admission_closes_at_the_handshake_deadline() {
	let (relay, mut admissions, connecting) = connect_through_embedded_admission(Duration::from_secs(1)).await;
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	let admission = admissions.next().await.expect("the admission is asked");

	// Only timers decide what follows: the deadline passes with nobody answering.
	tokio::time::pause();
	tokio::time::advance(Duration::from_secs(2)).await;
	tokio::time::resume();
	// The close reaches the client over loopback within moments; the old fixed cap
	// would have held the connection for another eight seconds.
	let connecting = tokio::time::timeout(TIMEOUT, connecting)
		.await
		.expect("the client never settled")
		.expect("client task");
	if let Ok(connection) = connecting {
		tokio::time::timeout(Duration::from_secs(3), connection.closed())
			.await
			.expect("the connection outlived the handshake deadline")
			.expect_err("closed as refused, not cleanly");
	}
	// A decision now has nobody to tell.
	drop(admission);

	trigger.start();
	tokio::time::timeout(TIMEOUT, running)
		.await
		.expect("run returned after the trigger")
		.expect("relay task panicked")
		.expect("relay exited with an error");
}

/// Admission has no bound but the handshake deadline: a decision past the old
/// ten-second cap still admits the session, under a deadline or with `"0"`, which
/// sets none. The wait stays under the transport's keep-alive, which a longer jump
/// of the paused clock would trip on its own.
#[tokio::test]
async fn a_late_decision_within_the_deadline_admits() {
	let wait = Duration::from_secs(11);
	for timeout in [Duration::from_secs(30), Duration::ZERO] {
		let (relay, mut admissions, connecting) = connect_through_embedded_admission(timeout).await;
		let trigger = relay.shutdown_trigger().clone();
		let running = tokio::spawn(relay.run());
		let admission = admissions.next().await.expect("the admission is asked");

		tokio::time::pause();
		tokio::time::advance(wait).await;
		tokio::time::resume();
		admission.grant(moq_auth::lease::Consumer::fixed(grant(Duration::from_secs(3600))));
		let connection = tokio::time::timeout(TIMEOUT, connecting)
			.await
			.expect("the late decision never reached the client")
			.expect("client task")
			.unwrap_or_else(|err| panic!("{timeout:?}: refused after {wait:?}: {err}"));
		// Admitted, so the session stays up; a refusal would have closed it by now.
		assert!(
			tokio::time::timeout(Duration::from_secs(1), connection.closed())
				.await
				.is_err(),
			"{timeout:?}: closed after the decision {wait:?} in"
		);
		drop(connection);

		trigger.start();
		tokio::time::timeout(TIMEOUT, running)
			.await
			.expect("run returned after the trigger")
			.expect("relay task panicked")
			.expect("relay exited with an error");
	}
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

/// The auth server sees the accepted transport, independent of negotiated MoQ ALPN.
/// An IP-literal dial sends no SNI, so neither reports a `server_name`: the
/// WebTransport CONNECT host selected no certificate and never stands in for one.
#[tokio::test]
async fn quic_and_webtransport_report_distinct_auth_transports() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	let (addr, relay) = spawn_quic_relay(build_auth(script.spawn().await), None).await;
	for (scheme, expected) in [("moql", "quic"), ("https", "webtransport")] {
		let path = format!("/transport/{scheme}");
		let url: url::Url = format!("{scheme}://127.0.0.1:{}{path}", addr.port()).parse().unwrap();
		let connections = connect_and_round_trip(&url).await;
		let seen = script.seen.lock().unwrap();
		let request = seen
			.iter()
			.find(|r| r.event == Event::Connect && r.path == path)
			.unwrap_or_else(|| panic!("auth connect for {path}: {seen:?}"));
		assert_eq!(request.transport.as_str(), expected, "{scheme}");
		assert!(request.alpn.as_ref().is_some_and(|alpn| alpn.starts_with("moq-lite-")));
		assert_eq!(request.server_name, None, "{scheme}");
		drop(connections);
	}
	relay.abort();
	let _ = relay.await;
}

/// A token can reach a prefix outside the initial credential, and withdrawing it
/// revokes only the streams that depended on it on both native and upgraded WS.
#[tokio::test]
async fn in_band_tokens_widen_and_withdraw_live_prefixes() {
	for scheme in ["tcp", "ws"] {
		let mut initial = Grant::new(all(), "a/**".parse::<moq_auth::Pattern>().unwrap());
		initial.expires = Some(moq_clock::wall() + Duration::from_secs(3600));
		let script = Script::new(initial);
		let auth = build_auth(script.spawn().await);
		let (port, relay) = if scheme == "tcp" {
			spawn_relay(auth).await
		} else {
			spawn_ws_relay(auth).await
		};
		let client = || {
			let mut config = moq_tokio::connect::Config::default();
			config.version = vec!["moq-transport-22".parse().unwrap()];
			#[cfg(feature = "websocket")]
			{
				config.websocket.delay = Some((Duration::ZERO).into());
			}
			config.init(Default::default()).unwrap().with_reconnect(false)
		};
		let origin = moq_tokio::origin::spawn();
		let a = origin.create_broadcast("a/cam").unwrap();
		let a_track = a.create_track("video", timed()).unwrap();
		a.announce(Default::default()).unwrap();
		let b = origin.create_broadcast("b/cam").unwrap();
		let b_track = b.create_track("video", timed()).unwrap();
		b.announce(Default::default()).unwrap();
		let publisher = tokio::time::timeout(
			TIMEOUT,
			client()
				.with_publisher(origin.consume())
				.connect(room_url(scheme, port))
				.established(),
		)
		.await
		.unwrap()
		.unwrap();
		let received = moq_tokio::origin::spawn();
		let read = received.consume();
		let subscriber = tokio::time::timeout(
			TIMEOUT,
			client()
				.with_subscriber(received)
				.connect(room_url(scheme, port))
				.established(),
		)
		.await
		.unwrap()
		.unwrap();
		let a_read = tokio::time::timeout(TIMEOUT, read.follow("a/cam", None).unwrap().available())
			.await
			.unwrap()
			.unwrap();
		let mut a_sub = a_read.track("video").unwrap().subscribe(None).await.unwrap();
		let mut replacement = Grant::new(all(), "b/**".parse::<moq_auth::Pattern>().unwrap());
		replacement.expires = Some(moq_clock::wall() + Duration::from_secs(3600));
		script.on_connect(Answer::Grant(replacement));
		let token = tokio::time::timeout(TIMEOUT, subscriber.auth().add("replacement"))
			.await
			.unwrap()
			.unwrap();
		let b_read = tokio::time::timeout(TIMEOUT, read.follow("b/cam", None).unwrap().available())
			.await
			.expect("new prefix not announced")
			.unwrap();
		let mut b_sub = b_read.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = b_track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, b"new prefix".as_ref())
			.unwrap();
		group.finish().unwrap();
		assert!(
			tokio::time::timeout(TIMEOUT, b_sub.recv_group())
				.await
				.unwrap()
				.unwrap()
				.is_some()
		);
		drop(token);
		let ended = tokio::time::timeout(TIMEOUT, b_sub.recv_group())
			.await
			.expect("withdrawn prefix stayed authorized");
		assert!(
			matches!(
				ended,
				Err(moq_net::Error::Unauthorized | moq_net::Error::Stream(moq_net::StreamError::Unauthorized))
			),
			"{scheme}: {:?}",
			ended.as_ref().err()
		);
		let mut group = a_track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, b"original prefix".as_ref())
			.unwrap();
		group.finish().unwrap();
		assert!(
			tokio::time::timeout(TIMEOUT, a_sub.recv_group())
				.await
				.unwrap()
				.unwrap()
				.is_some()
		);
		subscriber.abort(moq_net::Error::Cancel);
		publisher.abort(moq_net::Error::Cancel);
		relay.abort();
		let _ = relay.await;
	}
}

/// Both admission paths apply the selected cap before starting the session driver.
#[tokio::test]
async fn relay_request_limits_distinguish_clients_and_peers() {
	for scheme in ["tcp", "ws"] {
		for peer in [false, true] {
			let mut grant = Grant::new(all(), all());
			grant.peer = peer;
			let script = Script::new(grant);
			let auth = build_auth(script.spawn().await);
			let mut options = cluster::Options::default();
			options.session.announces = 1;
			options.peer.announces = 2;
			let cluster = cluster::Cluster::new(options).unwrap();
			let consumer = cluster.origin.consume();
			let mut observed = consumer.announced();
			let (port, relay) = match scheme {
				"tcp" => spawn_relay_with(auth, cluster).await,
				_ => spawn_ws_relay_with(auth, cluster).await,
			};
			let origin = moq_tokio::origin::spawn();
			let session = client()
				.with_publisher(origin.consume())
				.with_reconnect(false)
				.connect(room_url(scheme, port))
				.established()
				.await
				.unwrap();
			let first = origin.create_broadcast("one").unwrap();
			first.announce(Default::default()).unwrap();
			tokio::time::timeout(TIMEOUT, next_update(&mut observed))
				.await
				.unwrap()
				.unwrap();
			let second = origin.create_broadcast("two").unwrap();
			second.announce(Default::default()).unwrap();
			if peer {
				let (route, active) = tokio::time::timeout(TIMEOUT, next_update(&mut observed))
					.await
					.unwrap()
					.unwrap();
				assert!(active, "the peer must retain its second announcement: {route:?}");
			} else {
				let error = tokio::time::timeout(TIMEOUT, session.closed())
					.await
					.expect("the second client announcement exceeds the cap");
				assert!(
					matches!(
						error,
						Err(moq_tokio::Error::MoqNet(moq_net::Error::Session(
							moq_net::SessionError::TooManyRequests
						)))
					),
					"{error:?}"
				);
			}
			drop(session);
			relay.abort();
			let _ = relay.await;
		}
	}
}

/// Browser-visible policy refusals happen after the WebSocket upgrade, in MoQ's close registry.
#[tokio::test]
async fn websocket_refusal_is_in_band() {
	use moq_auth::refusal::Reason;
	use moq_net::transport::{
		Error as _,
		poll::{SendStream as _, Session as _},
	};
	for (answer, code) in [
		(Answer::Status(401), moq_net::SessionError::Unauthorized),
		(Answer::Status(403), moq_net::SessionError::Unauthorized),
		(Answer::Status(502), moq_net::SessionError::App(502)),
		(Answer::Refuse(Reason::Forbidden), moq_net::SessionError::Unauthorized),
		(Answer::Refuse(Reason::Expired), moq_net::SessionError::Expired),
		(
			Answer::Refuse(Reason::Malformed),
			moq_net::SessionError::MalformedAuthToken,
		),
	] {
		let script = Script::new(grant(Duration::from_secs(3600)));
		script.on_connect(answer);
		let (port, relay) = spawn_ws_relay(build_auth(script.spawn().await)).await;
		let session = qmux::ws::Client::new()
			.with_protocol("moq-lite-06", &[qmux::Version::QMux01])
			.connect(&format!("ws://127.0.0.1:{port}/room"))
			.await
			.expect("policy refusal must not reject the browser-opaque HTTP upgrade");
		let mut send = session.open_uni(Default::default()).await.expect("open SETUP");
		write_all(&mut send, &[1, 1, 0]).await;
		send.finish().expect("finish SETUP");
		let closed = tokio::time::timeout(TIMEOUT, session.closed())
			.await
			.expect("refused session stayed open");
		assert_eq!(closed.session_error().map(|(code, _)| code), Some(code.to_code()));
		relay.abort();
	}
}

/// The role seen at SETUP reaches the shared admission gate on the WebSocket path.
#[tokio::test]
async fn websocket_publisher_role_is_refused_by_a_subscribe_only_grant() {
	let script = Script::new(Grant::new(Patterns::new(), all()));
	let (port, relay) = spawn_ws_relay(build_auth(script.spawn().await)).await;
	let url = room_url("ws", port);
	let origin = moq_tokio::origin::spawn();
	let result = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_publisher(origin.consume())
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("publisher handshake timed out");

	let error = match result {
		Ok(session) => tokio::time::timeout(TIMEOUT, session.closed())
			.await
			.expect("mismatched publisher stayed open")
			.unwrap_err(),
		Err(error) => error,
	};
	assert_eq!(error.connect_error(), Some(moq_tokio::ConnectError::Unauthorized));
	let seen = script.seen.lock().unwrap().clone();
	let connect = seen
		.iter()
		.find(|request| request.event == Event::Connect)
		.expect("auth saw no connection");
	assert_eq!(connect.role, Some(moq_auth::Role::Publisher));
	relay.abort();
}

/// The native reconnect loop treats an in-band unauthorized refusal as terminal too.
#[tokio::test]
async fn websocket_unauthorized_stops_native_reconnect() {
	let script = Script::new(grant(Duration::from_secs(3600)));
	script.on_connect(Answer::Status(401));
	let (port, relay) = spawn_ws_relay(build_auth(script.spawn().await)).await;
	let connection = client().with_reconnect(true).connect(room_url("ws", port));
	let error = tokio::time::timeout(TIMEOUT, connection.closed())
		.await
		.expect("unauthorized reconnect loop did not stop")
		.unwrap_err();
	assert_eq!(error.connect_error(), Some(moq_tokio::ConnectError::Unauthorized));
	assert_eq!(
		script
			.seen
			.lock()
			.unwrap()
			.iter()
			.filter(|request| request.event == Event::Connect)
			.count(),
		1
	);
	relay.abort();
}

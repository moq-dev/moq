//! Push a re-check to live sessions through the internal listener.
//!
//! Stands up the same accept loop as [`auth_lifetime`], plus the ops routes, and
//! a scripted auth server whose reply the test flips. A POST is a re-check, not
//! an authority: the server's next word is what kicks, reties, or keeps the
//! session.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_auth::{Event, Grant, Pattern, Patterns, Request};
use moq_relay::session::{Filter, List, Nudged};
use moq_relay::{Connection, auth, cluster, internal, web};
use moq_tokio::moq_net::{self, Hop};

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
enum Answer {
	Grant(Grant),
	Status(u16),
}

#[derive(Clone)]
struct Script {
	connect: Arc<Mutex<Answer>>,
	revalidate: Arc<Mutex<Answer>>,
	seen: Arc<Mutex<Vec<Request>>>,
}

impl Script {
	fn new(grant: Grant) -> Self {
		Self {
			connect: Arc::new(Mutex::new(Answer::Grant(grant.clone()))),
			revalidate: Arc::new(Mutex::new(Answer::Grant(grant))),
			seen: Arc::new(Mutex::new(Vec::new())),
		}
	}

	fn on_revalidate(&self, answer: Answer) {
		*self.revalidate.lock().unwrap() = answer;
	}

	fn events(&self) -> Vec<Event> {
		self.seen.lock().unwrap().iter().map(|r| r.event.clone()).collect()
	}

	fn revalidate_ids(&self) -> Vec<String> {
		self.seen
			.lock()
			.unwrap()
			.iter()
			.filter(|r| r.event == Event::Revalidate)
			.map(|r| r.id.clone())
			.collect()
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
			Event::Revalidate => script.revalidate.lock().unwrap().clone(),
			Event::End { .. } => return StatusCode::NO_CONTENT.into_response(),
		};
		match answer {
			Answer::Grant(grant) => Json(grant).into_response(),
			Answer::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
		}
	}

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

/// A grant that lasts an hour and would not re-check on its own during a test.
fn grant() -> Grant {
	let mut grant = Grant::new(all(), all());
	grant.expires = Some(SystemTime::now() + Duration::from_secs(3600));
	grant.revalidate = Some(Duration::from_secs(3600));
	grant
}

fn build_auth(url: url::Url) -> moq_relay::auth::Auth {
	let mut config = auth::Config::default();
	config.url = Some(url);
	config
		.init("test-relay", &moq_tokio::tls::Connect::default())
		.expect("auth init")
}

async fn wait_for_listener(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
		assert!(
			std::time::Instant::now() < deadline,
			"listener never became ready on port {port}"
		);
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

fn free_port() -> u16 {
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	probe.local_addr().expect("local addr").port()
}

struct Fixture {
	url: url::Url,
	internal: url::Url,
	sessions: moq_relay::session::Registry,
	_relay: tokio::task::JoinHandle<()>,
}

impl Fixture {
	async fn tcp(auth: moq_relay::auth::Auth) -> Self {
		Self::spawn(auth, false).await
	}

	async fn ws(auth: moq_relay::auth::Auth) -> Self {
		Self::spawn(auth, true).await
	}

	async fn spawn(auth: moq_relay::auth::Auth, websocket: bool) -> Self {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let sessions = moq_relay::session::Registry::new();
		let internal_port = free_port();
		let internal_addr: std::net::SocketAddr = format!("127.0.0.1:{internal_port}").parse().unwrap();
		let mut internal_config = internal::Config::default();
		internal_config.listen = Some(internal_addr);
		let internal = internal::Internal::new(internal_config, moq_net::stats::Registry::disabled())
			.with_sessions(sessions.clone());
		tokio::spawn(async move {
			let _ = internal.run().await;
		});
		wait_for_listener(internal_port).await;

		let (url, handle) = if websocket {
			let port = free_port();
			let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
			let mut server_config = moq_tokio::listen::Config::default();
			server_config.bind = Some("[::]:0".to_string());
			server_config.tls.generate = vec!["localhost".into()];
			let certificates = server_config
				.init(Default::default())
				.expect("server init")
				.certificates();
			let mut web_config = web::Config::default();
			web_config.ws = true;
			web_config.http.listen = Some(format!("127.0.0.1:{port}").parse().expect("parse listen"));
			let web = web::Web::new(auth, cluster, certificates, web_config).with_sessions(sessions.clone());
			let handle = tokio::spawn(async move {
				let _ = web.run().await;
			});
			wait_for_listener(port).await;
			(format!("ws://127.0.0.1:{port}").parse().unwrap(), handle)
		} else {
			let port = free_port();
			let mut config = moq_tokio::listen::Config::default();
			config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
			let server = config.init(Default::default()).expect("server init");
			let mut server = server.listen().await.expect("listen");
			let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster init");
			let sessions = sessions.clone();
			let handle = tokio::spawn(async move {
				let mut id = 0;
				while let Some(request) = server.accept().await {
					let conn = Connection::new(request, cluster.clone(), auth.clone())
						.with_id(id)
						.with_sessions(sessions.clone());
					id += 1;
					tokio::spawn(async move {
						let _ = conn.run().await;
					});
				}
			});
			wait_for_listener(port).await;
			(format!("tcp://127.0.0.1:{port}").parse().unwrap(), handle)
		};

		Self {
			url,
			internal: format!("http://127.0.0.1:{internal_port}").parse().unwrap(),
			sessions,
			_relay: handle,
		}
	}

	fn path(&self, path: &str) -> url::Url {
		let mut url = self.url.clone();
		url.set_path(path);
		url
	}

	async fn list(&self, query: &str) -> (reqwest::StatusCode, List) {
		let url = format!("{}{query}", self.sessions_url());
		let response = reqwest::Client::new().get(&url).send().await.expect("GET /sessions");
		let status = response.status();
		let list = if status.is_success() {
			response.json().await.expect("list body")
		} else {
			let _ = response.text().await;
			List { sessions: Vec::new() }
		};
		(status, list)
	}

	fn sessions_url(&self) -> String {
		format!("{}/sessions", self.internal.as_str().trim_end_matches('/'))
	}

	async fn list_err(&self, query: &str) -> (reqwest::StatusCode, String) {
		let url = format!("{}{query}", self.sessions_url());
		let response = reqwest::Client::new().get(&url).send().await.expect("GET /sessions");
		(response.status(), response.text().await.expect("error body"))
	}

	async fn revalidate(&self, query: &str) -> (reqwest::StatusCode, Nudged) {
		let url = format!("{}/revalidate{query}", self.sessions_url());
		let response = reqwest::Client::new()
			.post(&url)
			.send()
			.await
			.expect("POST /sessions/revalidate");
		let status = response.status();
		let body = response.json().await.expect("nudge body");
		(status, body)
	}
}

fn client_at(bind: &str) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.once = Some(true);
	config.websocket.delay = Duration::ZERO.into();
	config.bind = Some(bind.parse().expect("parse bind"));
	config.init(Default::default()).expect("client init")
}

async fn connect(url: url::Url, bind: &str) -> moq_tokio::Connection {
	tokio::time::timeout(
		TIMEOUT,
		client_at(bind)
			.with_subscriber(moq_tokio::origin::spawn(Hop::random()))
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("connect timeout")
	.expect("connect failed")
}

async fn wait_listed(fixture: &Fixture, n: usize) -> List {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		let (status, list) = fixture.list("").await;
		assert_eq!(status, reqwest::StatusCode::OK);
		if list.sessions.len() >= n {
			return list;
		}
		assert!(
			std::time::Instant::now() < deadline,
			"expected {n} sessions, got {}",
			list.sessions.len()
		);
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

/// A push by id closes a refused session well inside the cadence, and `end` says
/// `refused`.
#[tokio::test]
async fn push_by_id_refuses_inside_the_cadence() {
	let script = Script::new(grant());
	let fixture = Fixture::tcp(build_auth(script.spawn().await)).await;
	let session = connect(fixture.path("/room"), "127.0.0.1:0").await;
	let list = wait_listed(&fixture, 1).await;
	assert_eq!(list.sessions.len(), 1);
	assert!(
		serde_json::to_value(&list.sessions[0]).unwrap().get("query").is_none(),
		"GET /sessions must omit query"
	);
	let id = list.sessions[0].id.clone();

	script.on_revalidate(Answer::Status(403));
	let (status, nudged) = fixture.revalidate(&format!("?id={id}")).await;
	assert_eq!(status, reqwest::StatusCode::ACCEPTED);
	assert_eq!(nudged.ids.as_slice(), std::slice::from_ref(&id));

	tokio::time::timeout(Duration::from_secs(3), session.closed())
		.await
		.expect("a push must close the session well inside the cadence")
		.expect_err("refused as Unauthorized");

	tokio::time::sleep(Duration::from_millis(200)).await;
	let ends = script.ends();
	assert_eq!(ends.len(), 1);
	assert_eq!(ends[0].id, id);
	match &ends[0].event {
		Event::End { reason, .. } => assert_eq!(*reason, moq_auth::lease::Reason::Refused),
		other => panic!("expected end, got {other:?}"),
	}
}

/// A path pattern and a CIDR each match the right subset of three sessions.
#[tokio::test]
async fn path_and_cidr_select_a_subset() {
	let script = Script::new(grant());
	let fixture = Fixture::tcp(build_auth(script.spawn().await)).await;

	let a = connect(fixture.path("/demo/one"), "127.0.0.1:0").await;
	let b = connect(fixture.path("/demo/two"), "127.0.0.1:0").await;
	let c = connect(fixture.path("/other"), "127.0.0.1:0").await;
	wait_listed(&fixture, 3).await;

	let (status, path) = fixture.list("?path=demo/**").await;
	assert_eq!(status, reqwest::StatusCode::OK);
	assert_eq!(path.sessions.len(), 2);
	assert!(path.sessions.iter().all(|s| s.path.starts_with("/demo/")));

	// Live tcp sessions all come from 127.0.0.1; the CIDR selector is proven
	// with the request the server would have seen from other addresses.
	let mut net = Request::new("test-relay", moq_auth::Transport::Tcp, "/cidr");
	net.id = "net".into();
	net.remote = Some("203.0.113.9:1".parse().unwrap());
	let mut other_net = Request::new("test-relay", moq_auth::Transport::Tcp, "/cidr");
	other_net.id = "out".into();
	other_net.remote = Some("198.51.100.2:1".parse().unwrap());
	let _net = fixture.sessions.register(net);
	let _out = fixture.sessions.register(other_net);
	let (status, cidr) = fixture.list("?remote=203.0.113.0/24").await;
	assert_eq!(status, reqwest::StatusCode::OK);
	assert_eq!(cidr.sessions.len(), 1);
	assert_eq!(cidr.sessions[0].id, "net");

	script.on_revalidate(Answer::Status(403));
	let (status, nudged) = fixture.revalidate("?path=demo/**").await;
	assert_eq!(status, reqwest::StatusCode::ACCEPTED);
	assert_eq!(nudged.ids.len(), 2);

	tokio::time::timeout(Duration::from_secs(3), a.closed())
		.await
		.expect("path match a")
		.expect_err("refused");
	tokio::time::timeout(Duration::from_secs(3), b.closed())
		.await
		.expect("path match b")
		.expect_err("refused");
	assert!(
		tokio::time::timeout(Duration::from_millis(200), c.closed())
			.await
			.is_err(),
		"the unmatched session must stay"
	);
}

/// A WebSocket session is listed and closes on a push like a QUIC one.
#[tokio::test]
async fn websocket_is_listed_and_closes_on_a_push() {
	let script = Script::new(grant());
	let fixture = Fixture::ws(build_auth(script.spawn().await)).await;
	let session = connect(fixture.path("/room"), "127.0.0.1:0").await;
	let list = wait_listed(&fixture, 1).await;
	assert_eq!(list.sessions[0].transport, moq_auth::Transport::WebSocket);

	script.on_revalidate(Answer::Status(403));
	let (status, _) = fixture.revalidate(&format!("?id={}", list.sessions[0].id)).await;
	assert_eq!(status, reqwest::StatusCode::ACCEPTED);
	tokio::time::timeout(Duration::from_secs(3), session.closed())
		.await
		.expect("websocket push")
		.expect_err("refused");
}

/// An empty filter reaches every session on this node.
#[tokio::test]
async fn empty_filter_reaches_all() {
	let script = Script::new(grant());
	let fixture = Fixture::tcp(build_auth(script.spawn().await)).await;
	let _a = connect(fixture.path("/a"), "127.0.0.1:0").await;
	let _b = connect(fixture.path("/b"), "127.0.0.1:0").await;
	wait_listed(&fixture, 2).await;

	let (status, nudged) = fixture.revalidate("").await;
	assert_eq!(status, reqwest::StatusCode::ACCEPTED);
	assert_eq!(nudged.ids.len(), 2);

	tokio::time::timeout(Duration::from_secs(3), async {
		loop {
			if script.revalidate_ids().len() >= 2 {
				break;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	})
	.await
	.expect("empty filter re-POSTs once per session");
}

/// A retier arrives on the next reply after a push, and the session stays.
#[tokio::test]
async fn a_pushed_retier_arrives_on_the_next_reply() {
	let script = Script::new(grant());
	let fixture = Fixture::tcp(build_auth(script.spawn().await)).await;
	let session = connect(fixture.path("/room"), "127.0.0.1:0").await;
	wait_listed(&fixture, 1).await;

	let mut moved = grant();
	moved.tier = Some("gold".into());
	script.on_revalidate(Answer::Grant(moved));
	let (status, _) = fixture.revalidate("").await;
	assert_eq!(status, reqwest::StatusCode::ACCEPTED);

	tokio::time::timeout(Duration::from_secs(3), async {
		loop {
			if script.events().contains(&Event::Revalidate) {
				break;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	})
	.await
	.expect("the push re-checked");
	assert!(
		tokio::time::timeout(Duration::from_millis(200), session.closed())
			.await
			.is_err(),
		"a retier must not close the session"
	);
}

/// Unknown fields and a `query` filter are refused; no match is 200 with an
/// empty list.
#[tokio::test]
async fn unknown_fields_are_refused() {
	let script = Script::new(grant());
	let fixture = Fixture::tcp(build_auth(script.spawn().await)).await;

	let (status, body) = fixture.list_err("?query=jwt=secret").await;
	assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
	assert!(body.contains("query"), "{body}");

	let (status, body) = fixture.list_err("?foo=bar").await;
	assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
	assert!(body.contains("foo"), "{body}");

	let (status, nudged) = fixture.revalidate("?id=missing").await;
	assert_eq!(status, reqwest::StatusCode::OK);
	assert!(nudged.ids.is_empty());

	assert!(Filter::from_query(Some("query=x")).is_err());
}

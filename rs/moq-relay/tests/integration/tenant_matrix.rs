//! One cross-tenant matrix through a real relay, on the shared Tokio QUIC
//! listener and again on io_uring workers, so a change to either runtime's
//! admission path that lets one tenant reach another fails here.
//!
//! A scripted auth server pins each credential to its tenant whatever URL it
//! dials: `a` is rooted at `tenant-a`, `b` at `tenant-b` with a read-only
//! mount of `tenant-a/public` at `shared`.

#![cfg(feature = "quic")]

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_auth::{Event, Grant, Pattern, Patterns, Request};
use moq_relay::Relay;
use moq_tokio::moq_net;
use moq_tokio::quic::driver;

use crate::refusals::{client, config};

const TIMEOUT: Duration = Duration::from_secs(10);

/// How long a broadcast that should never arrive gets to show up anyway, once
/// everything published before it has.
const SETTLE: Duration = Duration::from_millis(300);

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Publish and subscribe everything under `root`, for an hour, without a re-check
/// of its own: a test pushes one through `/sessions/revalidate` when it wants it.
fn rooted(root: &str) -> Grant {
	let all: Patterns = [Pattern::all()].into_iter().collect();
	let mut grant = Grant::new(all.clone(), all);
	grant.root = Some(root.into());
	grant.expires = Some(moq_clock::wall() + Duration::from_secs(3600));
	grant.revalidate = Some(Duration::from_secs(3600));
	grant
}

fn tenant_a() -> Grant {
	rooted("tenant-a")
}

fn tenant_b() -> Grant {
	let mut grant = rooted("tenant-b");
	grant.mounts = BTreeMap::from([("shared".into(), "tenant-a/public".into())]);
	grant
}

/// An auth server answering by the `jwt` query parameter, recording every request.
#[derive(Clone, Default)]
struct Tenants {
	grants: Arc<Mutex<HashMap<String, Grant>>>,
	seen: Arc<Mutex<Vec<Request>>>,
}

impl Tenants {
	fn new() -> Self {
		let tenants = Self::default();
		tenants.grant("a", tenant_a());
		tenants.grant("b", tenant_b());
		tenants
	}

	/// What `credential` is granted from its next request on.
	fn grant(&self, credential: &str, grant: Grant) {
		self.grants.lock().unwrap().insert(credential.into(), grant);
	}

	/// Every `connect` that presented `credential`.
	fn connects(&self, credential: &str) -> Vec<Request> {
		let query = format!("jwt={credential}");
		self.seen
			.lock()
			.unwrap()
			.iter()
			.filter(|request| request.event == Event::Connect && request.query.as_deref() == Some(query.as_str()))
			.cloned()
			.collect()
	}

	async fn handle(State(tenants): State<Tenants>, Json(request): Json<Request>) -> Response {
		tenants.seen.lock().unwrap().push(request.clone());
		if matches!(request.event, Event::End { .. }) {
			return StatusCode::NO_CONTENT.into_response();
		}
		let credential = request
			.query
			.as_deref()
			.unwrap_or_default()
			.split('&')
			.find_map(|pair| pair.strip_prefix("jwt="));
		match credential.and_then(|credential| tenants.grants.lock().unwrap().get(credential).cloned()) {
			Some(grant) => Json(grant).into_response(),
			None => StatusCode::UNAUTHORIZED.into_response(),
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

struct Running {
	tenants: Tenants,
	quic: SocketAddr,
	http: SocketAddr,
	internal: SocketAddr,
	trigger: moq_relay::shutdown::Trigger,
	running: tokio::task::JoinHandle<anyhow::Result<()>>,
	_dir: tempfile::TempDir,
}

/// A relay admitting through [`Tenants`], serving QUIC from one io_uring worker
/// when `io_uring`, else from the shared Tokio listener.
async fn start(io_uring: bool) -> Running {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let tenants = Tenants::new();
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path(), false);
	config.auth.url = Some(tenants.spawn().await);
	if io_uring {
		config.runtime.workers = Some(1);
		config.runtime.pin = false;
		config.runtime.io_uring = true;
	}
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let quic = relay.quic_addr().expect("quic listener bound");
	let http = relay.web_addrs().http.expect("http listener bound");
	let internal = relay.internal().addr().expect("internal listener bound");
	let ready = relay.ready();
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	ready.wait().await.expect("relay ready");
	Running {
		tenants,
		quic,
		http,
		internal,
		trigger,
		running,
		_dir: dir,
	}
}

impl Running {
	/// A WebTransport URL at `path` presenting `credential`.
	fn url(&self, path: &str, credential: &str) -> url::Url {
		format!("https://{}{path}?jwt={credential}", self.quic).parse().unwrap()
	}

	/// Re-check every live session against the auth server now.
	async fn revalidate(&self) {
		let response = reqwest::Client::new()
			.post(format!("http://{}/sessions/revalidate", self.internal))
			.send()
			.await
			.expect("POST /sessions/revalidate");
		assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
	}

	async fn stop(self) {
		self.trigger.start();
		tokio::time::timeout(TIMEOUT, self.running)
			.await
			.expect("run did not return after the shutdown trigger")
			.expect("run panicked")
			.expect("run returned an error after shutdown");
	}
}

/// A publishing session: each broadcast holds one group with `hello` in `video`.
struct Publisher {
	origin: moq_net::origin::Producer,
	session: moq_tokio::Connection,
	held: Vec<Box<dyn std::any::Any + Send>>,
}

impl Publisher {
	async fn connect(url: url::Url) -> Self {
		let origin = moq_tokio::origin::spawn();
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
		Self {
			origin,
			session,
			held: Vec::new(),
		}
	}

	fn publish(&mut self, name: &str) {
		let broadcast = self.origin.create_broadcast(name).expect("create broadcast");
		broadcast.announce(Default::default()).expect("announce");
		let track = broadcast.create_track("video", timed()).expect("create track");
		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
			.expect("write frame");
		self.held.push(Box::new((broadcast, track, group)));
	}
}

/// A subscribing session recording every broadcast announced to it.
struct Watcher {
	session: moq_tokio::Connection,
	consumer: moq_net::origin::Consumer,
	seen: Arc<Mutex<Vec<String>>>,
	changed: Arc<tokio::sync::Notify>,
	_task: tokio::task::JoinHandle<()>,
}

impl Watcher {
	async fn connect(url: url::Url) -> Self {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let session = tokio::time::timeout(
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

		let seen = Arc::new(Mutex::new(Vec::new()));
		let changed = Arc::new(tokio::sync::Notify::new());
		let task = tokio::spawn({
			let seen = seen.clone();
			let changed = changed.clone();
			async move {
				while let Some(event) = announced.next().await {
					if let moq_net::announce::Event::Start(route) = event {
						seen.lock().unwrap().push(route.prefix.to_string());
						changed.notify_waiters();
					}
				}
			}
		});
		Self {
			session,
			consumer,
			seen,
			changed,
			_task: task,
		}
	}

	fn seen(&self) -> Vec<String> {
		self.seen.lock().unwrap().clone()
	}

	/// Wait until `name` is announced.
	async fn announced(&self, name: &str) {
		tokio::time::timeout(TIMEOUT, async {
			loop {
				let changed = self.changed.notified();
				if self.seen().iter().any(|seen| seen == name) {
					return;
				}
				changed.await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{name} was never announced; saw {:?}", self.seen()));
	}

	/// Read the first frame of `name`'s `video` track.
	async fn read(&self, name: &str) -> bytes::Bytes {
		tokio::time::timeout(TIMEOUT, async {
			let broadcast = self.consumer.request(name, None).await.expect("broadcast resolves");
			let mut track = broadcast
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.expect("subscribe");
			let mut group = track.recv_group().await.expect("group failed").expect("track closed");
			group
				.read_frame()
				.await
				.expect("frame failed")
				.expect("group closed")
				.payload
		})
		.await
		.unwrap_or_else(|_| panic!("reading {name} timed out"))
	}
}

/// Dial raw QUIC as moq-lite-06 and send a SETUP whose Path is exactly `target`,
/// bytes no URL type touched. The relay may close it once admission decides;
/// the caller watches the auth server for what it was admitted as.
async fn raw_setup(addr: SocketAddr, target: &str) -> (driver::endpoint::Endpoint, driver::connection::Connection) {
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let mut tls = connect.tls.build().expect("tls config");
	tls.alpn_protocols = vec![b"moq-lite-06".to_vec()];
	let crypto = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic crypto");
	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
			.expect("endpoint");
	let conn = endpoint
		.connect(&driver::client::Config::new(Arc::new(crypto)), addr, "localhost")
		.expect("connect")
		.await
		.expect("handshake");

	// SETUP type and size, then one parameter: Path (2), its length, and the bytes.
	// Every length here fits a one-byte varint.
	assert!(target.len() < 60, "the target must fit a one-byte varint");
	let mut params = vec![1, 2, target.len() as u8];
	params.extend_from_slice(target.as_bytes());
	let mut bytes = vec![0x01, params.len() as u8];
	bytes.extend(params);
	let mut setup = conn.open_uni(Default::default()).await.expect("open SETUP");
	setup.write_all(&bytes[..]).await.expect("write SETUP");
	setup.finish();
	(endpoint, conn)
}

/// Checks 1 to 4, 6, and 7 of the matrix, on one runtime.
async fn matrix(io_uring: bool) {
	let relay = start(io_uring).await;

	// Each credential dials the other tenant's path: the grant's root decides.
	let mut a = Publisher::connect(relay.url("/tenant-b", "a")).await;
	a.publish("public/cam");
	a.publish("secret/cam");
	let mut b = Publisher::connect(relay.url("/tenant-a", "b")).await;
	b.publish("probe");

	let a_view = Watcher::connect(relay.url("/", "a")).await;
	let b_view = Watcher::connect(relay.url("/tenant-a", "b")).await;

	// 1. A sees its own broadcasts, and not B's.
	a_view.announced("public/cam").await;
	a_view.announced("secret/cam").await;
	assert_eq!(&a_view.read("secret/cam").await[..], b"hello");
	// 3. B's read-only mount announces and delivers A's public camera.
	b_view.announced("probe").await;
	b_view.announced("shared/cam").await;
	assert_eq!(&b_view.read("shared/cam").await[..], b"hello");

	// 4. B publishes under its read-only mount, then a marker on the same session.
	// The relay handles one session's announces in order, so once the marker
	// reaches B's viewer the mount write has been decided. A refusal that closes
	// the session is the ordering point instead.
	b.publish("shared/pwn");
	b.publish("marker");
	tokio::select! {
		_ = b_view.announced("marker") => (),
		_ = b.session.closed() => (),
	}

	// 6. Dot segments on the wire, which no URL type resolved first. The relay may
	// admit them as `/tenant-b`, query intact, or refuse them, but never as
	// anything under `tenant-a`.
	let target = "/tenant-a/../tenant-b?jwt=b";
	let (endpoint, conn) = raw_setup(relay.quic, target).await;
	let decided = tokio::time::timeout(TIMEOUT, async {
		loop {
			if relay.tenants.connects("b").len() > 2 {
				return;
			}
			if conn.close_reason().is_some() {
				return;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	})
	.await;
	assert!(decided.is_ok(), "{target} was neither admitted nor refused");
	// Two `b` sessions came before: the publisher and the viewer.
	for request in relay.tenants.connects("b").iter().skip(2) {
		assert_eq!(
			request.path, "/tenant-b",
			"{target} reached admission as {:?}",
			request.path
		);
		assert_eq!(request.query.as_deref(), Some("jwt=b"), "{target} lost its query");
	}
	conn.close(driver::VarInt::from_u32(0), "done");
	endpoint.close(driver::VarInt::from_u32(0), "done").await;

	// Give anything that should not arrive time to.
	tokio::time::sleep(SETTLE).await;
	let a_seen = a_view.seen();
	assert!(
		!a_seen
			.iter()
			.any(|name| name.contains("probe") || name.contains("marker")),
		"A saw B's broadcasts: {a_seen:?}"
	);
	assert!(
		!a_seen.iter().any(|name| name.contains("pwn")),
		"B wrote into A through its read-only mount: {a_seen:?}"
	);
	// 2. B never sees A's secret.
	let b_seen = b_view.seen();
	assert!(
		!b_seen.iter().any(|name| name.contains("secret")),
		"B saw A's secret: {b_seen:?}"
	);

	// 7. Moving a live session's root from `tenant-b` to `tenant-a` closes it.
	relay.tenants.grant("b", tenant_a());
	relay.revalidate().await;
	let error = tokio::time::timeout(TIMEOUT, b_view.session.closed())
		.await
		.expect("a moved root must close the session")
		.expect_err("a moved root is a refusal");
	assert_eq!(
		error.connect_error(),
		Some(moq_tokio::ConnectError::Unauthorized),
		"{error:?}"
	);

	drop(a);
	drop(b);
	drop(a_view);
	drop(b_view);
	relay.stop().await;
}

#[tokio::test]
async fn tokio_keeps_tenants_apart() {
	matrix(false).await;
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_keeps_tenants_apart() {
	if !crate::runtime_uring::supported() {
		return;
	}
	matrix(true).await;
}

/// 5. The HTTP routes hold B to its own root. They run on the Tokio web stack
/// whichever runtime serves QUIC, so once is enough.
#[tokio::test]
async fn http_routes_keep_tenants_apart() {
	let relay = start(false).await;
	let mut a = Publisher::connect(relay.url("/", "a")).await;
	a.publish("secret/cam");
	let mut b = Publisher::connect(relay.url("/", "b")).await;
	b.publish("probe");
	// The HTTP routes report only what has reached the relay.
	Watcher::connect(relay.url("/", "a"))
		.await
		.announced("secret/cam")
		.await;
	Watcher::connect(relay.url("/", "b")).await.announced("probe").await;

	let http = reqwest::Client::new();
	let get = |path: &str| http.get(format!("http://{}{path}", relay.http)).send();

	let announced = get("/announced/tenant-a?jwt=b").await.expect("announced request");
	let status = announced.status();
	let body = announced.text().await.expect("announced body");
	assert!(!body.contains("secret"), "{status}: {body}");
	if status == 200 {
		assert!(body.contains("probe"), "B's listing is not its own root: {body}");
	}

	// A fetch waits up to 30 seconds for its broadcast to appear. A's secret is
	// already here, so a fetch that would serve it answers at once; one still
	// waiting has served nothing.
	if let Ok(fetch) = tokio::time::timeout(Duration::from_secs(2), get("/fetch/tenant-a/secret/cam/video?jwt=b")).await
	{
		assert_ne!(fetch.expect("fetch request").status(), 200, "B fetched A's secret");
	}

	drop(a);
	drop(b);
	relay.stop().await;
}

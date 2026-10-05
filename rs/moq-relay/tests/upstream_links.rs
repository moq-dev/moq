//! Upstream links over real TCP sessions: a relay never offers a route learned on
//! one upstream link to another, whichever side dialed, and everything else
//! still transits.
//!
//! Each relay is a [`cluster::Cluster`] serving qmux over TLS on TCP (`tls://`),
//! the way edge links run, through an embedded auth decider that reads the
//! dialed path: `/upstream` admits the dialer as an upstream peer, `/peer` as a
//! plain one.

use std::collections::BTreeMap;
use std::time::Duration;

use moq_auth::{Grant, Pattern, lease};
use moq_net::{announce, origin};
use moq_relay::{
	Connection, auth,
	cluster::{self, Peer},
};

const TIMEOUT: Duration = Duration::from_secs(15);

/// Bound `fut` by [`TIMEOUT`], naming the step that hung.
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	tokio::time::timeout(TIMEOUT, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// Run on a big-stack thread: several relays' transport types make the test
/// future large enough to overflow libtest's default stack, as in
/// `goaway_cluster.rs`.
fn run_cluster_test<F>(fut: F)
where
	F: std::future::Future<Output = ()> + Send + 'static,
{
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(move || {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("build test runtime")
				.block_on(fut);
		})
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
}

/// One relay: its origin, listen port, and every task it runs. Dropping it
/// aborts them all, closing its sessions like a crash.
struct Node {
	origin: origin::Producer,
	port: u16,
	_tasks: tokio::task::JoinSet<()>,
}

impl Node {
	/// Dial this node from another. `upstream` is the dialer's own mark; `as_upstream`
	/// asks this node to admit the dialer as its upstream.
	fn dial(&self, upstream: bool, as_upstream: bool) -> Peer {
		let path = if as_upstream { "upstream" } else { "peer" };
		Peer::new(format!("tls://127.0.0.1:{}/{path}", self.port)).with_upstream(upstream)
	}
}

/// What the embedded decider grants a dial at `path`: everything, as a peer,
/// upstream when asked.
fn grant(path: &str) -> Grant {
	let all: moq_net::Patterns = [Pattern::all()].into_iter().collect();
	let mut grant = Grant::new(all.clone(), all);
	grant.root = Some(String::new());
	grant.peer = true;
	grant.upstream = path == "/upstream";
	grant
}

async fn node(id: u64, connect: Vec<Peer>) -> Node {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let mut tasks = tokio::task::JoinSet::new();

	let mut listen = moq_tokio::listen::Config::default();
	listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	listen.tcp.tls = Some(true);
	listen.tls.generate = vec!["localhost".into()];
	let mut listener = listen
		.init(Default::default())
		.expect("server init")
		.listen()
		.await
		.expect("listen");
	let port = listener.tcp_local_addr().expect("tcp listener bound").port();

	let mut client = moq_tokio::connect::Config::default();
	client.tls.insecure = Some(true);
	let client = client.init(Default::default()).expect("client init");

	let mut config = cluster::Config::default();
	config.id = Some(id);
	config.connect = connect;
	let cluster = cluster::Cluster::new(cluster::Options::new(config))
		.expect("cluster init")
		.with_client(client);

	let (auth, mut admissions) = auth::Auth::embedded(format!("node-{id}"));
	tasks.spawn(async move {
		while let Some(admission) = admissions.next().await {
			let grant = grant(&admission.request.path);
			admission.grant(lease::Consumer::fixed(grant));
		}
	});

	let serving = cluster.clone();
	tasks.spawn(async move {
		let mut sessions = tokio::task::JoinSet::new();
		while let Some(request) = listener.accept().await {
			let connection = Connection::new(request, serving.clone(), auth.clone());
			sessions.spawn(async move {
				let _ = connection.run().await;
			});
		}
	});

	let started = cluster.clone().start().await.expect("cluster start");
	tasks.spawn(async move {
		let _ = started.run().await;
	});

	Node {
		origin: cluster.origin.clone(),
		port,
		_tasks: tasks,
	}
}

/// A broadcast writing a group every 50ms on `origin`, so a subscriber attaching
/// at any point receives one. Dropping it ends the broadcast.
struct Publisher {
	_broadcast: moq_net::broadcast::Producer,
	writer: tokio::task::AbortHandle,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		self.writer.abort();
	}
}

fn publish(origin: &origin::Producer, path: &str) -> Publisher {
	let broadcast = origin.publish(path, Default::default()).expect("publish");
	let track = broadcast.create_track("video", None).expect("create track");
	let writer = tokio::spawn(async move {
		loop {
			let Ok(mut group) = track.append_group() else { break };
			if group.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref()).is_err() || group.finish().is_err() {
				break;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	})
	.abort_handle();
	Publisher {
		_broadcast: broadcast,
		writer,
	}
}

/// Subscribe to `path` through `origin`'s routes and read one frame.
async fn read_frame(origin: &origin::Producer, path: &str) {
	let consumer = origin.consume();
	let broadcast = within(&format!("route to {path}"), consumer.routed_broadcast(path))
		.await
		.unwrap_or_else(|err| panic!("{path} unroutable: {err}"));
	let subscription = moq_net::track::Subscription::default().with_max_age(Duration::from_secs(1));
	let mut track = within(
		&format!("subscribe to {path}"),
		broadcast.track("video").expect("track handle").subscribe(subscription),
	)
	.await
	.expect("subscribe");
	let mut group = within(&format!("group of {path}"), track.recv_group())
		.await
		.expect("recv group")
		.expect("track closed");
	within(&format!("frame of {path}"), group.read_frame())
		.await
		.expect("read frame")
		.expect("group closed");
}

/// Each path `origin` routes now, with its route's hop ids, oldest first.
///
/// Read from a fresh cursor's initial set, which reflects the route table as it
/// stands, unlike a live cursor that holds a best-route change back briefly.
async fn routes(origin: &origin::Producer) -> BTreeMap<String, Vec<u64>> {
	let mut announced = origin.consume().announced();
	let mut routes = BTreeMap::new();
	loop {
		match announced.next().await.expect("origin closed") {
			announce::Event::Start(announce) | announce::Event::Update(announce) => {
				let hops = announce.route.hops.iter().map(|hop| hop.id()).collect();
				routes.insert(announce.prefix.as_str().to_string(), hops);
			}
			announce::Event::End(announce) => {
				routes.remove(announce.prefix.as_str());
			}
			announce::Event::Live => return routes,
		}
	}
}

/// Wait until `origin`'s routes satisfy `ready`, and return them.
async fn routes_when(
	step: &str,
	origin: &origin::Producer,
	ready: impl Fn(&BTreeMap<String, Vec<u64>>) -> bool,
) -> BTreeMap<String, Vec<u64>> {
	within(step, async {
		// Created first, so a change landing between two reads still wakes us.
		let mut changes = origin.consume().announced();
		loop {
			let routes = routes(origin).await;
			if ready(&routes) {
				return routes;
			}
			changes.next().await.expect("origin closed");
		}
	})
	.await
}

fn has(routes: &BTreeMap<String, Vec<u64>>, paths: &[&str]) -> bool {
	paths.iter().all(|path| routes.contains_key(*path))
}

const R: u64 = 1;
const C1: u64 = 11;
const C2: u64 = 12;
const E1: u64 = 21;
const E2: u64 = 22;

/// Two regions: two edges whose links to two cores are upstream, and one relay
/// that also serves clients. Both edges send a path to the same core, an edge's
/// broadcast reaches the far relay, and losing a core moves only its paths.
#[test]
fn tiered_regions() {
	run_cluster_test(tiered_regions_inner());
}

async fn tiered_regions_inner() {
	let r = node(R, vec![]).await;
	let c1 = node(C1, vec![r.dial(false, false)]).await;
	let c2 = node(C2, vec![r.dial(false, false)]).await;
	let e1 = node(E1, vec![c1.dial(true, false), c2.dial(true, false)]).await;
	let e2 = node(E2, vec![c1.dial(true, false), c2.dial(true, false)]).await;

	let paths: Vec<String> = (0..8).map(|i| format!("live/{i}")).collect();
	let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
	let _published: Vec<_> = paths.iter().map(|path| publish(&r.origin, path)).collect();
	routes_when("both cores learn the far region", &c1.origin, |routes| {
		has(routes, &paths)
	})
	.await;
	routes_when("both cores learn the far region", &c2.origin, |routes| {
		has(routes, &paths)
	})
	.await;

	// A route is delivered before a later one: once an edge holds both cores'
	// markers, it holds both cores' routes to the far region too.
	let _c1_mark = publish(&c1.origin, "zz/c1");
	let _c2_mark = publish(&c2.origin, "zz/c2");
	let marks = ["zz/c1", "zz/c2"];
	let at_e1 = routes_when("edge 1 hears both cores", &e1.origin, |routes| has(routes, &marks)).await;
	let at_e2 = routes_when("edge 2 hears both cores", &e2.origin, |routes| has(routes, &marks)).await;

	// Every edge sends a given path to the same core, so the far region serves
	// one subscription per broadcast whichever edges watch it.
	let core = |routes: &BTreeMap<String, Vec<u64>>, path: &str| *routes[path].last().expect("hops");
	for path in &paths {
		assert_eq!(at_e1[*path].len(), 2, "{path} at edge 1: {:?}", at_e1[*path]);
		assert_eq!(core(&at_e1, path), core(&at_e2, path), "{path} split across cores");
	}
	read_frame(&e1.origin, paths[0]).await;
	read_frame(&e2.origin, paths[0]).await;

	// A client on the far relay reaches a broadcast published on an edge.
	let _edge = publish(&e1.origin, "edge/cam");
	read_frame(&r.origin, "edge/cam").await;

	// Losing a core moves only its paths.
	let on = |core_id: u64| -> Vec<&str> { paths.iter().copied().filter(|p| core(&at_e1, p) == core_id).collect() };
	let (on_c1, on_c2) = (on(C1), on(C2));
	assert!(
		!on_c1.is_empty() && !on_c2.is_empty(),
		"paths all on one core: {at_e1:?}"
	);
	drop(c1);
	let after = routes_when("edge 1 drops the lost core", &e1.origin, |routes| {
		has(routes, &paths) && routes.values().all(|hops| !hops.contains(&C1))
	})
	.await;
	for path in &on_c2 {
		assert_eq!(after[*path], at_e1[*path], "{path} moved off the surviving core");
	}
	for path in &on_c1 {
		assert_eq!(after[*path], [R, C2], "{path} did not fail over");
	}
}

/// A route from one core never reaches the other through an edge, whether the
/// edge dialed the core or the core dialed the edge.
#[test]
fn edges_never_transit_between_cores() {
	run_cluster_test(edges_never_transit_between_cores_inner());
}

async fn edges_never_transit_between_cores_inner() {
	let c1 = node(C1, vec![]).await;
	let e1 = node(E1, vec![c1.dial(true, false)]).await;
	// The core dials this edge, which still treats it as upstream.
	let c2 = node(C2, vec![e1.dial(false, true)]).await;
	let e2 = node(E2, vec![c1.dial(true, false), c2.dial(true, false)]).await;

	let _core = publish(&c1.origin, "live/c1");
	routes_when("edge 1 learns core 1", &e1.origin, |routes| has(routes, &["live/c1"])).await;
	routes_when("edge 2 learns core 1", &e2.origin, |routes| has(routes, &["live/c1"])).await;

	// Each edge's own broadcast reaches core 2 after anything it would offer from
	// core 1, so once both arrive, core 2 has seen every offer it will get.
	let _e1 = publish(&e1.origin, "zz/e1");
	let _e2 = publish(&e2.origin, "zz/e2");
	let at_c2 = routes_when("core 2 hears both edges", &c2.origin, |routes| {
		has(routes, &["zz/e1", "zz/e2"])
	})
	.await;
	assert!(
		!at_c2.contains_key("live/c1"),
		"core 1's broadcast reached core 2 through an edge: {at_c2:?}"
	);
	read_frame(&c2.origin, "zz/e1").await;
	read_frame(&c1.origin, "zz/e2").await;
}

/// A drone mesh node, a drone with a CDN uplink marked upstream, and a CDN relay
/// in a line: the mesh reaches the CDN and the CDN reaches the mesh.
#[test]
fn uplink_transits_both_ways() {
	run_cluster_test(uplink_transits_both_ways_inner());
}

async fn uplink_transits_both_ways_inner() {
	let cdn = node(31, vec![]).await;
	let uplink = node(32, vec![cdn.dial(true, false)]).await;
	let mesh = node(33, vec![uplink.dial(false, false)]).await;

	let _mesh = publish(&mesh.origin, "mesh/cam");
	let _cdn = publish(&cdn.origin, "cdn/cam");
	read_frame(&cdn.origin, "mesh/cam").await;
	read_frame(&mesh.origin, "cdn/cam").await;
}

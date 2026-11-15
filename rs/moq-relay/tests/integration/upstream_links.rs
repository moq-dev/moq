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
use moq_net::origin;
use moq_relay::{
	Connection, auth,
	cluster::{self, Peer},
};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(15);

/// Bound `fut` by [`TIMEOUT`], naming the step that hung.
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	kio::clock::timeout(TIMEOUT, fut)
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
	kio::clock::host_time(); // real relays on real sockets
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

const LITE_07: &str = "moq-lite-07-wip";

async fn node(connect: Vec<Peer>) -> Node {
	let _ = rind_rustls::default_provider().unwrap().install_default();
	let mut tasks = tokio::task::JoinSet::new();

	let mut listen = moq_tokio::listen::Config::default();
	listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse addr"));
	listen.tcp.tls = Some(true);
	listen.tls.generate = vec!["localhost".into()];
	// Links speak lite-07, which carries each route's instance, so every edge ranks a path's
	// routes alike: over an older version each edge mints instances of its own.
	listen.version = vec![LITE_07.parse().expect("lite-07")];
	let mut listener = listen
		.init(Default::default())
		.expect("server init")
		.listen()
		.await
		.expect("listen");
	let port = listener.tcp_local_addr().expect("tcp listener bound").port();

	let mut client = moq_tokio::connect::Config::default();
	client.tls.insecure = Some(true);
	client.version = vec![LITE_07.parse().expect("lite-07")];

	let mut config = cluster::Config::default();
	config.connect = connect;
	let cluster = crate::spawn_cluster(cluster::Options::new(config)).with_connect(client, Default::default());
	let id = cluster.id();

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
		while let Ok(request) = listener.accept().await {
			let connection = Connection::new(request, serving.clone(), auth.clone());
			sessions.spawn(async move {
				connection.run().await;
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
	_route: moq_net::origin::Route,
	writer: tokio::task::AbortHandle,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		self.writer.abort();
	}
}

fn publish(origin: &origin::Producer, path: &str) -> Publisher {
	let route = origin.route(path, Default::default()).expect("publish");
	let track = route.broadcast().create_track("video", timed()).expect("create track");
	let writer = tokio::spawn(async move {
		loop {
			let Ok(mut group) = track.append_group(track.largest().map_or(0, |largest| largest.group + 1)) else {
				break;
			};
			if group
				.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), b"hello".as_ref())
				.is_err() || group.close().is_err()
			{
				break;
			}
			kio::clock::sleep(Duration::from_millis(50)).await;
		}
	})
	.abort_handle();
	Publisher { _route: route, writer }
}

/// Subscribe to `path` through `origin`'s routes and read one frame.
async fn read_frame(origin: &origin::Producer, path: &str) {
	let consumer = origin.consume();
	let broadcast = within(
		&format!("route to {path}"),
		consumer.follow(path, None).unwrap().available(),
	)
	.await
	.unwrap_or_else(|err| panic!("{path} unroutable: {err}"))
	.expect("the first change is an instance");
	let subscription = moq_net::track::Subscription::default()
		.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(1)));
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

/// Wait until `origin`'s routes satisfy `ready`, and return each path it routes with
/// its route's hop ids, oldest first.
///
/// Each read is a fresh cursor's initial set, which reflects the route table as it
/// stands, unlike a live cursor that holds a best-route change back briefly. The
/// cursor replays that set when it registers, all starts and none held, so it drains
/// without waiting, and then waits on that same cursor. Having delivered no end, it
/// quarantines no stale path, so every later change to the table wakes it. A cursor
/// kept across reads would not: once it delivers a route's end, it holds back a
/// worse path's return and drops it silently when that path is withdrawn too, while
/// a read in between still sees it.
async fn routes_when(
	step: &str,
	origin: &origin::Producer,
	ready: impl Fn(&BTreeMap<String, Vec<u64>>) -> bool,
) -> BTreeMap<String, Vec<u64>> {
	within(step, async {
		loop {
			let mut changes = origin.consume().routes();
			let mut routes = BTreeMap::new();
			while let Some(event) = changes.try_next() {
				let origin::RouteEvent {
					kind: origin::RouteKind::Start,
					announce,
				} = event
				else {
					panic!("an initial set is only starts: got {event:?}");
				};
				let hops = moq_net::fuzz::route_hops(&announce.route);
				routes.insert(announce.prefix.as_str().to_string(), hops);
			}
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

/// Two regions: two edges whose links to two cores are upstream, and one relay
/// that also serves clients. The far region's broadcasts reach both edges, an edge's
/// broadcast reaches the far relay, and losing a core leaves every path reachable
/// through the other. Lite-07 carries no hop chain, so which core a path takes is the
/// route layer's choice, not something this asserts.
#[test]
fn tiered_regions() {
	run_cluster_test(tiered_regions_inner());
}

async fn tiered_regions_inner() {
	let r = node(vec![]).await;
	let c1 = node(vec![r.dial(false, false)]).await;
	let c2 = node(vec![r.dial(false, false)]).await;
	let e1 = node(vec![c1.dial(true, false), c2.dial(true, false)]).await;
	let e2 = node(vec![c1.dial(true, false), c2.dial(true, false)]).await;

	let paths: Vec<String> = (0..8).map(|i| format!("live/{i}")).collect();
	let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
	let _published: Vec<_> = paths.iter().map(|path| publish(&r.origin, path)).collect();
	for (step, at) in [
		("core 1 learns the far region", &c1),
		("core 2 learns the far region", &c2),
		("edge 1 learns the far region", &e1),
		("edge 2 learns the far region", &e2),
	] {
		routes_when(step, &at.origin, |routes| has(routes, &paths)).await;
	}
	read_frame(&e1.origin, paths[0]).await;
	read_frame(&e2.origin, paths[0]).await;

	// A client on the far relay reaches a broadcast published on an edge.
	let _edge = publish(&e1.origin, "edge/cam");
	read_frame(&r.origin, "edge/cam").await;

	// Losing a core: once edge 1 sees core 1's own broadcast go, every route it had
	// through core 1 went with it, and each path is still served through core 2.
	let _c1_mark = publish(&c1.origin, "zz/c1");
	routes_when("edge 1 hears core 1", &e1.origin, |routes| has(routes, &["zz/c1"])).await;
	drop(c1);
	routes_when("edge 1 drops the lost core", &e1.origin, |routes| {
		has(routes, &paths) && !routes.contains_key("zz/c1")
	})
	.await;
	for path in &paths {
		read_frame(&e1.origin, path).await;
	}
}

/// A route from one core never reaches the other through an edge, whether the
/// edge dialed the core or the core dialed the edge.
#[test]
fn edges_never_transit_between_cores() {
	run_cluster_test(edges_never_transit_between_cores_inner());
}

async fn edges_never_transit_between_cores_inner() {
	let c1 = node(vec![]).await;
	let e1 = node(vec![c1.dial(true, false)]).await;
	// The core dials this edge, which still treats it as upstream.
	let c2 = node(vec![e1.dial(false, true)]).await;
	let e2 = node(vec![c1.dial(true, false), c2.dial(true, false)]).await;

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
	let cdn = node(vec![]).await;
	let uplink = node(vec![cdn.dial(true, false)]).await;
	let mesh = node(vec![uplink.dial(false, false)]).await;

	let _mesh = publish(&mesh.origin, "mesh/cam");
	let _cdn = publish(&cdn.origin, "cdn/cam");
	read_frame(&cdn.origin, "mesh/cam").await;
	read_frame(&mesh.origin, "cdn/cam").await;
}

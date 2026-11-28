//! A draining relay drains the cluster links it dialed, as it does those it accepted:
//! each peer hears its GOAWAY and tells the others the relay is draining, and the
//! relay closes each link once its accepted sessions have left and nothing reads
//! through it.
//!
//! Three real relays on loopback: C dials A, and L dials both. L drains, so A and C
//! each hear L's GOAWAY themselves, and the first to hear it sends the other a DRAIN
//! for it, while L, which sent the GOAWAY, sends none. A reader at A reads L's
//! broadcast, so a link stays up while it reads, and L exits once it stops.

use std::time::Duration;

use moq_net::origin::{Cost, RouteKind};
use moq_net::stats::{Registry, Routes, Withdrawal};
use moq_relay::{Config, Relay, shutdown};

/// Only a hang reaches it.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// One relay running on the test runtime.
struct Node {
	port: u16,
	stats: Registry,
	origin: moq_net::origin::Producer,
	trigger: shutdown::Trigger,
	shutdown: shutdown::Observer,
	running: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Node {
	/// A relay `name` draining over `drain`, dialing each port in `peers` as a cluster
	/// peer and admitting every session as one.
	async fn start(name: &str, peers: &[u16], drain: Duration) -> Self {
		let mut config = Config::default();
		config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		config.listen.tls.generate = vec!["localhost".into()];
		config.drain_timeout = Some(drain.into());
		// Counted, but published only once.
		config.stats.enabled = true;
		config.stats.node = Some(name.into());
		config.stats.interval = Some(Duration::from_secs(3600).into());
		config.connect.bind = Some("127.0.0.1:0".parse().unwrap());
		config.connect.tls.insecure = Some(true);
		config.connect.websocket.enabled = Some(false);
		// Only lite-07 carries the route layer, which a relay offers only when asked.
		let lite07: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		config.listen.version = vec![lite07];
		config.connect.version = vec![lite07];
		config.cluster.connect = peers
			.iter()
			.map(|port| moq_relay::cluster::Peer::new(format!("https://127.0.0.1:{port}/")))
			.collect();
		let mut relay = Relay::load(config).await.expect("load relay").with_signals(false);
		// A peer grant, as a deployment's auth gives its relays: only a peer exchanges routes.
		let mut admissions = relay.admissions().expect("embedded auth");
		tokio::spawn(async move {
			let mut leases = Vec::new();
			while let Some(admission) = admissions.next().await {
				let mut grant = moq_auth::Grant::new(moq_auth::Prefix::all(), moq_auth::Prefix::all());
				grant.link = Some(moq_auth::Link::Peer);
				let (lease, consumer) = moq_auth::lease::Producer::new(grant);
				admission.grant(consumer);
				leases.push(lease);
			}
		});
		let port = relay.addr().expect("bound UDP").port();
		let stats = relay.cluster().stats.clone();
		let origin = relay.cluster().origin.clone();
		let trigger = relay.shutdown_trigger().clone();
		let shutdown = relay.shutdown().clone();
		let ready = relay.ready();
		let running = tokio::spawn(relay.run());
		ready.wait().await.expect("relay ready");
		Self {
			port,
			stats,
			origin,
			trigger,
			shutdown,
			running,
		}
	}

	/// The route-layer counters of every session, summed over tiers.
	fn routes(&self) -> Routes {
		let mut routes = Routes::default();
		for (_, presence) in self.stats.snapshot().sessions() {
			routes.add(presence.routes);
		}
		routes
	}

	/// Sessions open now, accepted and dialed.
	fn active(&self) -> u64 {
		self.stats
			.snapshot()
			.sessions()
			.iter()
			.map(|(_, presence)| presence.active())
			.sum()
	}

	/// Publish the broadcast `name` from this relay.
	fn publish(&self, name: &str) -> moq_net::origin::Route {
		self.origin
			.route(name, moq_net::origin::RouteOptions::default())
			.expect("create broadcast")
	}

	/// Resolve once this relay's route for `name` comes straight from its publisher: one
	/// unpriced link, costing 1, where a route through another neighbour costs 2. The
	/// relay forwards that route to its other neighbours as it takes it.
	async fn direct(&self, name: &str) {
		let mut routes = self.origin.consume().routes();
		kio::clock::timeout(TIMEOUT, async {
			loop {
				let event = routes.next().await.expect("origin open");
				let live = matches!(event.kind, RouteKind::Start | RouteKind::Update | RouteKind::Restart);
				let route = &event.announce;
				if live && route.prefix.as_str() == name && route.route.cost() == Cost::new(1) {
					return;
				}
			}
		})
		.await
		.unwrap_or_else(|_| panic!("timed out taking a direct route to {name}"));
	}

	/// Resolve once this relay has an active route for `name`.
	async fn learns(&self, name: &str) {
		kio::clock::timeout(TIMEOUT, self.origin.consume().follow(name, None).unwrap().available())
			.await
			.unwrap_or_else(|_| panic!("timed out learning {name}"))
			.expect("resolves");
	}

	/// Drain and wait for the relay to exit.
	async fn stop(self) {
		self.trigger.start();
		kio::clock::timeout(TIMEOUT, self.running)
			.await
			.expect("relay exits")
			.expect("relay task")
			.expect("relay run");
	}
}

/// A and C linked, then L dialing both and draining over `drain`, with a reader at A
/// reading L's broadcast `l` through L's links.
struct Mesh {
	a: Node,
	c: Node,
	l: Node,
	_broadcasts: [moq_net::origin::Route; 3],
	track: moq_net::track::Producer,
	reader: moq_net::track::Subscriber,
}

impl Mesh {
	async fn start(drain: Duration) -> Self {
		kio::clock::host_time(); // real relays on real sockets
		moq_tokio::crypto::install_default();

		// A and C first, until each learns the other's broadcast over the one link between
		// them: their route exchange is then up, so each forwards L's routes to the other as
		// it learns them, which is what a DRAIN for L is later sent along.
		let a = Node::start("a", &[], Duration::ZERO).await;
		let c = Node::start("c", &[a.port], Duration::ZERO).await;
		let broadcast_a = a.publish("a");
		let broadcast_c = c.publish("c");
		a.learns("c").await;
		c.learns("a").await;

		// Then L, dialing both, until each neighbour takes L's route straight from L and so
		// has forwarded it to the other.
		let l = Node::start("l", &[a.port, c.port], drain).await;
		let broadcast_l = l.publish("l");
		let track = broadcast_l.broadcast().create_track("video", timed()).unwrap();
		kio::clock::timeout(TIMEOUT, async {
			while l.active() < 2 {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("L dials both");
		a.direct("l").await;
		c.direct("l").await;

		let remote = kio::clock::timeout(TIMEOUT, a.origin.consume().follow("l", None).unwrap().available())
			.await
			.expect("A learns l")
			.expect("resolves")
			.expect("announced");
		let reader = remote.track("video").unwrap().subscribe(None).await.unwrap();
		let mut mesh = Self {
			a,
			c,
			l,
			_broadcasts: [broadcast_a, broadcast_c, broadcast_l],
			track,
			reader,
		};
		mesh.reads().await;
		mesh
	}

	/// Write a group at L and wait for the reader at A to receive it.
	async fn reads(&mut self) {
		let sequence = self.track.largest().map_or(0, |largest| largest.group + 1);
		let mut group = self
			.track
			.append_group(moq_net::Timestamp::from_millis(sequence).unwrap())
			.unwrap();
		group
			.write_frame(moq_net::Timestamp::from_millis(sequence).unwrap(), b"l".as_ref())
			.unwrap();
		group.finish().unwrap();
		loop {
			let group = kio::clock::timeout(TIMEOUT, self.reader.recv_group())
				.await
				.expect("the reader at A starved")
				.expect("the read failed")
				.expect("the track ended");
			if group.start >= sequence {
				return;
			}
		}
	}

	/// Each neighbour hears the GOAWAY on the link L dialed and tells the other, unless the
	/// other told it first: a DRAIN never goes back to the neighbour it came from. Either
	/// way the link between them carries one, and each one sent arrives.
	async fn told(&self) {
		let told = || {
			let (a, c) = (self.a.routes(), self.c.routes());
			let (a_sent, c_sent) = (a.sent.count(Withdrawal::Drain), c.sent.count(Withdrawal::Drain));
			a_sent + c_sent > 0
				&& a_sent == c.received.count(Withdrawal::Drain)
				&& c_sent == a.received.count(Withdrawal::Drain)
		};
		kio::clock::timeout(TIMEOUT, async {
			while !told() {
				tokio::task::yield_now().await;
			}
		})
		.await
		.unwrap_or_else(|_| {
			panic!(
				"no DRAIN for L crossed between A and C: A {:?}, C {:?}",
				self.a.routes(),
				self.c.routes()
			)
		});
	}
}

/// L holds a link while the reader at A reads through it, and exits once it stops,
/// long before its deadline, with nothing force-closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draining_relay_closes_its_dialed_links_once_nothing_reads_them() {
	// Far past every wait below, so only links closing on their own let L exit in time.
	const DRAIN: Duration = Duration::from_secs(600);
	let mut mesh = Mesh::start(DRAIN).await;

	let started = kio::clock::now();
	mesh.l.trigger.start();
	mesh.told().await;

	// The reader still reads, so L still serves it.
	mesh.reads().await;
	assert!(!mesh.l.running.is_finished(), "L exited under a reader");

	// The draining relay, which sent the GOAWAY, names itself in no DRAIN.
	assert_eq!(
		mesh.l.routes().sent.count(Withdrawal::Drain),
		0,
		"the draining relay sent a DRAIN"
	);

	let Mesh { a, c, l, reader, .. } = mesh;
	drop(reader);
	let observer = l.shutdown.clone();
	l.stop().await;
	assert!(started.elapsed() < DRAIN, "L held its links to the deadline");
	assert_eq!(
		observer.drained().await.forced,
		0,
		"a link closed on its own counted as forced"
	);

	c.stop().await;
	a.stop().await;
}

/// A link a reader still reads through at the deadline is closed by it, and counted as
/// forced like any session that never left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dialed_link_still_read_at_the_deadline_is_forced() {
	const DRAIN: Duration = Duration::from_secs(2);
	let mesh = Mesh::start(DRAIN).await;

	let started = kio::clock::now();
	let observer = mesh.l.shutdown.clone();
	let Mesh { a, c, l, reader, .. } = mesh;
	l.stop().await;
	assert!(started.elapsed() >= DRAIN, "L left before its drain deadline");
	assert!(
		observer.drained().await.forced > 0,
		"the read link was not counted as forced"
	);

	drop(reader);
	c.stop().await;
	a.stop().await;
}

/// A publisher's session to the relay at `port`, publishing the broadcast `name`.
fn publisher(port: u16, name: &str) -> (moq_tokio::Connection, moq_net::origin::Route) {
	let mut config = moq_tokio::connect::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.insecure = Some(true);
	config.websocket.enabled = Some(false);
	config.version = vec!["moq-lite-07-wip".parse().unwrap()];
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.route(name, moq_net::origin::RouteOptions::default()).unwrap();
	let url: url::Url = format!("https://127.0.0.1:{port}/").parse().unwrap();
	let connection = config
		.init(Default::default())
		.unwrap()
		.with_publisher(origin.consume())
		// Redials the draining relay once and keeps serving there, as a publisher
		// whose replacement relay has yet to come up.
		.with_restart(moq_tokio::Restart::Handoff)
		.connect(url);
	(connection, broadcast)
}

/// A link nothing reads through still announces the broadcast of a publisher that is yet
/// to leave the draining relay, so a neighbour whose only route is that link keeps it,
/// and the relay exits once the publisher leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dialed_link_announces_until_the_relays_publishers_leave() {
	const DRAIN: Duration = Duration::from_secs(600);
	let Mesh { a, c, l, reader, .. } = Mesh::start(DRAIN).await;
	drop(reader);

	let (publisher, _broadcast) = publisher(l.port, "p");
	a.learns("p").await;
	let mut routes = a.origin.consume().routes();

	let started = kio::clock::now();
	l.trigger.start();
	// Long past an idle link's close, were the publisher not held to.
	let withdrawn = kio::clock::timeout(Duration::from_secs(2), async {
		loop {
			let event = routes.next().await.expect("origin open");
			let gone = matches!(event.kind, RouteKind::End | RouteKind::Offline);
			if gone && event.announce.prefix.as_str() == "p" {
				return;
			}
		}
	})
	.await;
	assert!(withdrawn.is_err(), "A lost p while its publisher was still on L");
	assert!(!l.running.is_finished(), "L exited under a publisher");

	publisher.close().await.expect("publisher closes");
	let observer = l.shutdown.clone();
	l.stop().await;
	assert!(started.elapsed() < DRAIN, "L held its links to the deadline");
	assert_eq!(
		observer.drained().await.forced,
		0,
		"a link closed on its own counted as forced"
	);

	c.stop().await;
	a.stop().await;
}

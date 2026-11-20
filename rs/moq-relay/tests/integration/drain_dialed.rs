//! A draining relay drains the cluster links it dialed, as it does those it accepted:
//! each peer hears its GOAWAY and tells the others the relay is draining, well before
//! any link drops.
//!
//! Three real relays on loopback: C dials A, and L dials both. L drains, so A and C
//! each hear L's GOAWAY themselves, and the first to hear it sends the other a DRAIN
//! for it, while L, which sent the GOAWAY, sends none. Every link stays up until L's
//! drain deadline, so no relay counts a DEAD until then.

use std::time::Duration;

use moq_net::origin::{Cost, RouteKind};
use moq_net::stats::{Registry, Routes, Withdrawal};
use moq_relay::{Config, Relay, shutdown};

/// Only a hang reaches it.
const TIMEOUT: Duration = Duration::from_secs(10);

/// How long L holds its drain open. Long enough that every DRAIN lands well inside it.
const DRAIN: Duration = Duration::from_secs(2);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draining_relay_drains_the_links_it_dialed() {
	kio::clock::host_time(); // real relays on real sockets
	moq_tokio::crypto::install_default().unwrap();

	// A and C first, until each learns the other's broadcast over the one link between
	// them: their route exchange is then up, so each forwards L's routes to the other as
	// it learns them, which is what a DRAIN for L is later sent along.
	let a = Node::start("a", &[], Duration::ZERO).await;
	let c = Node::start("c", &[a.port], Duration::ZERO).await;
	let _a = a.publish("a");
	let _c = c.publish("c");
	a.learns("c").await;
	c.learns("a").await;

	// Then L, dialing both, until each neighbour takes L's route straight from L and so
	// has forwarded it to the other.
	let l = Node::start("l", &[a.port, c.port], DRAIN).await;
	let _l = l.publish("l");
	kio::clock::timeout(TIMEOUT, async {
		while l.active() < 2 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("L dials both");
	a.direct("l").await;
	c.direct("l").await;

	let started = kio::clock::now();
	l.trigger.start();

	// Each neighbour hears the GOAWAY on the link L dialed and tells the other, unless the
	// other told it first: a DRAIN never goes back to the neighbour it came from. Either
	// way the link between them carries one, and each one sent arrives.
	let told = || {
		let (a, c) = (a.routes(), c.routes());
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
			a.routes(),
			c.routes()
		)
	});

	// ...before any link dropped: L holds them open to its deadline.
	assert!(started.elapsed() < DRAIN, "the DRAINs took {:?}", started.elapsed());
	for (name, node) in [("A", &a), ("C", &c)] {
		let routes = node.routes();
		assert_eq!(
			routes.sent.count(Withdrawal::Dead),
			0,
			"{name} sent a DEAD before L left"
		);
		assert_eq!(
			routes.received.count(Withdrawal::Dead),
			0,
			"{name} received a DEAD before L left"
		);
	}
	assert!(!l.running.is_finished(), "L exited before its drain deadline");

	// The draining relay, which sent the GOAWAY, names itself in no DRAIN.
	let sent = l.routes().sent.count(Withdrawal::Drain);
	let observer = l.shutdown.clone();
	l.stop().await;
	// The links it held to the deadline by design are no session it failed to drain.
	assert_eq!(observer.drained().await.forced, 0, "the held links counted as forced");
	assert_eq!(sent, 0, "the draining relay sent a DRAIN");
	assert!(started.elapsed() >= DRAIN, "L left before its drain deadline");

	c.stop().await;
	a.stop().await;
}

//! A draining relay drains the cluster links it dialed, as it does those it accepted:
//! each peer hears its GOAWAY and tells the others the relay is draining, well before
//! any link drops.
//!
//! Three real relays on loopback: C dials A, and L dials both. L drains, so A and C
//! each hear L's GOAWAY themselves and send the other a DRAIN for it, while L, which
//! sent the GOAWAY, sends none. Every link stays up until L's drain deadline, so no
//! relay counts a DEAD until then.

use std::time::Duration;

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
				let mut grant = moq_auth::Grant::new(moq_auth::Pattern::all(), moq_auth::Pattern::all());
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
		let ready = relay.ready();
		let running = tokio::spawn(relay.run());
		ready.wait().await.expect("relay ready");
		Self {
			port,
			stats,
			origin,
			trigger,
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
	moq_tokio::crypto::install_default().unwrap();

	let a = Node::start("a", &[], Duration::ZERO).await;
	let c = Node::start("c", &[a.port], Duration::ZERO).await;
	let l = Node::start("l", &[a.port, c.port], DRAIN).await;
	let _route = l
		.origin
		.route("l", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");

	// The triangle is up: A accepted C and L, C dialed A and accepted L, and L dialed both.
	a.learns("l").await;
	c.learns("l").await;
	kio::clock::timeout(TIMEOUT, async {
		while a.active() < 2 || c.active() < 2 || l.active() < 2 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.unwrap_or_else(|_| panic!("every link connects: A {}, C {}, L {}", a.active(), c.active(), l.active()));

	let started = tokio::time::Instant::now();
	l.trigger.start();

	// Each neighbour hears the GOAWAY on the link L dialed and tells the other.
	let drain = |node: &Node| {
		let routes = node.routes();
		routes.sent.count(Withdrawal::Drain) > 0 && routes.received.count(Withdrawal::Drain) > 0
	};
	kio::clock::timeout(TIMEOUT, async {
		while !(drain(&a) && drain(&c)) {
			tokio::task::yield_now().await;
		}
	})
	.await
	.unwrap_or_else(|_| panic!("each neighbour told the other that L drains: A {:?}, C {:?}, L {:?}", a.routes(), c.routes(), l.routes()));

	// ...before any link dropped: L holds them open to its deadline.
	assert!(started.elapsed() < DRAIN, "the DRAINs took {:?}", started.elapsed());
	for (name, node) in [("A", &a), ("C", &c)] {
		let routes = node.routes();
		assert_eq!(routes.sent.count(Withdrawal::Dead), 0, "{name} sent a DEAD before L left");
		assert_eq!(
			routes.received.count(Withdrawal::Dead),
			0,
			"{name} received a DEAD before L left"
		);
	}
	assert!(!l.running.is_finished(), "L exited before its drain deadline");

	// The draining relay, which sent the GOAWAY, names itself in no DRAIN.
	let sent = l.routes().sent.count(Withdrawal::Drain);
	l.stop().await;
	assert_eq!(sent, 0, "the draining relay sent a DRAIN");
	assert!(started.elapsed() >= DRAIN, "L left before its drain deadline");

	c.stop().await;
	a.stop().await;
}

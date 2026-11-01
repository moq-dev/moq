//! A draining relay's cluster peers stay on the session it drains instead of
//! redialing it, and reconnect once its replacement is up.
//!
//! Three relays over real loopback TCP: B drains while A and C peer with it. The
//! runtime runs on a manual clock, so the drain window and every backoff pass
//! only when the test advances them, while the sockets carry traffic in real time.
//! Each peer dials B through its own forwarder, which counts the sessions opened.

use std::{
	net::SocketAddr,
	sync::{Arc, Mutex},
	time::Duration,
};

use moq_relay::{
	Relay,
	cluster::{self, Peer},
};

/// How long B drains before it force-closes what is left.
const DRAIN: Duration = Duration::from_secs(30);

/// How far the manual clock moves per step: no longer than the shortest first
/// backoff, so a redial waiting on one gets its chance to land between steps.
const STEP: Duration = Duration::from_millis(500);

/// Wall time a step gives loopback traffic to land, since a redial that a timer
/// released has to cross real sockets before the forwarder can count it.
const SETTLE: Duration = Duration::from_millis(10);

/// Wall-clock bound on anything the test waits for.
const WAIT: Duration = Duration::from_secs(10);

#[expect(
	clippy::disallowed_methods,
	reason = "the watchdog runs on tokio's real clock, beside the installed manual clock"
)]
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	tokio::time::timeout(WAIT, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// Run on a 32 MiB thread, as `goaway_cluster.rs` does: the multi-transport
/// client and session types make the test future large.
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

/// A TCP forwarder standing in for one peer's route to B: it records the
/// upstream each accepted connection reached, and refuses while there is none.
#[derive(Clone)]
struct Forwarder {
	port: u16,
	target: Arc<Mutex<Option<SocketAddr>>>,
	reached: Arc<Mutex<Vec<SocketAddr>>>,
}

impl Forwarder {
	async fn spawn() -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind forwarder");
		let forwarder = Self {
			port: listener.local_addr().expect("local addr").port(),
			target: Default::default(),
			reached: Default::default(),
		};
		let this = forwarder.clone();
		tokio::spawn(async move {
			while let Ok((mut inbound, _)) = listener.accept().await {
				let Some(target) = *this.target.lock().unwrap() else {
					continue;
				};
				let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else {
					continue;
				};
				this.reached.lock().unwrap().push(target);
				tokio::spawn(async move {
					let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
				});
			}
		});
		forwarder
	}

	fn point(&self, target: Option<SocketAddr>) {
		*self.target.lock().unwrap() = target;
	}

	/// How many connections reached `target`.
	fn sessions(&self, target: SocketAddr) -> usize {
		self.reached.lock().unwrap().iter().filter(|&&t| t == target).count()
	}
}

/// A relay with a plaintext TCP listener and an open auth policy, publishing `name`.
async fn relay(name: &str) -> (Relay, SocketAddr, moq_net::origin::Publication) {
	let mut config = moq_relay::Config::default();
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.drain_timeout = Some(DRAIN.into());
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let addr = relay.tcp_addr().expect("tcp listener");
	let broadcast = relay.cluster().origin.create_broadcast(name).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	(relay, addr, broadcast)
}

/// A peer relay publishing `name` and dialing B through `forwarder`.
async fn peer(name: &str, forwarder: &Forwarder) -> (cluster::Cluster, moq_net::origin::Publication) {
	let mut config = cluster::Config::default();
	config.connect = vec![Peer::new(format!("tcp://127.0.0.1:{}/", forwarder.port))];
	let cluster = cluster::Cluster::new(cluster::Options::new(config))
		.expect("cluster init")
		.with_connect(Default::default(), Default::default());
	let broadcast = cluster.origin.create_broadcast(name).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let started = cluster.clone().start().await.expect("cluster start");
	tokio::spawn(started.run());
	(cluster, broadcast)
}

/// Advance the manual clock by `total`, one [`STEP`] at a time.
async fn pass(clock: &kio::clock::Installed, total: Duration) {
	let mut passed = Duration::ZERO;
	while passed < total {
		clock.advance(STEP);
		passed += STEP;
		#[expect(
			clippy::disallowed_methods,
			reason = "real sockets settle on tokio's clock, beside the installed manual clock"
		)]
		tokio::time::sleep(SETTLE).await;
	}
}

#[test]
fn draining_relay_keeps_its_peers_until_it_restarts() {
	run_cluster_test(draining_relay_keeps_its_peers_until_it_restarts_inner());
}

async fn draining_relay_keeps_its_peers_until_it_restarts_inner() {
	let clock = kio::clock::Manual::new().install(&tokio::runtime::Handle::current());

	let (b, b_addr, _b_broadcast) = relay("b").await;
	let b_origin = b.cluster().origin.clone();
	let trigger = b.shutdown_trigger().clone();
	let b_run = tokio::spawn(b.run());

	let forward_a = Forwarder::spawn().await;
	let forward_c = Forwarder::spawn().await;
	forward_a.point(Some(b_addr));
	forward_c.point(Some(b_addr));
	let (a, _a_broadcast) = peer("a", &forward_a).await;
	let (c, _c_broadcast) = peer("c", &forward_c).await;

	// The mesh converges: each side learns the other's broadcasts.
	within("A learns b", a.origin.consume().follow("b", None).unwrap().available())
		.await
		.expect("resolves");
	within("C learns b", c.origin.consume().follow("b", None).unwrap().available())
		.await
		.expect("resolves");
	within("B learns a", b_origin.consume().follow("a", None).unwrap().available())
		.await
		.expect("resolves");
	within("B learns c", b_origin.consume().follow("c", None).unwrap().available())
		.await
		.expect("resolves");
	assert_eq!(forward_a.sessions(b_addr), 1);
	assert_eq!(forward_c.sessions(b_addr), 1);

	// B drains for its whole window: its peers hold their sessions to the end.
	trigger.start();
	pass(&clock, DRAIN).await;
	within("B exits after its drain", b_run)
		.await
		.expect("relay task")
		.expect("relay run");
	assert_eq!(forward_a.sessions(b_addr), 1, "A redialed the draining relay");
	assert_eq!(forward_c.sessions(b_addr), 1, "C redialed the draining relay");

	// Nothing answers while B is down. Then its replacement comes up.
	forward_a.point(None);
	forward_c.point(None);
	pass(&clock, Duration::from_secs(5)).await;
	let (b2, b2_addr, _b2_broadcast) = relay("b").await;
	let b2_origin = b2.cluster().origin.clone();
	let _b2_run = tokio::spawn(b2.run());
	forward_a.point(Some(b2_addr));
	forward_c.point(Some(b2_addr));

	// Each peer reconnects within the capped backoff.
	let mut waited = Duration::ZERO;
	while forward_a.sessions(b2_addr) == 0 || forward_c.sessions(b2_addr) == 0 {
		assert!(
			waited < Duration::from_secs(120),
			"a peer never reconnected to the replacement"
		);
		pass(&clock, STEP).await;
		waited += STEP;
	}
	within(
		"B' learns a",
		b2_origin.consume().follow("a", None).unwrap().available(),
	)
	.await
	.expect("resolves");
	within(
		"B' learns c",
		b2_origin.consume().follow("c", None).unwrap().available(),
	)
	.await
	.expect("resolves");

	// And reconnects exactly once.
	pass(&clock, Duration::from_secs(30)).await;
	assert_eq!(
		forward_a.sessions(b2_addr),
		1,
		"A dialed the replacement more than once"
	);
	assert_eq!(
		forward_c.sessions(b2_addr),
		1,
		"C dialed the replacement more than once"
	);
}

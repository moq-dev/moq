//! Test harness that produces a connected `(client, server)` pair of
//! [`moq_net::Session`]s over the in-memory mock transport.
//!
//! The harness runs the full MoQ handshake (Client::connect + Server::accept)
//! over a [`super::mock::MockSession`] pair and spawns both protocol drivers, giving tests
//! two live sessions ready for pub/sub without any real QUIC or network I/O.

#![allow(dead_code, reason = "shared by several test binaries, each using only part of it")]

use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};
use std::task::Poll;

use moq_net::{Client, Server, Session, Version, kio, origin};

use super::mock::{MockSession, MockStats, create_mock_session_pair};

/// The current ambient instant.
pub fn now() -> moq_net::time::Instant {
	kio::clock::now()
}

/// Run a protocol driver in the background: on `kio::clock`'s sim in tests, on tokio in
/// the benches.
pub fn spawn<D: moq_net::time::Driver + Send + Unpin + 'static>(mut driver: D) {
	match kio::clock::sim::is_running() {
		true => drop(kio::clock::sim::spawn(kio::clock::drive(move |now, waiter| {
			moq_net::time::Driver::poll(&mut driver, now, waiter)
		}))),
		false => drop(tokio::spawn(moq_net::time::run(driver))),
	}
}

struct Controlled<D> {
	driver: D,
	polls: Option<Arc<AtomicUsize>>,
	gate: Option<kio::Consumer<bool>>,
}

impl<D: moq_net::time::Driver> moq_net::time::Driver for Controlled<D> {
	fn poll(
		&mut self,
		now: moq_net::time::Instant,
		waiter: &kio::Waiter,
	) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
		if let Some(gate) = &self.gate
			&& gate
				.poll(waiter, |open| if **open { Poll::Ready(()) } else { Poll::Pending })
				.is_pending()
		{
			return Ok(None);
		}
		if let Some(polls) = &self.polls {
			polls.fetch_add(1, Ordering::Relaxed);
		}
		self.driver.poll(now, waiter)
	}
}

/// Wait for a subscription to reach `track`. Demand alone does not say one has: on
/// moq-lite 05 and later a TRACK stream is demand before its SUBSCRIBE arrives.
pub async fn subscribed(track: &moq_net::track::Producer) {
	let mut track = track.clone();
	while track.subscription_changed().await.expect("track open").is_none() {}
}

/// Options for [`connect_mock`].
pub struct MockConnectOptions {
	/// The MoQ version to negotiate (determines the ALPN protocol string).
	pub version: Version,
	/// In-memory stream flow-control settings; ALPN comes from `version`.
	pub transport: moq_net::transport::memory::Config,
	/// Origin whose broadcasts the client publishes to the server.
	pub client_publish: Option<origin::Consumer>,
	/// Origin the client inserts remote broadcasts into.
	pub client_subscribe: Option<origin::Producer>,
	/// Origin whose broadcasts the server publishes to the client.
	pub server_publish: Option<origin::Consumer>,
	/// Origin the server inserts remote broadcasts into.
	pub server_subscribe: Option<origin::Producer>,
	/// One-way delay for stream data in each direction.
	pub latency: std::time::Duration,
	/// What the client may make the server's session hold, when not the default.
	pub server_limits: Option<moq_net::session::Limits>,
	/// The extensions the client offers.
	pub client_extensions: moq_net::setup::Extensions,
	/// The extensions the server offers.
	pub server_extensions: moq_net::setup::Extensions,
	/// The price the client puts on the link, when not the default.
	pub cost: Option<u64>,
	/// What both transports report as their connection statistics.
	pub stats: MockStats,
	/// The context the server records under.
	pub server_stats: moq_net::stats::Session,
	/// Count server driver polls into this counter when supplied.
	pub server_polls: Option<Arc<AtomicUsize>>,
	/// Hold an announcement stream over the client's whole subscribe origin for
	/// the pair's lifetime, so the client asks the server about everything it may,
	/// the way a reader listing everything would. Clear it to test discovery
	/// following the readers.
	pub client_discovers_all: bool,
	/// Pause the receiving driver while its transport keeps delivering data.
	pub server_gate: Option<kio::Consumer<bool>>,
	/// What the client advertises as its implementation, when not the default.
	pub client_implementation: Option<String>,
	/// What the server advertises as its implementation, when not the default.
	pub server_implementation: Option<String>,
}

impl MockConnectOptions {
	/// Create options for the given version with no origins attached.
	pub fn new(version: Version) -> Self {
		Self {
			version,
			transport: Default::default(),
			client_publish: None,
			client_subscribe: None,
			server_publish: None,
			server_subscribe: None,
			latency: std::time::Duration::ZERO,
			server_limits: None,
			client_extensions: Default::default(),
			server_extensions: Default::default(),
			cost: None,
			stats: MockStats::default(),
			server_stats: Default::default(),
			server_polls: None,
			client_discovers_all: true,
			server_gate: None,
			client_implementation: None,
			server_implementation: None,
		}
	}
}

/// A connected mock pair. Both protocol drivers run on spawned tasks for the
/// lifetime of their session.
pub struct MockPair {
	pub client: Session,
	pub server: Session,
	/// The client's end of the mock transport, for steering delivery.
	pub client_transport: MockSession,
	/// The server's end of the mock transport, for steering delivery.
	pub server_transport: MockSession,
	/// See [`MockConnectOptions::client_discovers_all`].
	pub client_discovery: Option<moq_net::announce::Consumer>,
}

/// Run the MoQ handshake over the mock transport, returning connected sessions.
///
/// Both sides negotiate the version via ALPN (the mock reports the protocol
/// string matching the requested version), mirroring a real QUIC transport
/// where ALPN selects the wire format before the connection starts.
///
/// # Panics
///
/// Panics if the handshake fails on either side (test-only code).
pub async fn connect_mock(opts: MockConnectOptions) -> MockPair {
	let protocol = opts.version.alpn();
	let mut transport = opts.transport;
	transport.protocol = Some(protocol.to_string());
	let (client_transport, server_transport) = create_mock_session_pair(transport);
	client_transport.set_latency(opts.latency);
	client_transport.set_stats(opts.stats);
	server_transport.set_stats(opts.stats);
	let transports = (client_transport.clone(), server_transport.clone());

	let mut client = Client::new()
		.with_versions(opts.version.into())
		.with_extensions(opts.client_extensions);
	if let Some(cost) = opts.cost {
		client = client.with_cost(cost);
	}
	if let Some(implementation) = opts.client_implementation {
		client = client.with_implementation(implementation);
	}
	if let Some(publish) = &opts.client_publish {
		client = client.with_publisher(publish);
	}
	let client_discovery = opts
		.client_subscribe
		.as_ref()
		.filter(|_| opts.client_discovers_all)
		.map(|subscribe| subscribe.consume().announced());
	if let Some(subscribe) = opts.client_subscribe {
		client = client.with_subscriber(subscribe);
	}

	let mut server = Server::new()
		.with_stats(opts.server_stats)
		.with_versions(opts.version.into())
		.with_extensions(opts.server_extensions);
	if let Some(publish) = &opts.server_publish {
		server = server.with_publisher(publish);
	}
	if let Some(subscribe) = opts.server_subscribe {
		server = server.with_subscriber(subscribe);
	}
	if let Some(limits) = opts.server_limits {
		server = server.with_limits(limits);
	}
	if let Some(implementation) = opts.server_implementation {
		server = server.with_implementation(implementation);
	}

	// Run both handshakes concurrently and spawn each side's driver
	// the moment its handshake resolves: on moq-transport the server's accept blocks
	// on the client's SETUP, which only reaches the wire once the client's
	// machine is polled (and vice versa for the server's own SETUP).
	let client_fut = async {
		let (session, driver) = client
			.connect(now(), client_transport)
			.await
			.expect("client handshake failed");
		spawn(driver);
		session
	};
	let server_fut = async {
		let (session, driver) = server
			.accept(now(), server_transport)
			.await
			.expect("server handshake failed");
		spawn(Controlled {
			driver,
			polls: opts.server_polls,
			gate: opts.server_gate,
		});
		session
	};
	let (client_session, server_session) = futures::join!(client_fut, server_fut);

	MockPair {
		client: client_session,
		server: server_session,
		client_transport: transports.0,
		server_transport: transports.1,
		client_discovery,
	}
}

/// Peer two relays the way `moq-relay`'s cluster does: one session, both directions.
pub async fn peer(version: Version, a: &origin::Producer, b: &origin::Producer) -> MockPair {
	peer_with_latency(version, a, b, std::time::Duration::ZERO).await
}

/// [`peer`] over a link with a one-way `latency`.
pub async fn peer_with_latency(
	version: Version,
	a: &origin::Producer,
	b: &origin::Producer,
	latency: std::time::Duration,
) -> MockPair {
	let a = a.clone().with_link(moq_net::origin::Link::Peer);
	let b = b.clone().with_link(moq_net::origin::Link::Peer);
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(a.consume().with_hidden(true));
	options.client_subscribe = Some(a);
	options.server_publish = Some(b.consume().with_hidden(true));
	options.server_subscribe = Some(b);
	options.latency = latency;
	connect_mock(options).await
}

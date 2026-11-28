//! The model benchmark's scenarios through the relay's admission, cluster, and stats.
//!
//! Run with `cargo bench -p moq-relay --features test-support --bench session`.

#[path = "../../moq-net/benches/session/mod.rs"]
mod session;

use std::fmt;
use std::sync::Arc;

use futures::FutureExt;
use moq_auth::{Grant, Prefix, lease};
use moq_net::{cache, origin};
use moq_relay::{Accepted, Connection, auth, cluster, stats};
use session::{Backend, MockConnectOptions, Pair, mock::create_mock_session_pair};
use swole::bench;

/// Sessions each opening and closing against `readers` actively waiting tally readers,
/// named `sessions=<sessions>/<readers>`.
#[derive(Clone, Copy)]
struct Tally {
	sessions: usize,
	readers: usize,
}

impl fmt::Display for Tally {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "sessions={}/{}", self.sessions, self.readers)
	}
}

fn tallies() -> Vec<Tally> {
	[1, 16, 256]
		.into_iter()
		.flat_map(|sessions| [1, 16, 256].map(|readers| Tally { sessions, readers }))
		.collect()
}

struct Readers {
	sessions: usize,
	observer: moq_relay::shutdown::Observer,
	readers: Vec<moq_relay::shutdown::Observer>,
}

fn readers(tally: Tally) -> Readers {
	let observer = moq_relay::shutdown::Observer::disabled();
	Readers {
		sessions: tally.sessions,
		readers: (0..tally.readers).map(|_| observer.clone()).collect(),
		observer,
	}
}

/// Session lifecycle updates against independent, actively waiting tally readers.
#[bench(args = tallies(), setup = readers)]
fn relay_tally(readers: &mut Readers) {
	let Readers {
		sessions,
		observer,
		readers,
	} = readers;
	for _ in 0..*sessions {
		let mut changes: Vec<_> = readers.iter_mut().map(|reader| Box::pin(reader.changed())).collect();
		for change in &mut changes {
			assert!(change.as_mut().now_or_never().is_none());
		}
		let serving = observer.serve();
		for change in &mut changes {
			assert_eq!(change.as_mut().now_or_never().unwrap().open, 1);
		}
		drop(changes);
		let mut changes: Vec<_> = readers.iter_mut().map(|reader| Box::pin(reader.changed())).collect();
		for change in &mut changes {
			assert!(change.as_mut().now_or_never().is_none());
		}
		drop(serving);
		for change in &mut changes {
			assert_eq!(change.as_mut().now_or_never().unwrap(), Default::default());
		}
	}
}

struct Relay;

#[derive(Clone)]
struct Node(Arc<State>);

struct State {
	cluster: cluster::Cluster,
	auth: auth::Auth,
	/// The cluster's driver, which would otherwise run on until its stats publisher next ticks.
	driver: tokio::task::JoinHandle<anyhow::Result<()>>,
	decider: tokio::task::JoinHandle<()>,
}

impl Drop for State {
	fn drop(&mut self) {
		self.driver.abort();
		self.decider.abort();
	}
}

impl Backend for Relay {
	type Relay = Node;

	fn relay(id: u64, capacity: Option<u64>) -> (Node, cache::Pool) {
		let mut config = moq_relay::cache::Config::default();
		// Only the swept capacity bounds the pool, not the machine's free memory.
		config.headroom = Some("0".to_string());
		if std::env::var_os("SESSION_EXPIRY_ONLY").is_none() {
			config.capacity = capacity.map(|capacity| capacity.to_string());
		}
		let cache = config.init().unwrap();
		let pool = cache.pool.clone();
		let mut options = cluster::Options::default();
		// The shared sweeps include more announcements than a default ordinary client.
		options.client.announces = options.peer.announces;
		let mut stats = stats::Config::default();
		stats.enabled = true;
		stats.node = Some(id.to_string());
		let (cluster, driver) = cluster::Cluster::new(options.with_cache(cache).with_stats(stats)).unwrap();
		let driver = tokio::spawn(driver.run());

		let (auth, mut admissions) = auth::Auth::embedded(format!("bench-{id}"));
		let decider = tokio::spawn(async move {
			while let Some(admission) = admissions.next().await {
				let mut grant = Grant::new(Prefix::all(), Prefix::all());
				grant.root = Some(String::new());
				grant.link = (admission.request.path == "/peer").then_some(moq_auth::Link::Peer);
				admission.grant(lease::Consumer::fixed(grant));
			}
		});
		let state = State {
			cluster,
			auth,
			driver,
			decider,
		};
		(Node(Arc::new(state)), pool)
	}

	fn origin(relay: &Node) -> &origin::Producer {
		&relay.0.cluster.origin
	}

	async fn connect(options: MockConnectOptions, relay: &Node, hidden: bool) -> Pair {
		let mut transport = options.transport;
		transport.protocol = Some(options.version.alpn().to_string());
		let (client_link, server_link) = create_mock_session_pair(transport);
		client_link.set_stats(options.stats);
		server_link.set_stats(options.stats);
		let server_transport = server_link.clone();
		let (cluster, auth) = (relay.0.cluster.clone(), relay.0.auth.clone());
		let version = options.version;
		let extensions = options.server_extensions;
		let task = tokio::spawn(async move {
			let server = moq_net::Server::new()
				.with_versions(version.into())
				.with_solicit(extensions.solicit)
				.with_ietf(extensions.ietf);
			let handshake = server
				.handshake(moq_net::time::Instant::now(), server_link)
				.await
				.unwrap();
			Connection::new(Accepted::new(handshake, moq_auth::Transport::Quic), cluster, auth)
				.run()
				.await;
		});

		let peer = options.client_publish.is_some() && options.client_subscribe.is_some();
		let discovery = options
			.client_subscribe
			.as_ref()
			.filter(|_| options.client_discovers_all)
			.map(|origin| origin.consume().routes());
		let mut client = moq_net::Client::new()
			.with_versions(version.into())
			.with_solicit(options.client_extensions.solicit)
			.with_ietf(options.client_extensions.ietf);
		// Older protocols need the peer exemption to discover dot paths.
		if peer || hidden {
			client = client.with_url("moql://localhost/peer".parse().unwrap());
		}
		if let Some(publish) = options.client_publish {
			client = client.with_publisher(publish);
		}
		if let Some(subscribe) = options.client_subscribe {
			client = client.with_subscriber(subscribe);
		}
		let (client, driver) = client
			.connect(moq_net::time::Instant::now(), client_link)
			.await
			.unwrap();
		tokio::spawn(moq_net::time::run(driver));
		Pair {
			client,
			server_transport,
			server: None,
			discovery,
			task: Some(task),
		}
	}
}

session::benches!(Relay);

fn main() {
	swole::main();
}

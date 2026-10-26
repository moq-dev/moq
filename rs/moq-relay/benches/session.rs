//! The model benchmark's scenarios through the relay's admission, cluster, and stats.

#[path = "../../moq-net/benches/session/mod.rs"]
mod session;

use std::sync::Arc;

use criterion::{criterion_group, criterion_main};
use moq_auth::{Grant, Pattern, lease};
use moq_net::{cache, origin};
use moq_relay::{Accepted, Connection, auth, cluster, stats};
use session::{Backend, MockConnectOptions, Pair, mock::create_mock_session_pair};

struct Relay;

#[derive(Clone)]
struct Node(Arc<State>);

struct State {
	cluster: cluster::Cluster,
	auth: auth::Auth,
	decider: tokio::task::JoinHandle<()>,
}

impl Drop for State {
	fn drop(&mut self) {
		self.decider.abort();
	}
}

impl Backend for Relay {
	type Relay = Node;
	const PREFIX: &'static str = "relay_";

	fn relay(id: u64, capacity: u64) -> (Node, cache::Pool) {
		let mut config = moq_relay::cache::Config::default();
		if std::env::var_os("SESSION_EXPIRY_ONLY").is_none() {
			config.capacity = Some(capacity.to_string());
		}
		let cache = config.init().unwrap();
		let pool = cache.pool.clone();
		let mut options = cluster::Options::default();
		options.config.id = Some(id);
		// The shared sweeps include more announcements than a default ordinary client.
		options.session.announces = options.peer.announces;
		let cluster = cluster::Cluster::new(options.with_cache(cache)).unwrap();
		let mut stats = stats::Config::default();
		stats.enabled = true;
		stats.node = Some(id.to_string());
		let producer = stats.build(cluster.origin.clone()).unwrap();
		let cluster = cluster.with_stats(producer);

		let (auth, mut admissions) = auth::Auth::embedded(format!("bench-{id}"));
		let decider = tokio::spawn(async move {
			while let Some(admission) = admissions.next().await {
				let mut grant = Grant::new(Pattern::all(), Pattern::all());
				grant.root = Some(String::new());
				grant.peer = admission.request.path == "/peer";
				admission.grant(lease::Consumer::fixed(grant));
			}
		});
		(Node(Arc::new(State { cluster, auth, decider })), pool)
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
				.with_extensions(extensions);
			let handshake = server
				.accept_request(moq_net::time::Instant::now(), server_link)
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
			.map(|origin| origin.consume().announced());
		let mut client = moq_net::Client::new()
			.with_versions(version.into())
			.with_extensions(options.client_extensions);
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

criterion_group!(
	benches,
	session::session::<Relay>,
	session::group_cost::<Relay>,
	session::fetch_frames::<Relay>,
	session::fetch_range::<Relay>,
	session::withdrawal::<Relay>
);
criterion_main!(benches);

//! A relay [`Connection`] over moq-net's in-memory transport, on kio's simulator.
//!
//! No sockets and no Tokio: every task runs on [`kio::clock::sim`], whose clock only
//! advances once every message in flight has landed, so a test never waits on the host.

#[cfg(feature = "test-support")]
use moq_net::transport::memory;
use std::time::Duration;

#[cfg(feature = "test-support")]
use futures::StreamExt;
#[cfg(feature = "test-support")]
use moq_relay::{Accepted, Connection, auth, cluster};

/// The version a test speaks unless it pins one: the newest the relay offers.
pub fn newest() -> moq_net::Version {
	moq_net::ALPNS[0].parse().expect("a known ALPN")
}

/// Dial a relay serving `cluster` through `auth` over a fresh in-memory link, as
/// `client` speaking `version`.
///
/// The relay's half runs as [`Connection::run`] on its own simulator task, reported
/// to the decider as QUIC. Lite 05+ answers SETUP before the verdict, so a refused session
/// may connect and then close.
#[cfg(feature = "test-support")]
pub async fn connect(
	client: moq_net::Client,
	version: moq_net::Version,
	cluster: &cluster::Cluster,
	auth: &auth::Auth,
) -> Result<moq_net::Session, moq_net::Error> {
	let mut config = memory::Config::default();
	config.protocol = Some(version.alpn().to_string());
	let (client_link, relay_link) = memory::pair(config);

	let (cluster, auth) = (cluster.clone(), auth.clone());
	drop(kio::clock::sim::spawn(async move {
		let server = moq_net::Server::new().with_versions(version.into());
		let Ok(handshake) = server.accept_request(moq_net::time::Instant::now(), relay_link).await else {
			return;
		};
		let request = Accepted::new(handshake, moq_auth::Transport::Quic);
		Connection::new(request, cluster, auth).run().await;
	}));

	let (session, driver) = client
		.with_versions(version.into())
		.connect(moq_net::time::Instant::now(), client_link)
		.await?;
	drop(kio::clock::sim::spawn(moq_net::time::run(driver)));
	Ok(session)
}

/// An `Auth` granting everything under `public`, `public_publish`, and
/// `public_subscribe` to anyone, as the `[auth]` config does.
#[cfg(feature = "test-support")]
pub fn public(public: &[&str], publish: &[&str], subscribe: &[&str]) -> auth::Auth {
	let patterns = |texts: &[&str]| texts.iter().map(|text| text.parse().expect("pattern")).collect();
	let mut config = auth::Config::default();
	config.public = patterns(public);
	config.public_publish = patterns(publish);
	config.public_subscribe = patterns(subscribe);
	config
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.expect("auth init")
}

/// A standalone cluster with default options, driven on the simulator.
#[cfg(feature = "test-support")]
pub fn cluster() -> cluster::Cluster {
	spawn_cluster(cluster::Options::default())
}

/// Build a cluster and drive it on a simulator task, as [`crate::spawn_cluster`]
/// does on Tokio. A failed driver panics the run.
#[cfg(feature = "test-support")]
pub fn spawn_cluster(options: cluster::Options) -> cluster::Cluster {
	let (cluster, driver) = cluster::Cluster::new(options).expect("cluster init");
	drop(kio::clock::sim::spawn(async move {
		if let Err(err) = driver.run().await {
			panic!("cluster driver failed: {err:#}");
		}
	}));
	cluster
}

/// An origin whose driver runs on a simulator task, as `moq_tokio::origin::spawn`
/// does on Tokio.
///
/// Its cache never reclaims idle groups, as in [`Net::options`].
#[cfg(feature = "test-support")]
pub fn origin() -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::default();
	config.pool = moq_net::cache::Pool::unbounded();
	let (origin, driver) = moq_net::origin::Producer::new(config);
	drop(kio::clock::sim::spawn(moq_net::time::run(driver)));
	origin
}

/// The one-way latency of every link a [`Net`] routes, so a broadcast reaches each
/// relay over its shortest path first, as on a network, rather than over every path
/// in the same instant.
#[cfg(feature = "test-support")]
pub const LATENCY: Duration = Duration::from_millis(10);

/// A network of named hosts on the simulator, reached at `https://<name>.sim/`.
///
/// Every cluster built with [`Net::options`] dials through it, and each dial lands on
/// the host its URL names over a fresh in-memory link speaking the dialer's preferred
/// version. A dial to a host that is not up is refused, as a closed port would be.
#[cfg(feature = "test-support")]
#[derive(Clone)]
pub struct Net {
	network: moq_tokio::sim::Network,
	hosts: std::sync::Arc<std::sync::Mutex<Hosts>>,
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct Hosts {
	/// Where each host's accepted links go, by name.
	up: std::collections::HashMap<String, futures::channel::mpsc::UnboundedSender<(memory::Session, moq_net::Version)>>,
	/// The dials each host accepted, by name, whether or not it is still up.
	accepted: std::collections::HashMap<String, usize>,
}

#[cfg(feature = "test-support")]
impl Net {
	/// An empty network, routing its dials on a simulator task.
	pub fn new() -> Self {
		let (network, mut dials) = moq_tokio::sim::Network::new();
		let hosts = std::sync::Arc::<std::sync::Mutex<Hosts>>::default();
		let routing = hosts.clone();
		drop(kio::clock::sim::spawn(async move {
			while let Some(dial) = dials.next().await {
				let name = dial.url().host_str().and_then(|host| host.strip_suffix(".sim"));
				let mut hosts = routing.lock().unwrap();
				let Some(name) = name.map(str::to_string) else { continue };
				let Some(host) = hosts.up.get(&name).cloned() else { continue };
				let version = *dial.versions().iter().next().expect("a dial offers a version");
				let mut config = memory::Config::default();
				config.protocol = Some(version.alpn().to_string());
				config.latency = LATENCY;
				if host.unbounded_send((dial.accept(config), version)).is_ok() {
					*hosts.accepted.entry(name).or_default() += 1;
				}
			}
		}));
		Self { network, hosts }
	}

	/// The URL a peer dials `name` at.
	pub fn url(name: &str) -> String {
		format!("https://{name}.sim/")
	}

	/// Cluster options dialing `peers` by name through this network.
	///
	/// The cache never reclaims idle groups: its sweep would keep a timer armed forever,
	/// so [`kio::clock::sim::idle`] could never see the mesh settle.
	pub fn options(&self, peers: &[&str]) -> cluster::Options {
		let mut config = cluster::Config::default();
		config.connect = peers.iter().map(|peer| cluster::Peer::new(Self::url(peer))).collect();
		let mut cache = moq_relay::cache::Config::default();
		// Past what the pool counts in milliseconds, so it never expires idle groups.
		cache.duration = Some(Duration::MAX.into());
		let mut options = cluster::Options::new(config).with_cache(cache.init().expect("cache"));
		options.network = Some(self.network.clone());
		options
	}

	/// Bring `name` up, handing each link it accepts to the returned stream.
	pub fn listen(
		&self,
		name: &str,
	) -> futures::channel::mpsc::UnboundedReceiver<(memory::Session, moq_net::Version)> {
		let (sender, accepted) = futures::channel::mpsc::unbounded();
		let previous = self.hosts.lock().unwrap().up.insert(name.to_string(), sender);
		assert!(previous.is_none(), "{name} is already up");
		accepted
	}

	/// Take `name` down: later dials to it are refused, and links it accepted stay up.
	pub fn down(&self, name: &str) {
		self.hosts.lock().unwrap().up.remove(name);
	}

	/// How many dials `name` has accepted, across every time it was up.
	pub fn accepted(&self, name: &str) -> usize {
		self.hosts.lock().unwrap().accepted.get(name).copied().unwrap_or_default()
	}

	/// Serve `name` as a relay: every link it accepts runs a relay [`Connection`] for
	/// `cluster` through `auth`, draining with `shutdown`.
	pub fn relay(&self, name: &str, cluster: &cluster::Cluster, auth: &auth::Auth, shutdown: &moq_relay::shutdown::Observer) {
		let mut accepted = self.listen(name);
		let (cluster, auth, shutdown) = (cluster.clone(), auth.clone(), shutdown.clone());
		drop(kio::clock::sim::spawn(async move {
			while let Some((link, version)) = accepted.next().await {
				let (cluster, auth, shutdown) = (cluster.clone(), auth.clone(), shutdown.clone());
				drop(kio::clock::sim::spawn(async move {
					let server = moq_net::Server::new().with_versions(version.into());
					let Ok(handshake) = server.accept_request(moq_net::time::Instant::now(), link).await else {
						return;
					};
					let request = Accepted::new(handshake, moq_auth::Transport::Quic);
					Connection::new(request, cluster, auth).with_shutdown(shutdown).run().await;
				}));
			}
		}));
	}
}

/// Start `cluster`'s peer dials on a simulator task, with the dial template the
/// relay defaults to, and return it.
#[cfg(feature = "test-support")]
pub async fn start(cluster: cluster::Cluster, connect: moq_tokio::connect::Config) -> cluster::Cluster {
	let started = cluster
		.clone()
		.with_connect(connect, Default::default())
		.start()
		.await
		.expect("cluster start");
	drop(kio::clock::sim::spawn(async move {
		if let Err(err) = started.run().await {
			panic!("cluster run failed: {err:#}");
		}
	}));
	cluster
}

/// Run `test` on the simulator, on a thread with room for the relay's large futures.
pub fn run<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(move || kio::clock::sim::run(test()))
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
}

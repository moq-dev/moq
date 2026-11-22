//! A network of relays on kio's simulator, for tests: named hosts over in-memory links,
//! with links that can be cut and held down.
//!
//! No traffic over sockets and no Tokio: every task runs on [`kio::clock::sim`], whose clock only
//! advances once every message in flight has landed, so a test never waits on the host.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use moq_net::transport::memory;

use crate::{Accepted, Connection, auth, cluster};

/// The one-way latency of every link a [`Net`] routes, so a broadcast reaches each
/// relay over its shortest path first, as on a network, rather than over every path
/// in the same instant.
pub const LATENCY: Duration = Duration::from_millis(10);

/// A network of named hosts on the simulator, each reached at its own port of
/// `https://sim/`, so a GOAWAY may redirect between them as between ports of one host.
///
/// Every dial names the host it comes from, so a link held down with
/// [`link_down`](Self::link_down) refuses dials both ways, as a partition would. Each dial
/// lands on the host its URL names over a fresh in-memory link speaking the dialer's
/// preferred version. A dial to a host that is not up is refused, as a closed port would be.
#[derive(Clone, Default)]
pub struct Net {
	hosts: Arc<Mutex<Hosts>>,
}

#[derive(Default)]
struct Hosts {
	/// Each host that is up, by name.
	up: HashMap<String, Host>,
	/// The dials each host accepted, by name, whether or not it is still up.
	accepted: HashMap<String, usize>,
	/// Each host's name, by the port it is reached at, less one.
	ports: Vec<String>,
	/// Pairs of hosts whose links are held down, each sorted.
	down: BTreeSet<(String, String)>,
	/// The links accepted and not yet closed, so a cut can close them.
	open: Vec<Open>,
}

/// Where a host's dials go.
#[derive(Clone)]
enum Host {
	/// Accepted, each link with the version it speaks.
	Accept(mpsc::UnboundedSender<(memory::Session, moq_net::Version)>),
	/// Held unanswered, so each dial hangs as against a peer that never completes a handshake.
	Hold(mpsc::UnboundedSender<moq_tokio::sim::Dial>),
}

/// One accepted link: who dialed whom, and what cuts it without holding it open.
struct Open {
	from: String,
	to: String,
	link: memory::Closer,
}

impl Hosts {
	/// The port `name` is reached at, assigned on first use.
	fn port(&mut self, name: &str) -> u16 {
		let index = match self.ports.iter().position(|port| port == name) {
			Some(index) => index,
			None => {
				self.ports.push(name.to_string());
				self.ports.len() - 1
			}
		};
		u16::try_from(index + 1).expect("fewer hosts than ports")
	}

	/// Route one dial from `from`, refusing it by dropping it.
	fn route(&mut self, from: &str, dial: moq_tokio::sim::Dial) {
		let port = dial.url().port_or_known_default().map(usize::from);
		let Some(to) = port.and_then(|port| self.ports.get(port.wrapping_sub(1))).cloned() else {
			return;
		};
		if self.down.contains(&pair(from, &to)) {
			return;
		}
		let accepted = match self.up.get(&to) {
			Some(Host::Accept(host)) => {
				let version = *dial.versions().iter().next().expect("a dial offers a version");
				let mut config = memory::Config::default();
				config.protocol = Some(version.alpn().to_string());
				config.latency = LATENCY;
				let link = dial.accept(config);
				let closer = link.closer();
				let accepted = host.unbounded_send((link, version)).is_ok();
				if accepted {
					// Forget the links that closed on their own, so the list stays as long as
					// what is open.
					self.open.retain(|open| !open.link.is_closed());
					self.open.push(Open {
						from: from.to_string(),
						to: to.clone(),
						link: closer,
					});
				}
				accepted
			}
			Some(Host::Hold(host)) => host.unbounded_send(dial).is_ok(),
			None => false,
		};
		if accepted {
			*self.accepted.entry(to).or_default() += 1;
		}
	}

	/// Close every open link `cut` selects.
	fn cut(&mut self, cut: impl Fn(&Open) -> bool) {
		self.open.retain(|open| {
			if cut(open) {
				open.link.close(0, "cut");
				false
			} else {
				true
			}
		});
	}
}

/// `a` and `b` in order, so a link is the same pair from either end.
fn pair(a: &str, b: &str) -> (String, String) {
	if a <= b {
		(a.to_string(), b.to_string())
	} else {
		(b.to_string(), a.to_string())
	}
}

impl Net {
	/// An empty network.
	pub fn new() -> Self {
		Self::default()
	}

	/// The URL a peer dials `name` at.
	pub fn url(&self, name: &str) -> String {
		format!("https://sim:{}/", self.hosts.lock().unwrap().port(name))
	}

	/// A network whose dials come from `from`, routed on a simulator task.
	pub fn network(&self, from: &str) -> moq_tokio::sim::Network {
		let (network, mut dials) = moq_tokio::sim::Network::new();
		let (hosts, from) = (self.hosts.clone(), from.to_string());
		drop(kio::clock::sim::spawn(async move {
			while let Some(dial) = dials.next().await {
				hosts.lock().unwrap().route(&from, dial);
			}
		}));
		network
	}

	/// Cluster options for the host `from`, dialing `peers` by name through this network.
	///
	/// The cache never reclaims idle groups: its sweep would keep a timer armed forever,
	/// so [`kio::clock::sim::idle`] could never see the mesh settle.
	pub fn options(&self, from: &str, peers: &[&str]) -> cluster::Options {
		let config = cluster::Config {
			connect: peers.iter().map(|peer| cluster::Peer::new(self.url(peer))).collect(),
			..Default::default()
		};
		let cache = crate::cache::Config {
			// Past what the pool counts in milliseconds, so it never expires idle groups.
			duration: Some(Duration::MAX.into()),
			// No governor either: its timer would keep the mesh from settling.
			headroom: Some("0".to_string()),
			..Default::default()
		};
		let mut options = cluster::Options::new(config).with_cache(cache.init().expect("cache"));
		options.network = Some(self.network(from));
		options
	}

	/// A client built from `config` that dials from `from` through this network.
	pub fn client(&self, from: &str, config: moq_tokio::connect::Config) -> moq_tokio::Client {
		config
			.init(Default::default())
			.expect("client init")
			.with_network(self.network(from))
	}

	/// Bring `name` up, handing each link it accepts to the returned stream.
	pub fn listen(&self, name: &str) -> mpsc::UnboundedReceiver<(memory::Session, moq_net::Version)> {
		let (sender, accepted) = mpsc::unbounded();
		self.up(name, Host::Accept(sender));
		accepted
	}

	/// Bring `name` up as a host that never answers: each dial to it hangs, held by the
	/// returned stream until dropped.
	pub fn black_hole(&self, name: &str) -> mpsc::UnboundedReceiver<moq_tokio::sim::Dial> {
		let (sender, held) = mpsc::unbounded();
		self.up(name, Host::Hold(sender));
		held
	}

	fn up(&self, name: &str, host: Host) {
		let previous = self.hosts.lock().unwrap().up.insert(name.to_string(), host);
		assert!(previous.is_none(), "{name} is already up");
	}

	/// Take `name` down: later dials to it are refused, and links it accepted stay up.
	pub fn down(&self, name: &str) {
		self.hosts.lock().unwrap().up.remove(name);
	}

	/// Take `name` down and cut every link to or from it, as a host that dies would.
	pub fn crash(&self, name: &str) {
		let mut hosts = self.hosts.lock().unwrap();
		hosts.up.remove(name);
		hosts.cut(|open| open.from == name || open.to == name);
	}

	/// Hold the link between `a` and `b` down: cut what is open, and refuse their dials
	/// both ways until [`link_up`](Self::link_up).
	pub fn link_down(&self, a: &str, b: &str) {
		let mut hosts = self.hosts.lock().unwrap();
		hosts.down.insert(pair(a, b));
		hosts.cut(|open| pair(&open.from, &open.to) == pair(a, b));
	}

	/// Let `a` and `b` dial each other again.
	pub fn link_up(&self, a: &str, b: &str) {
		self.hosts.lock().unwrap().down.remove(&pair(a, b));
	}

	/// How many dials `name` has accepted, across every time it was up.
	pub fn accepted(&self, name: &str) -> usize {
		self.hosts
			.lock()
			.unwrap()
			.accepted
			.get(name)
			.copied()
			.unwrap_or_default()
	}

	/// Serve `name` as a relay: every link it accepts runs a relay [`Connection`] for
	/// `cluster` through `auth`, draining with `shutdown`. The returned stream yields
	/// once per link accepted.
	pub fn relay(
		&self,
		name: &str,
		cluster: &cluster::Cluster,
		auth: &auth::Auth,
		shutdown: &crate::shutdown::Observer,
	) -> mpsc::UnboundedReceiver<()> {
		let mut accepted = self.listen(name);
		let (cluster, auth, shutdown) = (cluster.clone(), auth.clone(), shutdown.clone());
		let (notify, accepts) = mpsc::unbounded();
		drop(kio::clock::sim::spawn(async move {
			while let Some((link, version)) = accepted.next().await {
				let _ = notify.unbounded_send(());
				let (cluster, auth, shutdown) = (cluster.clone(), auth.clone(), shutdown.clone());
				drop(kio::clock::sim::spawn(async move {
					let server = moq_net::Server::new().with_versions(version.into());
					let Ok(handshake) = server.handshake(moq_net::time::Instant::now(), link).await else {
						return;
					};
					let request = Accepted::new(handshake, moq_auth::Transport::Quic);
					Connection::new(request, cluster, auth)
						.with_shutdown(shutdown)
						.run()
						.await;
				}));
			}
		}));
		accepts
	}

	/// Serve `name` as a bare server publishing `origin` to every peer it accepts, and
	/// hand each session to the returned stream. Pass a [`relay_origin`] when a test
	/// relays through it, so it forwards what it learned from its own upstream.
	pub fn upstream(
		&self,
		name: &str,
		origin: &moq_net::origin::Producer,
	) -> mpsc::UnboundedReceiver<moq_net::Session> {
		let mut accepted = self.listen(name);
		let origin = origin.clone();
		let (sessions, received) = mpsc::unbounded();
		drop(kio::clock::sim::spawn(async move {
			while let Some((link, version)) = accepted.next().await {
				// Serve the shared origin bidirectionally, like a relay peer would.
				let server = moq_net::Server::new()
					.with_versions(version.into())
					.with_publisher(origin.consume())
					.with_subscriber(self::origin());
				let Ok((session, driver)) = server.accept(moq_net::time::Instant::now(), link).await else {
					continue;
				};
				drop(kio::clock::sim::spawn(moq_net::time::run(driver)));
				let _ = sessions.unbounded_send(session);
			}
		}));
		received
	}
}

/// An origin whose driver runs on a simulator task, as `moq_tokio::origin::spawn`
/// does on Tokio.
///
/// Its cache never reclaims idle groups, as in [`Net::options`].
pub fn origin() -> moq_net::origin::Producer {
	spawn_origin(false)
}

/// An [`origin`] that forwards what it learned from one peer to the others, as a
/// relay's does.
pub fn relay_origin() -> moq_net::origin::Producer {
	spawn_origin(true)
}

fn spawn_origin(proxy: bool) -> moq_net::origin::Producer {
	let mut config = moq_net::origin::Config::default();
	config.pool = moq_net::cache::Pool::unbounded();
	config.proxy = proxy;
	let (origin, driver) = moq_net::origin::Producer::new(config);
	drop(kio::clock::sim::spawn(moq_net::time::run(driver)));
	origin
}

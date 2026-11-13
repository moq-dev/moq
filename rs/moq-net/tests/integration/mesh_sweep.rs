#![cfg(feature = "async")]

//! Seeded random lite-07 meshes, real sessions over the memory transport on the
//! simulated clock, checked against the route rules at every quiet point.
//!
//! Each case builds a random mesh of relays with clients publishing and watching
//! broadcasts, then applies one external change at a time: a link flap, a relay
//! restart, a publisher restarting under a fresh identity, a client joining or
//! leaving, or a broadcast published or withdrawn. Once the mesh goes quiet, every
//! relay and client must have followed the rules the `verify-route` skill states on
//! what it showed, and must now show exactly what it can reach. The announce
//! messages each change cost are counted against [`budget`].
//!
//! `MOQ_ROUTE_SEED` and `MOQ_ROUTE_CASES` pick the cases; a failure prints the seed
//! and case that replay it bit-exact. Nightly sweeps a fresh seed (`just test
//! route-sweep`), and `benchmark_route_mesh` reports the counts per mesh size and
//! change.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::Duration;

use futures::channel::mpsc;

use moq_net::{Instance, Timescale, Timestamp, Version, cache, origin, stats, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

use crate::support::{self, Rng};
use crate::support::AppendNext;

const VERSION: &str = "moq-lite-07-wip";

/// The only track each broadcast carries, holding one frame.
const TRACK: &str = "t";

/// What one case runs.
#[derive(Clone, Copy, Debug)]
struct Shape {
	/// Relays in the mesh.
	relays: usize,
	/// External changes applied, one per quiet point.
	changes: usize,
}

/// One external change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
	/// A link between two relays goes down.
	LinkDown,
	/// A link that went down comes back.
	LinkUp,
	/// A relay restarts under a fresh identity, and its links and clients reconnect.
	RelayRestart,
	/// A publishing client restarts under a fresh identity, attaches to a random relay,
	/// and publishes its broadcasts again as new instances.
	OriginRestart,
	/// A client joins at a random relay.
	Join,
	/// A client's session closes.
	Leave,
	/// A client publishes a new broadcast.
	Publish,
	/// A client withdraws one of its broadcasts.
	Withdraw,
}

impl Change {
	const ALL: [Self; 8] = [
		Self::LinkDown,
		Self::LinkUp,
		Self::RelayRestart,
		Self::OriginRestart,
		Self::Join,
		Self::Leave,
		Self::Publish,
		Self::Withdraw,
	];

	fn as_str(self) -> &'static str {
		match self {
			Self::LinkDown => "link_down",
			Self::LinkUp => "link_up",
			Self::RelayRestart => "relay_restart",
			Self::OriginRestart => "origin_restart",
			Self::Join => "join",
			Self::Leave => "leave",
			Self::Publish => "publish",
			Self::Withdraw => "withdraw",
		}
	}
}

/// A route event as a node's reader saw it.
#[derive(Clone, Debug)]
struct Seen {
	path: String,
	kind: origin::RouteKind,
	instance: Instance,
	available: bool,
}

/// Watch `routes` from its own task, so every intermediate event is recorded.
fn observe(mut routes: origin::Routes) -> mpsc::UnboundedReceiver<Seen> {
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		while let Some(event) = routes.next().await {
			let seen = Seen {
				path: event.announce.prefix.to_string(),
				kind: event.kind,
				instance: event.announce.route.instance().clone(),
				available: event.announce.route.available(),
			};
			if tx.unbounded_send(seen).is_err() {
				break;
			}
		}
	});
	rx
}

/// A broadcast as one node currently shows it.
#[derive(Clone, Debug)]
struct Shown {
	instance: Instance,
	available: bool,
}

/// What one node's reader has been shown, checked event by event.
struct Reader {
	events: mpsc::UnboundedReceiver<Seen>,
	shown: BTreeMap<String, Shown>,
	/// The newest generation shown per path, which must never go back.
	newest: BTreeMap<String, u64>,
	/// The instances this reader saw END for.
	ended: BTreeSet<Instance>,
}

impl Reader {
	fn new(origin: &origin::Producer) -> Self {
		Self {
			events: observe(origin.consume().routes()),
			shown: BTreeMap::new(),
			newest: BTreeMap::new(),
			ended: BTreeSet::new(),
		}
	}
}

/// One publication of a broadcast: a path served by one instance.
struct Publication {
	path: String,
	/// Counts the path's publications, so a newer instance compares higher.
	generation: u64,
	/// Whether it still serves; once over it never serves again.
	live: bool,
	/// The client publishing it.
	client: usize,
}

/// A client's own broadcast, held so it stays published.
struct Held {
	route: origin::Route,
	_track: track::Producer,
}

struct Relay {
	origin: origin::Producer,
	reader: Reader,
}

struct Client {
	relay: usize,
	origin: origin::Producer,
	pair: MockPair,
	reader: Reader,
	held: BTreeMap<String, Held>,
}

/// A link between two relays, up or down.
struct Link {
	ends: (usize, usize),
	latency: Duration,
	pair: Option<MockPair>,
}

/// Every external change's announce-message count, by kind.
type Counts = BTreeMap<Change, Vec<u64>>;

struct Mesh {
	rng: Rng,
	version: Version,
	registry: stats::Registry,
	next_hop: u64,
	relays: Vec<Relay>,
	links: Vec<Link>,
	clients: Vec<Option<Client>>,
	publications: BTreeMap<Instance, Publication>,
	/// Publications per path so far.
	generations: BTreeMap<String, u64>,
	/// Broadcasts published so far, so each gets a fresh path.
	published: u64,
	/// What each change has done, for the failure report.
	log: String,
}

impl Mesh {
	async fn new(rng: Rng, relays: usize) -> Self {
		let mut mesh = Self {
			rng,
			version: VERSION.parse().unwrap(),
			registry: stats::Registry::new(Default::default()),
			next_hop: 0,
			relays: Vec::new(),
			links: Vec::new(),
			clients: Vec::new(),
			publications: BTreeMap::new(),
			generations: BTreeMap::new(),
			published: 0,
			log: String::new(),
		};
		for _ in 0..relays {
			let origin = mesh.origin(true);
			let reader = Reader::new(&origin);
			mesh.relays.push(Relay { origin, reader });
		}
		// A random spanning tree, then up to as many extra links.
		let mut linked = BTreeSet::new();
		for b in 1..relays {
			let a = mesh.rng.pick(b);
			linked.insert((a, b));
		}
		for _ in 0..mesh.rng.range(0, relays as u64) {
			let a = mesh.rng.pick(relays);
			let b = mesh.rng.pick(relays);
			if a != b {
				linked.insert((a.min(b), a.max(b)));
			}
		}
		for ends in linked {
			let latency = Duration::from_millis(mesh.rng.range(1, 150));
			mesh.links.push(Link {
				ends,
				latency,
				pair: None,
			});
			let link = mesh.links.len() - 1;
			mesh.link_up(link).await;
		}
		for _ in 0..mesh.rng.range(1, relays as u64) {
			mesh.join().await;
		}
		mesh
	}

	/// A fresh origin under a new identity: a proxying relay, or a client.
	fn origin(&mut self, proxy: bool) -> origin::Producer {
		self.next_hop += 1;
		let mut config = origin::Config::new(std::num::NonZeroU64::new(self.next_hop).unwrap());
		// Nothing expires, so no expiry sweep keeps a timer armed and the mesh can go idle.
		config.pool = cache::Pool::unbounded();
		let (origin, driver) = origin::Producer::new(config);
		support::harness::spawn(driver);
		origin.with_proxy(proxy)
	}

	fn stats(&self) -> moq_net::stats::Session {
		self.registry
			.tier(stats::Tier::new(moq_net::Path::from_static("mesh")))
			.session(stats::SessionInfo::new(moq_net::Path::from_static("")))
	}

	fn size(&self) -> Size {
		let clients = self.live_clients().len() as u64;
		Size {
			sessions: self.links.iter().filter(|link| link.pair.is_some()).count() as u64 + clients,
			identities: self.relays.len() as u64 + clients,
			claims: self
				.publications
				.values()
				.filter(|publication| publication.live)
				.count() as u64,
		}
	}

	/// Announce messages sent so far over every session, both directions.
	fn announce_messages(&self) -> u64 {
		self.registry
			.snapshot()
			.control()
			.iter()
			.filter(|(_, class, _)| *class == stats::Message::Announce)
			.map(|(_, _, totals)| totals.tx_messages + totals.rx_messages)
			.sum()
	}

	async fn link_up(&mut self, link: usize) {
		let (a, b) = self.links[link].ends;
		let a = self.relays[a].origin.clone().with_link(origin::Link::Peer);
		let b = self.relays[b].origin.clone().with_link(origin::Link::Peer);
		let mut options = MockConnectOptions::new(self.version);
		options.client_publish = Some(a.consume().with_hidden(true));
		options.client_subscribe = Some(a);
		options.server_publish = Some(b.consume().with_hidden(true));
		options.server_subscribe = Some(b);
		options.latency = self.links[link].latency;
		options.server_stats = self.stats();
		self.links[link].pair = Some(connect_mock(options).await);
	}

	fn link_down(&mut self, link: usize) {
		if let Some(pair) = self.links[link].pair.take() {
			close(pair);
		}
	}

	async fn connect_client(&mut self, relay: usize, origin: &origin::Producer) -> MockPair {
		let mut options = MockConnectOptions::new(self.version);
		options.client_publish = Some(origin.consume());
		options.client_subscribe = Some(origin.clone());
		options.server_publish = Some(self.relays[relay].origin.consume());
		options.server_subscribe = Some(self.relays[relay].origin.clone());
		options.latency = Duration::from_millis(self.rng.range(1, 50));
		options.server_stats = self.stats();
		connect_mock(options).await
	}

	async fn join(&mut self) -> usize {
		let relay = self.rng.pick(self.relays.len());
		let origin = self.origin(false);
		let reader = Reader::new(&origin);
		let pair = self.connect_client(relay, &origin).await;
		self.clients.push(Some(Client {
			relay,
			origin,
			pair,
			reader,
			held: BTreeMap::new(),
		}));
		let client = self.clients.len() - 1;
		for _ in 0..self.rng.range(0, 2) {
			self.publish(client);
		}
		client
	}

	/// Publish a new broadcast from `client`.
	fn publish(&mut self, client: usize) {
		self.published += 1;
		let path = format!("room/{}", self.published);
		self.announce(client, path);
	}

	/// Announce `path` from `client` as a new instance, with one frame on its track.
	fn announce(&mut self, client: usize, path: String) {
		let held = {
			let client = self.clients[client].as_ref().unwrap();
			let route = client
				.origin
				.route(path.as_str(), origin::RouteOptions::hidden())
				.unwrap();
			let track = route
				.broadcast()
				.create_track(TRACK, track::Info::default().with_timescale(Timescale::MILLI))
				.unwrap();
			let mut group = track.append_next().unwrap();
			group
				.write_frame(Timestamp::ZERO, bytes::Bytes::from(path.clone()))
				.unwrap();
			group.close().unwrap();
			route.announce().unwrap();
			Held { route, _track: track }
		};
		let generation = self.generations.entry(path.clone()).or_default();
		*generation += 1;
		self.publications.insert(
			held.route.instance(),
			Publication {
				path: path.clone(),
				generation: *generation,
				live: true,
				client,
			},
		);
		self.clients[client].as_mut().unwrap().held.insert(path, held);
	}

	/// Mark every publication `held` serves as over.
	fn over(&mut self, held: &BTreeMap<String, Held>) {
		for held in held.values() {
			self.publications.get_mut(&held.route.instance()).unwrap().live = false;
		}
	}

	fn live_clients(&self) -> Vec<usize> {
		(0..self.clients.len()).filter(|&c| self.clients[c].is_some()).collect()
	}

	/// Apply one random external change, returning which, or `None` when the drawn
	/// kind has nothing to act on.
	async fn change(&mut self) -> Option<Change> {
		let change = Change::ALL[self.rng.pick(Change::ALL.len())];
		match change {
			Change::LinkDown | Change::LinkUp => {
				let up = change == Change::LinkUp;
				let links: Vec<usize> = (0..self.links.len())
					.filter(|&l| self.links[l].pair.is_none() == up)
					.collect();
				let link = *links.get(self.rng.pick(links.len().max(1)))?;
				let _ = writeln!(self.log, "{change:?} {:?}", self.links[link].ends);
				match up {
					true => self.link_up(link).await,
					false => self.link_down(link),
				}
			}
			Change::RelayRestart => {
				let relay = self.rng.pick(self.relays.len());
				let _ = writeln!(self.log, "{change:?} {relay}");
				let touching: Vec<usize> = (0..self.links.len())
					.filter(|&l| {
						let (a, b) = self.links[l].ends;
						(a == relay || b == relay) && self.links[l].pair.is_some()
					})
					.collect();
				for &link in &touching {
					self.link_down(link);
				}
				let attached: Vec<usize> = (self.live_clients().into_iter())
					.filter(|&c| self.clients[c].as_ref().unwrap().relay == relay)
					.collect();
				let mut held = Vec::new();
				for &client in &attached {
					let client = self.clients[client].take().unwrap();
					close(client.pair);
					held.push((client.origin, client.reader, client.held));
				}
				let origin = self.origin(true);
				let reader = Reader::new(&origin);
				self.relays[relay] = Relay { origin, reader };
				for link in touching {
					self.link_up(link).await;
				}
				for (client, (origin, reader, held)) in attached.into_iter().zip(held) {
					let pair = self.connect_client(relay, &origin).await;
					self.clients[client] = Some(Client {
						relay,
						origin,
						pair,
						reader,
						held,
					});
				}
			}
			Change::OriginRestart => {
				let publishers: Vec<usize> = (self.live_clients().into_iter())
					.filter(|&c| !self.clients[c].as_ref().unwrap().held.is_empty())
					.collect();
				let client = *publishers.get(self.rng.pick(publishers.len().max(1)))?;
				let old = self.clients[client].take().unwrap();
				close(old.pair);
				self.over(&old.held);
				let paths: Vec<String> = old.held.keys().cloned().collect();
				drop(old.held);
				drop(old.origin);
				let relay = self.rng.pick(self.relays.len());
				let _ = writeln!(self.log, "{change:?} client {client} to relay {relay}: {paths:?}");
				let origin = self.origin(false);
				let reader = Reader::new(&origin);
				let pair = self.connect_client(relay, &origin).await;
				self.clients[client] = Some(Client {
					relay,
					origin,
					pair,
					reader,
					held: BTreeMap::new(),
				});
				for path in paths {
					self.announce(client, path);
				}
			}
			Change::Join => {
				let client = self.join().await;
				let relay = self.clients[client].as_ref().unwrap().relay;
				let _ = writeln!(self.log, "{change:?} client {client} at relay {relay}");
			}
			Change::Leave => {
				let clients = self.live_clients();
				let client = *clients.get(self.rng.pick(clients.len().max(1)))?;
				let _ = writeln!(self.log, "{change:?} client {client}");
				let old = self.clients[client].take().unwrap();
				close(old.pair);
				self.over(&old.held);
			}
			Change::Publish => {
				let clients = self.live_clients();
				let client = *clients.get(self.rng.pick(clients.len().max(1)))?;
				self.publish(client);
				let _ = writeln!(self.log, "{change:?} client {client}: room/{}", self.published);
			}
			Change::Withdraw => {
				let publishers: Vec<usize> = (self.live_clients().into_iter())
					.filter(|&c| !self.clients[c].as_ref().unwrap().held.is_empty())
					.collect();
				let client = *publishers.get(self.rng.pick(publishers.len().max(1)))?;
				let held = &self.clients[client].as_ref().unwrap().held;
				let path = held.keys().nth(self.rng.pick(held.len())).unwrap().clone();
				let _ = writeln!(self.log, "{change:?} client {client}: {path}");
				let held = self.clients[client].as_mut().unwrap().held.remove(&path).unwrap();
				self.over(&BTreeMap::from([(path, held)]));
			}
		}
		Some(change)
	}

	/// The relays each relay reaches over links that are up.
	fn components(&self) -> Vec<usize> {
		let mut component: Vec<usize> = (0..self.relays.len()).collect();
		fn find(component: &mut [usize], x: usize) -> usize {
			let mut root = x;
			while component[root] != root {
				root = component[root];
			}
			component[x] = root;
			root
		}
		for link in self.links.iter().filter(|link| link.pair.is_some()) {
			let (a, b) = link.ends;
			let (a, b) = (find(&mut component, a), find(&mut component, b));
			component[a] = b;
		}
		(0..self.relays.len()).map(|r| find(&mut component, r)).collect()
	}

	/// Check every reader's events since the last quiet point, then that each shows
	/// exactly the live broadcasts it reaches.
	fn check(&mut self) -> Result<(), String> {
		let components = self.components();
		let mut failures = Vec::new();
		let publications = &self.publications;
		let attached: BTreeMap<usize, usize> = (self.clients.iter().enumerate())
			.filter_map(|(c, client)| Some((c, client.as_ref()?.relay)))
			.collect();
		let mut readers: Vec<(String, usize, &mut Reader)> = Vec::new();
		for (r, relay) in self.relays.iter_mut().enumerate() {
			readers.push((format!("relay {r}"), components[r], &mut relay.reader));
		}
		for (c, client) in self.clients.iter_mut().enumerate() {
			if let Some(client) = client {
				readers.push((format!("client {c}"), components[client.relay], &mut client.reader));
			}
		}

		for (name, component, reader) in readers {
			while let Ok(seen) = reader.events.try_recv() {
				if let Err(err) = apply(reader, publications, &seen) {
					failures.push(format!("{name}: {err}: {seen:?}"));
				}
			}
			// Settled: exactly the live broadcasts whose publisher this node reaches.
			let expected: BTreeMap<&str, &Instance> = (publications.iter())
				.filter(|(_, publication)| publication.live)
				.filter(|(_, publication)| {
					let relay = attached[&publication.client];
					component == components[relay]
				})
				.map(|(instance, publication)| (publication.path.as_str(), instance))
				.collect();
			let shown: BTreeMap<&str, &Instance> = (reader.shown.iter())
				.map(|(path, shown)| (path.as_str(), &shown.instance))
				.collect();
			if shown != expected {
				failures.push(format!("{name}: not settled: shows {shown:?}, reaches {expected:?}"));
			}
			for (path, shown) in &reader.shown {
				if !shown.available {
					failures.push(format!("{name}: {path} still shown unavailable"));
				}
			}
		}
		match failures.is_empty() {
			true => Ok(()),
			false => Err(failures.join("\n")),
		}
	}

	/// Read the frame on one shown broadcast from each client, through its relay.
	async fn read(&mut self) -> Result<(), String> {
		let mut failures = Vec::new();
		for c in self.live_clients() {
			let client = self.clients[c].as_ref().unwrap();
			let remote: Vec<(&String, &Shown)> = (client.reader.shown.iter())
				.filter(|(path, _)| !client.held.contains_key(*path))
				.collect();
			let Some(&(path, shown)) = remote.get(self.rng.pick(remote.len().max(1))) else {
				continue;
			};
			let read = async {
				let broadcast = (client.origin.consume())
					.request(path.as_str(), shown.instance.clone())
					.await
					.map_err(|err| format!("request: {err}"))?;
				let mut track = (broadcast.track(TRACK).map_err(|err| format!("track: {err}"))?)
					.subscribe(None)
					.await
					.map_err(|err| format!("subscribe: {err}"))?;
				let mut group = (track.recv_group().await)
					.map_err(|err| format!("group: {err}"))?
					.ok_or("no group")?;
				let frame = (group.read_frame().await)
					.map_err(|err| format!("frame: {err}"))?
					.ok_or("no frame")?;
				match frame.payload == path.as_bytes() {
					true => Ok(()),
					false => Err(format!("read {:?}", frame.payload)),
				}
			};
			match kio::clock::timeout(Duration::from_secs(30), read).await {
				Ok(Ok(())) => {}
				Ok(Err(err)) => failures.push(format!("client {c} reading {path}: {err}")),
				Err(_) => failures.push(format!("client {c} reading {path}: no frame in 30 s")),
			}
		}
		match failures.is_empty() {
			true => Ok(()),
			false => Err(failures.join("\n")),
		}
	}
}

/// Close both ends of a session, as a lost connection does.
fn close(pair: MockPair) {
	pair.client.abort(moq_net::Error::Cancel);
	pair.server.abort(moq_net::Error::Cancel);
}

/// Apply one event to what `reader` shows, checking the per-event rules.
fn apply(reader: &mut Reader, publications: &BTreeMap<Instance, Publication>, seen: &Seen) -> Result<(), String> {
	use origin::RouteKind::*;

	// Truthful: every instance shown is one some client published at that path.
	let publication = publications.get(&seen.instance).ok_or("an instance nobody published")?;
	if publication.path != seen.path {
		return Err(format!("the instance of {}", publication.path));
	}
	let current = reader.shown.get(&seen.path);
	match (seen.kind, current) {
		(Start, Some(_)) => return Err("START for a shown broadcast".into()),
		(Update | Restart | End | Offline, None) => return Err("an event for a broadcast not shown".into()),
		(Update | End | Offline, Some(shown)) if shown.instance != seen.instance => {
			return Err(format!("the event names another instance than {:?}", shown.instance));
		}
		(Restart, Some(shown)) if shown.instance == seen.instance => {
			return Err("RESTART to the same instance".into());
		}
		_ => {}
	}
	match seen.kind {
		Start | Update | Restart => {
			// NoRevival: an instance this reader saw end never starts again.
			if reader.ended.contains(&seen.instance) {
				return Err("revived after END".into());
			}
			// NoRegression: a reader never goes back to an older publication.
			let newest = reader.newest.entry(seen.path.clone()).or_default();
			if publication.generation < *newest {
				return Err(format!("generation {} after {newest}", publication.generation));
			}
			*newest = publication.generation;
			reader.shown.insert(
				seen.path.clone(),
				Shown {
					instance: seen.instance.clone(),
					available: seen.available,
				},
			);
		}
		End => {
			// NoFalseEnd: END only once the publication stopped.
			if publication.live {
				return Err("END while its publisher still serves it".into());
			}
			reader.ended.insert(seen.instance.clone());
			reader.shown.remove(&seen.path);
		}
		Offline => {
			reader.shown.remove(&seen.path);
		}
	}
	Ok(())
}

/// What a change's announce messages scale with: sessions up, route identities (each
/// relay and client origin), and live broadcasts.
#[derive(Clone, Copy, Debug)]
struct Size {
	sessions: u64,
	identities: u64,
	claims: u64,
}

/// The most announce messages one change may cost, from the mesh's size before and
/// after it: each session carries at most 4 messages each way per route identity and
/// per claim. Converging takes one advertisement of each route over each session;
/// path hunting or counting to infinity takes one per stale path or hop, which grows
/// past any such constant.
fn budget(before: Size, after: Size) -> u64 {
	let sessions = before.sessions.max(after.sessions);
	let identities = before.identities.max(after.identities);
	let claims = before.claims.max(after.claims);
	8 * sessions * (identities + claims)
}

/// What one case found: each change's announce-message count, or the failure.
async fn run_case(rng: Rng, shape: Shape) -> Result<Counts, String> {
	let mut mesh = Mesh::new(rng, shape.relays).await;
	kio::clock::sim::idle().await;
	mesh.check().map_err(|err| format!("after building the mesh:\n{err}"))?;
	let mut counts = Counts::new();
	for _ in 0..shape.changes {
		let before = (mesh.announce_messages(), mesh.size());
		let Some(change) = mesh.change().await else {
			continue;
		};
		kio::clock::sim::idle().await;
		let sent = mesh.announce_messages() - before.0;
		counts.entry(change).or_default().push(sent);
		let budget = budget(before.1, mesh.size());
		let result = match sent <= budget {
			true => mesh.check(),
			false => Err(format!("{sent} announce messages, over the budget of {budget}")),
		};
		let result = match result {
			Ok(()) => mesh.read().await,
			err => err,
		};
		if let Err(err) = result {
			return Err(format!("{}\n{err}", mesh.log));
		}
		// The reads' own requests settle before the next change is counted.
		kio::clock::sim::idle().await;
	}
	Ok(counts)
}

/// The case `index` of a sweep from `seed`: its own rng and simulator seed, and a shape.
fn case(seed: u64, index: u64) -> (u64, Shape) {
	let case_seed = Rng(seed ^ index.wrapping_mul(0xA24B_AED4_963E_E407)).next();
	let mut rng = Rng(case_seed);
	let shape = Shape {
		relays: rng.range(2, 10) as usize,
		changes: rng.range(5, 30) as usize,
	};
	(case_seed, shape)
}

fn setting(name: &str, fallback: u64) -> u64 {
	std::env::var(name).map_or(fallback, |value| value.parse().expect(name))
}

/// Runs the cases `MOQ_ROUTE_SEED` and `MOQ_ROUTE_CASES` pick.
#[test]
fn seeded_meshes_hold_the_route_rules() {
	let seed = setting("MOQ_ROUTE_SEED", 0x6d6f71);
	let cases = setting("MOQ_ROUTE_CASES", 8);
	let first = setting("MOQ_ROUTE_CASE", 0);
	eprintln!("MOQ_ROUTE_SEED={seed} MOQ_ROUTE_CASES={cases}");
	for index in first..first + cases {
		let (case_seed, shape) = case(seed, index);
		// A mesh that never goes quiet hangs rather than fails; the last of these names it.
		eprintln!("MOQ_ROUTE_CASE={index} {shape:?}");
		let result = kio::clock::sim::run_seeded(case_seed, run_case(Rng(case_seed), shape));
		if let Err(err) = result {
			panic!(
				"{shape:?} failed; replay it with MOQ_ROUTE_SEED={seed} MOQ_ROUTE_CASE={index} MOQ_ROUTE_CASES=1\n{err}"
			);
		}
	}
}

/// Announce messages per external change over fixed seeds, by mesh size and change.
#[test]
#[ignore = "a benchmark; Nightly reports it"]
fn benchmark_route_mesh() {
	println!("relays,change,mean_messages,max_messages");
	for relays in [3, 6, 12, 24] {
		let mut all = Counts::new();
		for index in 0..16 {
			let (case_seed, _) = case(0x6d6f71, index);
			let shape = Shape { relays, changes: 24 };
			let counts = kio::clock::sim::run_seeded(case_seed, run_case(Rng(case_seed), shape))
				.unwrap_or_else(|err| panic!("{shape:?} case {index}: {err}"));
			for (change, sent) in counts {
				all.entry(change).or_default().extend(sent);
			}
		}
		for (change, sent) in all {
			let mean = sent.iter().sum::<u64>() as f64 / sent.len() as f64;
			let max = sent.iter().max().unwrap();
			println!("{relays},{},{mean:.1},{max}", change.as_str());
		}
	}
}

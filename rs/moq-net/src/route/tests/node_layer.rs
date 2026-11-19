//! Two node layers under MoQ, on the bake-off's network beside the path vector that
//! ships, measured to decide whether moq-net keeps routing inside a trusted domain
//! (moq-dev/moq#9149: it does). Nothing here ships.
//!
//! Both keep what only MoQ does: an endpoint's claims belong to the relay it connects
//! to, which announces them under its own identity with a version, so a newer copy
//! always wins, and a reader shows a claim lost by its announcer unavailable for the
//! same debounce the router uses. They differ in what tells a relay how to reach an
//! announcer:
//!
//! - [`Layer::LinkState`]: each relay floods its links as it sees them, numbered, and
//!   computes cheapest paths over what it holds, as OSPF and Open/R do. A request
//!   follows each relay's next hop toward the announcer its first hop picked, as PIM
//!   joins follow the unicast table. Claims flood the same way. A session that opens
//!   swaps both databases whole, as a database exchange does.
//! - [`Layer::Flat`]: the mesh is the network underneath, which reaches any relay from
//!   any other while a path exists, and a relay keeps a session to every relay it can
//!   reach. An announcer sends its claims to every relay itself, and none passes on
//!   what it hears (split horizon). Which relays exist comes from a membership feed,
//!   which names a relay dead once one neighbour notices and back once it has a link;
//!   pairwise costs come from configuration, the mesh as built. A request goes straight
//!   to the announcer, its data crossing the network's own path. The network reroutes
//!   around a lost link unseen; a session whose path it loses entirely ends only when
//!   its keep-alives run out, the bake-off's idle timeout, since nothing below tells
//!   it.
//!
//! Both honour the mesh's marks as the route layer does: no path carries what a relay
//! learned over a link it marks upstream onward over another such link. Scoped links
//! join domains, which every option keeps routing by claims with a chain, so shapes
//! with them are left out.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
};

use super::bakeoff::{Failure, Row, Shape, fail, shapes};
use super::compare::{Arm, Chan, Faults, Held, Lab, Mesh, Net, Sent, tally};
use super::multi::{self, Broadcast, Seen, Tie};
use super::*;

/// What tells a relay how to reach an announcer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layer {
	LinkState,
	Flat,
}

/// Who sends the membership feed's messages.
const FEED: Node = usize::MAX;

/// How many rounds a reader shows a lost claim unavailable: the router's debounce.
fn debounce() -> u64 {
	(relay::debounce(HOP).as_millis() / HOP.as_millis()) as u64
}

/// The relay an identity names.
fn node_of(ident: Id) -> Node {
	(ident % 1_000) as Node
}

/// Whether an announcer still claims a broadcast, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
	Live,
	/// The publisher withdrew it.
	Ended,
	/// The announcer lost its publisher.
	Lost,
	/// Flat: this relay lost its session to the announcer, so the copy is stale until
	/// the announcer sends it again.
	Gone,
}

/// A relay's links as it last saw them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Lsa {
	seq: u64,
	/// Each neighbour's identity, the link's cost, and whether this relay marks it.
	links: BTreeMap<Id, (Metric, bool)>,
	draining: bool,
}

#[derive(Clone, Debug)]
enum Msg {
	Lsa {
		origin: Id,
		lsa: Lsa,
	},
	Claim {
		announcer: Id,
		broadcast: Broadcast,
		version: u64,
		state: State,
	},
	/// An endpoint's claim, to its relay.
	Own {
		broadcast: Broadcast,
		live: bool,
	},
	/// The membership feed: `node` is up under `ident`, or that identity is gone.
	Member {
		node: Node,
		ident: Id,
		alive: bool,
	},
	/// A flat relay's GOAWAY on one of its sessions.
	Drain,
}

/// A claim as a reader shows it.
#[derive(Clone, Copy, Debug)]
struct Shown {
	available: bool,
	/// When it goes offline, while unavailable.
	offline: Option<u64>,
}

/// A cheapest path's rank: through a draining relay, then cost, then first hop.
type Rank = (bool, Metric, Node);

#[derive(Clone, Debug, Default)]
struct Relay {
	ident: Id,
	version: u64,
	seq: u64,
	draining: bool,
	/// Relay neighbours by node, with their identity.
	adj: BTreeMap<Node, Id>,
	endpoints: BTreeSet<Node>,
	/// What each endpoint claims through this relay.
	own: BTreeMap<Node, BTreeSet<Broadcast>>,
	lsdb: BTreeMap<Id, Lsa>,
	claims: BTreeMap<(Id, Broadcast), (u64, State)>,
	/// Link state: the rank and next hop toward each identity.
	table: BTreeMap<Id, (Rank, Node)>,
	/// Flat: the relays it holds a session to, with their identity.
	sessions: BTreeMap<Node, Id>,
	/// Flat: the relays the membership feed says exist.
	known: BTreeSet<Node>,
	/// Flat: the relays that sent GOAWAY.
	drains: BTreeSet<Id>,
	shown: BTreeMap<Broadcast, Shown>,
}

#[derive(Clone, Debug)]
enum Spot {
	Relay(Box<Relay>),
	Endpoint { id: Option<Id>, claims: BTreeSet<Path> },
}

/// A network of relays running one node layer, and their endpoints.
pub(super) struct LayerSim {
	layer: Layer,
	spots: Vec<Spot>,
	restarts: Vec<u32>,
	/// Each side's cost and whether it marks the link, keyed from that side.
	ports: BTreeMap<(Node, Node), (Metric, bool)>,
	up: BTreeSet<(Node, Node)>,
	/// Processes gone, whose links may not have closed yet.
	dead: BTreeSet<Node>,
	/// Flat: what the feed last said of each relay.
	members: BTreeSet<Node>,
	/// Flat: configured cost between relays, the cheapest path of the mesh as built.
	config: BTreeMap<(Node, Node), Metric>,
	/// Flat: sessions whose path the network lost, and when their keep-alives run out.
	doomed: BTreeMap<(Node, Node), u64>,
	/// Link state: relays whose links changed this round.
	dirty: BTreeSet<Node>,
	/// Flat: relays that sent GOAWAY and closed their sessions, which open no more.
	leaving: BTreeSet<Node>,
	built: bool,
	/// Each message with the round it may arrive in: a flat session crosses the
	/// network's path between its relays, a round per link, and waits with `None`
	/// while the network has no path, setting out again once it has one.
	queues: BTreeMap<Chan, VecDeque<(Option<u64>, Msg)>>,
	/// Flat: the links of the network's path between two relays, while one exists.
	paths: std::cell::RefCell<BTreeMap<(Node, Node), Option<usize>>>,
	/// A queue is delivering this round, so what it sends moves next round at the earliest.
	stepping: bool,
	held: BTreeSet<Chan>,
	lives: BTreeMap<Broadcast, u64>,
	stats: Stats,
	events: Vec<Seen>,
	now: u64,
	spent: usize,
}

fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

/// Cheapest paths from `source` to every node `edges` reaches. No path carries what a
/// relay learned over a link it marks onward over another link it marks, a path
/// already `marked` leaves `source` by no link it marks, and only a node `transit`
/// admits passes a path on. Returns each node's rank and the path.
fn cheapest(
	source: Id,
	marked: bool,
	edges: &dyn Fn(Id) -> Vec<(Id, Metric)>,
	marks: &dyn Fn(Id, Id) -> bool,
	draining: &dyn Fn(Id) -> bool,
	transit: &dyn Fn(Id) -> bool,
	spent: &mut usize,
) -> BTreeMap<Id, (Rank, Vec<Id>)> {
	// A label is a node, whether some relay on the way marked the link it arrived over
	// (from such a relay on, the path never leaves over a link its sender marks), and
	// whether it passed a draining relay. Draining ranks first, so a cheaper drained
	// path to a node is kept beside a dearer clean one: past another draining relay
	// both are drained and the cheaper wins.
	type Label = (Id, bool, bool);
	let mut best: BTreeMap<Label, (Rank, Option<Label>)> = BTreeMap::new();
	let mut heap = BinaryHeap::new();
	let start = (source, marked, false);
	best.insert(start, ((false, 0, usize::MAX), None));
	heap.push(Reverse(((false, 0, usize::MAX), start)));
	while let Some(Reverse((rank, label))) = heap.pop() {
		if best.get(&label).is_some_and(|(have, _)| *have < rank) {
			continue;
		}
		let (at, marked, _) = label;
		if at != source && !transit(at) {
			continue;
		}
		for (next, cost) in edges(at) {
			*spent += 1;
			if marked && marks(at, next) {
				continue;
			}
			let drain = rank.0 || (at != source && draining(at));
			let first = if at == source { node_of(next) } else { rank.2 };
			let reach = (drain, rank.1 + cost, first);
			let next = (next, marked || marks(next, at), drain);
			if best.get(&next).is_none_or(|(have, _)| reach < *have) {
				best.insert(next, (reach, Some(label)));
				heap.push(Reverse((reach, next)));
			}
		}
	}
	let mut out: BTreeMap<Id, (Rank, Vec<Id>)> = BTreeMap::new();
	for (&label, &(rank, _)) in &best {
		if label.0 == source || out.get(&label.0).is_some_and(|(have, _)| *have <= rank) {
			continue;
		}
		let mut path = vec![label.0];
		let mut at = label;
		while let Some((_, Some(prev))) = best.get(&at) {
			if prev.0 != source {
				path.push(prev.0);
			}
			at = *prev;
		}
		path.reverse();
		out.insert(label.0, (rank, path));
	}
	out
}

impl LayerSim {
	pub fn new(layer: Layer, nodes: usize) -> Self {
		let spots = (0..nodes)
			.map(|node| {
				Spot::Relay(Box::new(Relay {
					ident: hop(node, 0),
					..Relay::default()
				}))
			})
			.collect();
		Self {
			layer,
			spots,
			restarts: vec![0; nodes],
			ports: BTreeMap::new(),
			up: BTreeSet::new(),
			dead: BTreeSet::new(),
			members: BTreeSet::new(),
			config: BTreeMap::new(),
			doomed: BTreeMap::new(),
			dirty: BTreeSet::new(),
			leaving: BTreeSet::new(),
			built: false,
			queues: BTreeMap::new(),
			paths: std::cell::RefCell::new(BTreeMap::new()),
			stepping: false,
			held: BTreeSet::new(),
			lives: BTreeMap::new(),
			stats: Stats::default(),
			events: Vec::new(),
			now: 0,
			spent: 0,
		}
	}

	fn relay(&self, node: Node) -> Option<&Relay> {
		match self.spots.get(node)? {
			Spot::Relay(relay) => Some(relay),
			Spot::Endpoint { .. } => None,
		}
	}

	fn relay_mut(&mut self, node: Node) -> Option<&mut Relay> {
		match self.spots.get_mut(node)? {
			Spot::Relay(relay) => Some(relay),
			Spot::Endpoint { .. } => None,
		}
	}

	fn is_relay(&self, node: Node) -> bool {
		self.relay(node).is_some()
	}

	fn relays(&self) -> Vec<Node> {
		(0..self.spots.len()).filter(|&node| self.is_relay(node)).collect()
	}

	fn send(&mut self, from: Node, to: Node, msg: Msg) {
		match &msg {
			Msg::Lsa { lsa, .. } => {
				self.stats.route_msgs += 1;
				self.stats.route_items += lsa.links.len().max(1);
			}
			Msg::Member { .. } | Msg::Drain => {
				self.stats.route_msgs += 1;
				self.stats.route_items += 1;
			}
			Msg::Claim { .. } | Msg::Own { .. } => {
				self.stats.claim_msgs += 1;
				self.stats.claim_items += 1;
			}
		}
		// A message sent while this round delivers can move no earlier than the next,
		// so it waits one round more than one sent between rounds.
		let due =
			(self.links_between(from, to)).map(|links| self.now + links.max(1) as u64 - 1 + u64::from(self.stepping));
		self.queues.entry((from, to)).or_default().push_back((due, msg));
	}

	fn live(&self, broadcast: &Broadcast) -> bool {
		self.spots.iter().enumerate().any(|(node, spot)| match spot {
			Spot::Endpoint { id, claims } => {
				*id == Some(broadcast.0)
					&& claims.contains(&broadcast.1)
					&& self.up.iter().any(|&(a, b)| a == node || b == node)
			}
			Spot::Relay(_) => false,
		})
	}

	/// The relays a request from `relay` may leave by, with each link's cost.
	fn mesh_edges(&self, node: Node) -> Vec<(Node, Metric)> {
		(self.up.iter())
			.filter_map(|&(a, b)| match node {
				_ if a == node => Some(b),
				_ if b == node => Some(a),
				_ => None,
			})
			.filter(|&peer| self.is_relay(peer))
			.map(|peer| (peer, self.ports[&(node, peer)].0))
			.collect()
	}

	fn marks(&self, x: Node, y: Node) -> bool {
		self.ports.get(&(x, y)).is_some_and(|&(_, marked)| marked)
	}

	/// Flat: the network's own path from `from` to `to` over open links and live
	/// relays, honouring the marks.
	fn underlay(&self, from: Node, to: Node) -> Option<Vec<Node>> {
		if from == to {
			return Some(Vec::new());
		}
		self.underlay_from(from).remove(&to)
	}

	/// Flat: [`Self::underlay`] from `from` to every relay it reaches.
	fn underlay_from(&self, from: Node) -> BTreeMap<Node, Vec<Node>> {
		let mut spent = 0;
		let as_id = |node: Node| node as Id;
		let edges = |at: Id| {
			(self.mesh_edges(at as Node).into_iter())
				.map(|(peer, cost)| (as_id(peer), cost))
				.collect()
		};
		let marks = |x: Id, y: Id| self.marks(x as Node, y as Node);
		let transit = |at: Id| !self.dead.contains(&(at as Node));
		let found = cheapest(as_id(from), false, &edges, &marks, &|_| false, &transit, &mut spent);
		(found.into_iter())
			.map(|(id, (_, path))| (id as Node, path.into_iter().map(|id| id as Node).collect()))
			.collect()
	}

	/// Flat: how many links a message between two relays crosses, or `None` while the
	/// network has no path between them. Every other session is one link.
	fn links_between(&self, from: Node, to: Node) -> Option<usize> {
		if self.layer != Layer::Flat || !self.is_relay(from) || !self.is_relay(to) {
			return Some(1);
		}
		// Ordered: ties break by the source's first hop, so the way back can differ.
		let key = (from, to);
		if let Some(&known) = self.paths.borrow().get(&key) {
			return known;
		}
		// One search from `from` answers every pair it starts.
		let reached = self.underlay_from(from);
		let mut paths = self.paths.borrow_mut();
		for relay in self.relays().into_iter().filter(|&relay| relay != from) {
			paths.insert((from, relay), reached.get(&relay).map(Vec::len));
		}
		paths.insert((from, from), Some(1));
		paths[&key]
	}

	/// The network changed: forget every path worked out on it. A flat message whose
	/// path is gone waits, and one waiting sets out on the path that came back, as
	/// if sent now: no hop moves while the network has no path.
	fn reroute(&mut self) {
		self.paths.get_mut().clear();
		if self.layer != Layer::Flat {
			return;
		}
		let now = self.now;
		let chans: Vec<Chan> = (self.queues.iter())
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(&chan, _)| chan)
			.collect();
		for (from, to) in chans {
			let links = self.links_between(from, to);
			for (due, _) in self.queues.get_mut(&(from, to)).expect("a queue") {
				*due = match links {
					None => None,
					Some(links) => due.or(Some(now + links.max(1) as u64 - 1)),
				};
			}
		}
	}

	/// The identity a relay's link-state database says `ident` links to, both ways.
	fn ls_edges(relay: &Relay, ident: Id) -> Vec<(Id, Metric)> {
		let Some(lsa) = relay.lsdb.get(&ident) else {
			return Vec::new();
		};
		(lsa.links.iter())
			.filter(|(peer, _)| relay.lsdb.get(peer).is_some_and(|back| back.links.contains_key(&ident)))
			.map(|(&peer, &(cost, _))| (peer, cost))
			.collect()
	}

	/// Link state: `relay`'s rank and next hop toward each identity in its database,
	/// for a request that arrived `marked` or not.
	fn next_hops(relay: &Relay, marked: bool, spent: &mut usize) -> BTreeMap<Id, (Rank, Node)> {
		let edges = |at: Id| Self::ls_edges(relay, at);
		let marks = |x: Id, y: Id| {
			relay
				.lsdb
				.get(&x)
				.and_then(|lsa| lsa.links.get(&y))
				.is_some_and(|l| l.1)
		};
		let draining = |at: Id| relay.lsdb.get(&at).is_some_and(|lsa| lsa.draining);
		let transit = |at: Id| relay.lsdb.contains_key(&at);
		(cheapest(relay.ident, marked, &edges, &marks, &draining, &transit, spent).into_iter())
			.map(|(ident, (rank, path))| (ident, (rank, node_of(path[0]))))
			.collect()
	}

	fn compute(&mut self, node: Node) {
		let mut spent = 0;
		let Some(relay) = self.relay(node) else {
			return;
		};
		let table = Self::next_hops(relay, false, &mut spent);
		self.spent += spent;
		self.relay_mut(node).expect("a relay").table = table;
	}

	fn lsa(relay: &Relay, ports: &BTreeMap<(Node, Node), (Metric, bool)>, node: Node) -> Lsa {
		Lsa {
			seq: relay.seq,
			links: (relay.adj.iter())
				.map(|(&peer, &ident)| (ident, ports[&(node, peer)]))
				.collect(),
			draining: relay.draining,
		}
	}

	/// Link state: `node` sees its links change. It routes on them at once and floods
	/// them, numbered anew, once the round ends, as the router advertises once per burst.
	fn originate(&mut self, node: Node) {
		let ports = &self.ports;
		let Some(Spot::Relay(relay)) = self.spots.get_mut(node) else {
			return;
		};
		let lsa = Self::lsa(relay, ports, node);
		relay.lsdb.insert(relay.ident, lsa);
		self.dirty.insert(node);
		self.compute(node);
		self.review(node);
	}

	/// Link state: every relay whose links changed floods them, once per burst as the
	/// router batches: what a change from outside did floods before the next round
	/// delivers, and what a round's deliveries did floods as it ends.
	fn flood_dirty(&mut self) {
		for node in std::mem::take(&mut self.dirty) {
			self.flood(node);
		}
	}

	/// Link state: `node` floods its links, numbered anew.
	fn flood(&mut self, node: Node) {
		let ports = &self.ports;
		let Some(Spot::Relay(relay)) = self.spots.get_mut(node) else {
			return;
		};
		relay.seq += 1;
		let lsa = Self::lsa(relay, ports, node);
		let origin = relay.ident;
		relay.lsdb.insert(origin, lsa.clone());
		let peers: Vec<Node> = relay.adj.keys().copied().collect();
		for peer in peers {
			self.send(
				node,
				peer,
				Msg::Lsa {
					origin,
					lsa: lsa.clone(),
				},
			);
		}
	}

	/// `node` announces a change to an endpoint's claim under its own identity.
	fn announce(&mut self, node: Node, broadcast: Broadcast, state: State) {
		let layer = self.layer;
		let relay = self.relay_mut(node).expect("a relay");
		relay.version += 1;
		let (announcer, version) = (relay.ident, relay.version);
		relay.claims.insert((announcer, broadcast.clone()), (version, state));
		let peers: Vec<Node> = match layer {
			Layer::LinkState => relay.adj.keys().copied().collect(),
			Layer::Flat => relay.sessions.keys().copied().collect(),
		};
		for peer in peers {
			let msg = Msg::Claim {
				announcer,
				broadcast: broadcast.clone(),
				version,
				state,
			};
			self.send(node, peer, msg);
		}
		self.review(node);
	}

	/// The identity `node` would pull `broadcast` from, with its cost.
	fn pick(&self, node: Node, broadcast: &Broadcast) -> Option<(Id, Rank)> {
		let relay = self.relay(node)?;
		(relay.claims.iter())
			.filter(|((_, b), (_, state))| b == broadcast && *state == State::Live)
			.filter_map(|((announcer, _), _)| Some((*announcer, self.reach(node, *announcer)?)))
			.min_by_key(|&(announcer, rank)| (rank, announcer))
	}

	/// How `node` reaches announcer `ident`, if it can.
	fn reach(&self, node: Node, ident: Id) -> Option<Rank> {
		let relay = self.relay(node)?;
		if ident == relay.ident {
			return Some((false, 0, node));
		}
		match self.layer {
			Layer::LinkState => relay.table.get(&ident).map(|&(rank, _)| rank),
			Layer::Flat => {
				let peer = node_of(ident);
				(relay.sessions.get(&peer) == Some(&ident)).then(|| {
					let cost = self.config.get(&pair(node, peer)).copied().unwrap_or(Metric::MAX / 2);
					(relay.drains.contains(&ident), cost, peer)
				})
			}
		}
	}

	/// Show `node`'s readers what its claims now say.
	fn review(&mut self, node: Node) {
		let Some(relay) = self.relay(node) else {
			return;
		};
		let mut broadcasts: BTreeSet<Broadcast> = relay.claims.keys().map(|(_, b)| b.clone()).collect();
		broadcasts.extend(relay.shown.keys().cloned());
		let mut changes = Vec::new();
		for broadcast in broadcasts {
			let copies: Vec<(Id, State)> = (relay.claims.iter())
				.filter(|((_, b), _)| *b == broadcast)
				.map(|((announcer, _), &(_, state))| (*announcer, state))
				.collect();
			let live = copies.iter().any(|(_, state)| *state == State::Live);
			let reachable = self.pick(node, &broadcast).is_some();
			let ended = !live && copies.iter().all(|(_, state)| *state == State::Ended);
			let was = relay.shown.get(&broadcast).copied();
			let kind = match (was, reachable) {
				(None, true) => Some((Kind::Start, Some(true))),
				(None, false) => None,
				(Some(_), _) if ended => Some((Kind::End, None)),
				(Some(shown), true) if !shown.available => Some((Kind::Available, Some(true))),
				(Some(shown), false) if shown.available => Some((Kind::Unavailable, Some(false))),
				_ => None,
			};
			if let Some(kind) = kind {
				changes.push((broadcast, kind));
			}
		}
		let now = self.now;
		for (broadcast, (kind, shown)) in changes {
			let relay = self.relay_mut(node).expect("a relay");
			match shown {
				Some(available) => {
					let offline = (!available).then_some(now + debounce());
					relay.shown.insert(broadcast.clone(), Shown { available, offline });
				}
				None => {
					relay.shown.remove(&broadcast);
				}
			}
			self.event(node, broadcast, kind);
		}
	}

	fn event(&mut self, node: Node, broadcast: Broadcast, kind: Kind) {
		let seen = Seen {
			node,
			live: self.live(&broadcast),
			life: self.lives.get(&broadcast).copied().unwrap_or(0),
			broadcast,
			kind,
			forgot: 0,
		};
		self.events.push(seen);
	}

	/// Flat: the relay pairs that hold a session: each knows the other and the network
	/// joins them.
	fn wanted(&self) -> BTreeSet<(Node, Node)> {
		let relays = self.relays();
		let mut out = BTreeSet::new();
		for &a in &relays {
			for &b in relays.iter().filter(|&&b| b > a) {
				let knows = |x: Node, y: Node| {
					!self.leaving.contains(&x) && self.relay(x).is_some_and(|relay| relay.known.contains(&y))
				};
				if knows(a, b) && knows(b, a) && self.links_between(a, b).is_some() {
					out.insert((a, b));
				}
			}
		}
		out
	}

	/// Flat: open and close sessions to match the network and the feed.
	fn resync(&mut self) {
		if self.layer != Layer::Flat || !self.built {
			return;
		}
		// The feed names a relay back once it has a link again.
		for node in self.relays() {
			let linked = !self.mesh_edges(node).is_empty() && !self.dead.contains(&node);
			if linked && !self.members.contains(&node) {
				self.members.insert(node);
				let ident = self.relay(node).expect("a relay").ident;
				for peer in self.relays().into_iter().filter(|&peer| peer != node) {
					self.send(
						FEED,
						peer,
						Msg::Member {
							node,
							ident,
							alive: true,
						},
					);
				}
				let all: BTreeSet<Node> = self.members.iter().copied().filter(|&m| m != node).collect();
				self.relay_mut(node).expect("a relay").known = all;
			}
		}
		let want = self.wanted();
		// A session is open once both ends hold it under the other's current identity;
		// one end alone, or a stale identity, is reopened while wanted.
		let holds = |x: Node, y: Node| {
			let ident = self.relay(y).expect("a relay").ident;
			self.relay(x).expect("a relay").sessions.get(&y) == Some(&ident)
		};
		let mut have = BTreeSet::new();
		let mut open = BTreeSet::new();
		for a in self.relays() {
			for &b in self.relay(a).expect("a relay").sessions.keys() {
				have.insert(pair(a, b));
				if holds(a, b) && holds(b, a) {
					open.insert(pair(a, b));
				}
			}
		}
		// The network lost the path between them, which only the session's keep-alives
		// show; one that comes back first carries on.
		let now = self.now;
		for &(a, b) in have.difference(&want) {
			self.doomed.entry((a, b)).or_insert(now + super::bakeoff::IDLE as u64);
		}
		self.doomed.retain(|pair, _| !want.contains(pair));
		for &(a, b) in want.difference(&open) {
			for (x, y) in [(a, b), (b, a)] {
				let ident = self.relay(y).expect("a relay").ident;
				if self
					.relay(x)
					.expect("a relay")
					.sessions
					.get(&y)
					.is_some_and(|&held| held != ident)
				{
					self.forget(x, y);
				}
				self.relay_mut(x).expect("a relay").sessions.insert(y, ident);
			}
			for (x, y) in [(a, b), (b, a)] {
				let relay = self.relay(x).expect("a relay");
				let mine: Vec<(Broadcast, u64, State)> = (relay.claims.iter())
					.filter(|((announcer, _), _)| *announcer == relay.ident)
					.map(|((_, broadcast), &(version, state))| (broadcast.clone(), version, state))
					.collect();
				let (announcer, draining) = (relay.ident, relay.draining);
				for (broadcast, version, state) in mine {
					let msg = Msg::Claim {
						announcer,
						broadcast,
						version,
						state,
					};
					self.send(x, y, msg);
				}
				if draining {
					self.send(x, y, Msg::Drain);
				}
			}
			self.review(a);
			self.review(b);
		}
	}

	/// Flat: the session between `a` and `b` ends; each loses what the other announced.
	fn part(&mut self, a: Node, b: Node) {
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.held.remove(&(x, y));
			self.forget(x, y);
		}
	}

	/// Flat: `x` lets go of its session to `y` and of `y`'s claims.
	fn forget(&mut self, x: Node, y: Node) {
		let Some(relay) = self.relay_mut(x) else {
			return;
		};
		let Some(ident) = relay.sessions.remove(&y) else {
			return;
		};
		for ((announcer, _), (_, state)) in relay.claims.iter_mut() {
			if *announcer == ident {
				*state = State::Gone;
			}
		}
		self.review(x);
	}

	fn connect(&mut self, a: Node, b: Node) {
		for (x, y) in [(a, b), (b, a)] {
			match (self.is_relay(x), self.is_relay(y)) {
				(true, true) => {
					let ident = self.relay(y).expect("a relay").ident;
					self.relay_mut(x).expect("a relay").adj.insert(y, ident);
				}
				(true, false) => {
					self.relay_mut(x).expect("a relay").endpoints.insert(y);
				}
				(false, true) => {
					let Spot::Endpoint { id: Some(id), claims } = &self.spots[x] else {
						continue;
					};
					let own: Vec<Broadcast> = claims.iter().map(|claim| (*id, claim.clone())).collect();
					for broadcast in own {
						self.send(x, y, Msg::Own { broadcast, live: true });
					}
				}
				(false, false) => {}
			}
		}
		if self.is_relay(a) && self.is_relay(b) && self.layer == Layer::LinkState {
			for node in [a, b] {
				self.originate(node);
			}
			// A database exchange: each side sends the other everything it holds.
			for (x, y) in [(a, b), (b, a)] {
				let relay = self.relay(x).expect("a relay");
				let lsas: Vec<(Id, Lsa)> = relay.lsdb.iter().map(|(&i, l)| (i, l.clone())).collect();
				let claims: Vec<((Id, Broadcast), (u64, State))> =
					relay.claims.iter().map(|(k, &v)| (k.clone(), v)).collect();
				for (origin, lsa) in lsas {
					self.send(x, y, Msg::Lsa { origin, lsa });
				}
				for ((announcer, broadcast), (version, state)) in claims {
					let msg = Msg::Claim {
						announcer,
						broadcast,
						version,
						state,
					};
					self.send(x, y, msg);
				}
			}
		}
		self.resync();
	}

	/// `peer` lets go of its session to `node`.
	fn disconnect(&mut self, node: Node, peer: Node) {
		if !self.is_relay(peer) {
			return;
		}
		if self.is_relay(node) {
			let relay = self.relay_mut(peer).expect("a relay");
			if relay.adj.remove(&node).is_some() && self.layer == Layer::LinkState {
				self.originate(peer);
			}
		} else {
			let relay = self.relay_mut(peer).expect("a relay");
			relay.endpoints.remove(&node);
			let lost: Vec<Broadcast> = relay.own.remove(&node).unwrap_or_default().into_iter().collect();
			for broadcast in lost {
				self.announce(peer, broadcast, State::Lost);
			}
		}
	}

	fn drop_link(&mut self, a: Node, b: Node) {
		self.up.remove(&pair(a, b));
		self.reroute();
		// A flat session between relays outlives the link: it ends only by `part`.
		if self.layer == Layer::Flat && self.is_relay(a) && self.is_relay(b) {
			return;
		}
		for key in [(a, b), (b, a)] {
			self.queues.remove(&key);
			self.held.remove(&key);
		}
	}

	/// The cheapest path between every pair of relays on the mesh as built.
	fn configure(&mut self) {
		for a in self.relays() {
			for b in self.relays().into_iter().filter(|&b| b > a) {
				if let Some(path) = self.underlay(a, b) {
					let mut cost = 0;
					let mut at = a;
					for next in path {
						cost += self.ports[&(at, next)].0;
						at = next;
					}
					self.config.insert((a, b), cost);
				}
			}
		}
		self.members = self.relays().into_iter().collect();
		for node in self.relays() {
			let all = self.members.iter().copied().filter(|&m| m != node).collect();
			self.relay_mut(node).expect("a relay").known = all;
		}
		self.built = true;
		self.resync();
	}

	fn deliver(&mut self, from: Node, to: Node) {
		let Some((_, msg)) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return;
		};
		self.stats.delivered += 1;
		match msg {
			Msg::Lsa { origin, lsa } => {
				let relay = self.relay_mut(to).expect("a relay");
				if relay.lsdb.get(&origin).is_some_and(|have| have.seq >= lsa.seq) {
					return;
				}
				// A relay hearing an old copy of itself numbers past it.
				if origin == relay.ident {
					relay.seq = lsa.seq;
					self.originate(to);
					return;
				}
				relay.lsdb.insert(origin, lsa.clone());
				let peers: Vec<Node> = relay.adj.keys().copied().filter(|&peer| peer != from).collect();
				for peer in peers {
					self.send(
						to,
						peer,
						Msg::Lsa {
							origin,
							lsa: lsa.clone(),
						},
					);
				}
				self.compute(to);
				self.review(to);
			}
			Msg::Claim {
				announcer,
				broadcast,
				version,
				state,
			} => {
				let layer = self.layer;
				let relay = self.relay_mut(to).expect("a relay");
				let key = (announcer, broadcast.clone());
				// A copy forgotten with its session takes the announcer's word again.
				let stale = |&(have, held): &(u64, State)| have > version || (have == version && held != State::Gone);
				if relay.claims.get(&key).is_some_and(stale) {
					return;
				}
				// Flat: a copy from a relay it no longer holds a session to is stale.
				if layer == Layer::Flat && relay.sessions.get(&from) != Some(&announcer) {
					return;
				}
				relay.claims.insert(key, (version, state));
				if layer == Layer::LinkState {
					let peers: Vec<Node> = relay.adj.keys().copied().filter(|&peer| peer != from).collect();
					for peer in peers {
						let msg = Msg::Claim {
							announcer,
							broadcast: broadcast.clone(),
							version,
							state,
						};
						self.send(to, peer, msg);
					}
				}
				self.review(to);
			}
			Msg::Own { broadcast, live } => {
				let relay = self.relay_mut(to).expect("a relay");
				let own = relay.own.entry(from).or_default();
				let changed = match live {
					true => own.insert(broadcast.clone()),
					false => own.remove(&broadcast),
				};
				if changed {
					let state = if live { State::Live } else { State::Ended };
					self.announce(to, broadcast, state);
				}
			}
			Msg::Member { node, ident, alive } => {
				let relay = self.relay_mut(to).expect("a relay");
				match alive {
					true => {
						relay.known.insert(node);
					}
					// The news of an identity it already holds a newer session past is old.
					false if relay.sessions.get(&node).is_some_and(|&held| held != ident) => {}
					false => {
						relay.known.remove(&node);
						self.forget(to, node);
					}
				}
				self.resync();
			}
			Msg::Drain => {
				let ident = self.relay(from).map(|relay| relay.ident);
				if let (Some(ident), Some(relay)) = (ident, self.relay_mut(to)) {
					relay.drains.insert(ident);
				}
				self.review(to);
			}
		}
	}

	/// The endpoint through which announcer `ident` at `home` serves `broadcast`.
	fn served(&self, home: Node, ident: Id, broadcast: &Broadcast) -> Option<Node> {
		let relay = self.relay(home).filter(|relay| relay.ident == ident)?;
		(relay.own.iter())
			.find(|(endpoint, set)| set.contains(broadcast) && relay.endpoints.contains(endpoint))
			.map(|(&endpoint, _)| endpoint)
	}
}

impl Net for LayerSim {
	fn client(&mut self, node: Node) {
		self.spots[node] = Spot::Endpoint {
			id: None,
			claims: BTreeSet::new(),
		};
	}

	fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		assert_ne!(tie, Tie::Scoped, "a node layer serves one domain");
		self.ports.insert((a, b), (cost, tie == Tie::Up));
		self.ports.insert((b, a), (cost, false));
		self.open(a, b);
	}

	fn goaway(&mut self, node: Node) {
		let Some(relay) = self.relay_mut(node) else {
			return;
		};
		relay.draining = true;
		let peers: Vec<Node> = relay.sessions.keys().copied().collect();
		match self.layer {
			Layer::LinkState => self.originate(node),
			Layer::Flat => {
				for peer in peers {
					self.send(node, peer, Msg::Drain);
				}
			}
		}
	}

	fn open(&mut self, a: Node, b: Node) {
		assert!(self.up.insert(pair(a, b)), "{a}-{b} already up");
		self.reroute();
		self.connect(a, b);
	}

	fn close(&mut self, a: Node, b: Node) {
		// A flat relay that sent GOAWAY and now closes its links is leaving: it closes
		// every session of its own, wherever they run.
		for node in [a, b] {
			if self.layer == Layer::Flat && self.relay(node).is_some_and(|relay| relay.draining) {
				self.leaving.insert(node);
				let peers: Vec<Node> = self.relay(node).expect("a relay").sessions.keys().copied().collect();
				for peer in peers {
					self.doomed.remove(&pair(node, peer));
					self.part(node, peer);
				}
			}
		}
		self.drop_link(a, b);
		self.disconnect(a, b);
		self.disconnect(b, a);
		self.resync();
	}

	fn mute(&mut self, node: Node, peer: Node) {
		self.dead.insert(node);
		self.reroute();
		self.held.insert((node, peer));
		self.held.insert((peer, node));
		if self.layer == Layer::Flat {
			let keys: Vec<Chan> = self
				.queues
				.keys()
				.copied()
				.filter(|&(x, y)| x == node || y == node)
				.collect();
			self.held.extend(keys);
		}
	}

	fn sever(&mut self, node: Node, peer: Node) {
		self.drop_link(node, peer);
		self.disconnect(node, peer);
		// The feed names a dead relay gone once a neighbour notices.
		if self.layer == Layer::Flat && self.dead.contains(&node) && self.members.remove(&node) {
			let ident = self.relay(node).expect("a relay").ident;
			for to in self.relays().into_iter().filter(|&to| to != node) {
				self.send(
					FEED,
					to,
					Msg::Member {
						node,
						ident,
						alive: false,
					},
				);
			}
		}
		self.resync();
	}

	fn reset(&mut self, node: Node) {
		let peers: Vec<Node> = (self.up.iter())
			.filter_map(|&(a, b)| match node {
				_ if a == node => Some(b),
				_ if b == node => Some(a),
				_ => None,
			})
			.collect();
		for peer in peers {
			self.close(node, peer);
		}
		let keys: Vec<Chan> = self
			.queues
			.keys()
			.copied()
			.filter(|&(x, y)| x == node || y == node)
			.collect();
		for key in keys {
			self.queues.remove(&key);
			self.held.remove(&key);
		}
		self.held.retain(|&(x, y)| x != node && y != node);
		self.doomed.retain(|&(a, b), _| a != node && b != node);
		self.dirty.remove(&node);
		self.leaving.remove(&node);
		if self.layer == Layer::Flat {
			for other in self.relays() {
				self.forget(other, node);
			}
			self.members.remove(&node);
		}
		self.restarts[node] += 1;
		self.dead.remove(&node);
		self.reroute();
		if self.is_relay(node) {
			self.spots[node] = Spot::Relay(Box::new(Relay {
				ident: hop(node, self.restarts[node]),
				..Relay::default()
			}));
		}
		self.resync();
	}

	fn publish(&mut self, node: Node, named: Id) {
		if !self.built {
			self.configure();
		}
		let Spot::Endpoint { id, .. } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		*id = Some(named);
	}

	fn claim(&mut self, node: Node, claim: &str, live: bool) {
		let claim = p(claim);
		let Spot::Endpoint { id, claims } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		let id = id.expect("a publisher");
		let changed = match live {
			true => claims.insert(claim.clone()),
			false => claims.remove(&claim),
		};
		if !changed {
			return;
		}
		let broadcast = (id, claim);
		if live {
			*self.lives.entry(broadcast.clone()).or_default() += 1;
		}
		let relays: Vec<Node> = (self.up.iter())
			.filter_map(|&(a, b)| match node {
				_ if a == node => Some(b),
				_ if b == node => Some(a),
				_ => None,
			})
			.collect();
		for relay in relays {
			self.send(
				node,
				relay,
				Msg::Own {
					broadcast: broadcast.clone(),
					live,
				},
			);
		}
	}

	fn ready(&self) -> Vec<(Chan, usize)> {
		(self.queues.iter())
			.filter(|(key, _)| !self.held.contains(key))
			// A flat session waits while the network has no path to carry it.
			.filter(|((from, to), _)| self.links_between(*from, *to).is_some())
			.map(|(key, queue)| {
				let due = queue
					.iter()
					.take_while(|(due, _)| due.is_some_and(|due| due <= self.now));
				(*key, due.count())
			})
			.filter(|&(_, count)| count > 0)
			.collect()
	}

	fn flush(&mut self) {
		self.flood_dirty();
	}

	fn step(&mut self, (from, to): Chan) {
		self.stepping = true;
		self.deliver(from, to);
		self.stepping = false;
	}

	fn tick(&mut self) -> bool {
		self.flood_dirty();
		self.now += 1;
		let now = self.now;
		// Flat: a session whose path the network lost ends when its keep-alives run out.
		let due: Vec<(Node, Node)> = (self.doomed.iter())
			.filter(|&(_, &at)| at <= now)
			.map(|(&pair, _)| pair)
			.collect();
		for (a, b) in due {
			self.doomed.remove(&(a, b));
			self.part(a, b);
		}
		let mut waiting = !self.doomed.is_empty() || self.queues.values().any(|queue| !queue.is_empty());
		for node in self.relays() {
			let relay = self.relay(node).expect("a relay");
			let due: Vec<Broadcast> = (relay.shown.iter())
				.filter(|(_, shown)| shown.offline.is_some_and(|at| at <= now))
				.map(|(broadcast, _)| broadcast.clone())
				.collect();
			waiting |= relay
				.shown
				.values()
				.any(|shown| shown.offline.is_some_and(|at| at > now));
			for broadcast in due {
				self.relay_mut(node).expect("a relay").shown.remove(&broadcast);
				self.event(node, broadcast, Kind::Offline);
			}
		}
		waiting
	}

	fn chase(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)> {
		let broadcast = (id, p(&Mesh::claim(0)));
		let (announcer, _) = self.pick(from, &broadcast)?;
		let home = node_of(announcer);
		match self.layer {
			Layer::LinkState => {
				// Whether the request arrived over a link some relay on the way marks: a
				// relay forwards that one by no link it marks, as the sticky rule needs.
				// Rare, so worked out per request rather than kept as a second table.
				let (mut at, mut hops, mut marked) = (from, Vec::new(), false);
				while hops.len() <= self.spots.len() {
					if at == home && self.relay(at).is_some_and(|relay| relay.ident == announcer) {
						let done = self
							.served(at, announcer, &broadcast)
							.inspect(|&endpoint| hops.push(endpoint));
						return Some((hops, done.is_some()));
					}
					let next = self.relay(at).and_then(|relay| match marked {
						false => relay.table.get(&announcer).map(|&(_, next)| next),
						true => Self::next_hops(relay, true, &mut 0)
							.get(&announcer)
							.map(|&(_, next)| next),
					});
					let Some(next) = next else {
						return Some((hops, false));
					};
					marked |= self.marks(next, at);
					hops.push(next);
					at = next;
				}
				Some((hops, false))
			}
			Layer::Flat => {
				let Some(mut hops) = self.underlay(from, home) else {
					return Some((Vec::new(), false));
				};
				let done = self
					.served(home, announcer, &broadcast)
					.inspect(|&endpoint| hops.push(endpoint));
				Some((hops, done.is_some()))
			}
		}
	}

	/// The nodes whose sessions carry a request from `from` for `id`, the publisher last,
	/// and whether it got there: for link state every relay on the way, for flat only
	/// the announcer.
	fn pulls(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)> {
		let broadcast = (id, p(&Mesh::claim(0)));
		let (announcer, _) = self.pick(from, &broadcast)?;
		match self.layer {
			Layer::LinkState => self.chase(from, id),
			Layer::Flat => {
				let home = node_of(announcer);
				let mut hops = if home == from { Vec::new() } else { vec![home] };
				let done = self
					.served(home, announcer, &broadcast)
					.inspect(|&endpoint| hops.push(endpoint));
				Some((hops, done.is_some()))
			}
		}
	}

	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		let shown = self.relay(node)?.shown.get(&(id, p(claim)))?;
		Some(shown.available)
	}

	fn sent(&self) -> Sent {
		let (offline, unavailable, starts) = tally(self.events.iter().map(|event| event.kind));
		Sent {
			msgs: self.stats.route_msgs,
			items: self.stats.route_items,
			claims: self.stats.claim_items,
			offline,
			unavailable,
			starts,
		}
	}

	fn held(&self) -> Held {
		let alive: BTreeSet<Id> = self
			.relays()
			.into_iter()
			.map(|node| self.relay(node).expect("a relay").ident)
			.collect();
		let mut held = Held::default();
		for node in self.relays() {
			let relay = self.relay(node).expect("a relay");
			held.routes += relay.table.len() + relay.sessions.len();
			held.offers += relay.lsdb.len();
			held.claims += relay.claims.len();
			// What a link-state database keeps of relays that restarted, until it ages out.
			held.lasting += relay.lsdb.keys().filter(|ident| !alive.contains(ident)).count();
		}
		held
	}

	fn faults(&self) -> Faults {
		Faults {
			false_ends: multi::false_ends(&self.events),
			revivals: multi::revivals(&self.events),
		}
	}

	fn work(&self) -> usize {
		self.spent + self.stats.delivered
	}
}

/// The arms compared here, the shipped router first.
pub(super) const LAYERS: [Arm; 3] = [Arm::RELAY, Arm::Layer(Layer::LinkState), Arm::Layer(Layer::Flat)];

/// The bake-off's shapes that stay inside one domain.
pub(super) fn domains(relays: usize, publishers: usize) -> Vec<Shape> {
	(shapes(relays, publishers).into_iter())
		.filter(|shape| !shape.mesh.ties.values().any(|&tie| tie == Tie::Scoped))
		.collect()
}

/// What carrying the first publisher to every relay costs once settled, every relay
/// with a reader: each relay pulls once from the first node on its way that carries
/// it, and a pull crosses the network's cheapest path between the two. The copies the
/// busiest link carries, and the link crossings summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Copies {
	pub busiest: usize,
	pub total: usize,
}

/// The network's cheapest path between two relays over open links in `lab`, by the
/// links' costs and honouring their marks as [`LayerSim::underlay`] does: its cost and
/// links.
fn path(lab: &Lab, from: Node, to: Node) -> (Metric, Vec<(Node, Node)>) {
	let costs: BTreeMap<(Node, Node), Metric> = (lab.mesh.links.iter())
		.map(|&(a, b, cost)| (pair(a, b), cost))
		.collect();
	let edges = |at: Id| {
		(lab.links_of(at as Node).into_iter())
			.filter(|&peer| peer < lab.mesh.relays)
			.map(|peer| (peer as Id, costs.get(&pair(at as Node, peer)).copied().unwrap_or(1)))
			.collect()
	};
	let marks = |x: Id, y: Id| lab.mesh.ties.get(&(x as Node, y as Node)) == Some(&Tie::Up);
	let mut spent = 0;
	let found = cheapest(from as Id, false, &edges, &marks, &|_| false, &|_| true, &mut spent);
	let Some(((_, cost, _), nodes)) = found.get(&(to as Id)) else {
		return (0, Vec::new());
	};
	let mut links = Vec::new();
	let mut at = from;
	for &next in nodes {
		links.push(pair(at, next as Node));
		at = next as Node;
	}
	(*cost, links)
}

/// Copies per link for `pulls`, each relay from its source: over the link between them
/// where the arm routes hop by hop, or the network's path where a session spans it.
fn tally_copies(lab: &Lab, pulls: &BTreeSet<(Node, Node)>, spans: bool) -> Copies {
	let mut load: BTreeMap<(Node, Node), usize> = BTreeMap::new();
	for &(source, relay) in pulls {
		let links = match spans {
			true => path(lab, source, relay).1,
			false => vec![pair(source, relay)],
		};
		for link in links {
			*load.entry(link).or_default() += 1;
		}
	}
	Copies {
		busiest: load.values().copied().max().unwrap_or(0),
		total: load.values().sum(),
	}
}

/// [`Copies`] as `lab`'s arm pulls: each relay from the first node its pull names,
/// which is a neighbour unless the arm's sessions `span` the network.
pub(super) fn copies(lab: &Lab, spans: bool) -> Copies {
	let (_, id) = lab.mesh.publisher(0);
	let mut pulls = BTreeSet::new();
	for relay in 0..lab.mesh.relays {
		// The last hop is the publisher: a relay pulling from it alone is its home.
		if let Some((hops, true)) = lab.net.pulls(relay, id)
			&& hops.len() >= 2
		{
			pulls.insert((hops[0], relay));
		}
	}
	tally_copies(lab, &pulls, spans)
}

/// [`Copies`] for flat if every relay pulled from the cheapest relay already carrying
/// that is nearer the announcer, in order of distance: a bound on what warm routes
/// could recover over a flat domain, not a protocol.
pub(super) fn warm_bound(lab: &Lab) -> Copies {
	let home = lab.mesh.homes[0];
	let dist = |a: Node, b: Node| path(lab, a, b).0;
	let mut order: Vec<Node> = (0..lab.mesh.relays).filter(|&relay| relay != home).collect();
	order.sort_by_key(|&relay| (dist(relay, home), relay));
	let mut carrying = vec![home];
	let mut pulls = BTreeSet::new();
	for relay in order {
		let source = (carrying.iter().copied())
			.filter(|&c| dist(c, home) < dist(relay, home))
			.min_by_key(|&c| (dist(relay, c), c))
			.unwrap_or(home);
		pulls.insert((source, relay));
		carrying.push(relay);
	}
	tally_copies(lab, &pulls, true)
}

/// Every failure on every single-domain shape: each layer settles, every relay with a
/// path routes to its publisher, no reader sees an end the publisher never made or a
/// life it had seen end, and nothing waits on another node once it settles.
#[test]
fn the_layers_carry_every_failure_on_every_shape() {
	for layer in [Layer::LinkState, Layer::Flat] {
		for shape in domains(9, 2) {
			for (failure, row) in fail(Arm::Layer(layer), &shape.mesh, false) {
				let case = format!("{layer:?}: {} on {}", failure.as_str(), shape.name);
				assert_eq!(row.right, Ok(()), "{case}");
				assert_eq!(row.faults, Faults::default(), "{case}");
				assert_eq!(row.pending, 0, "{case}");
			}
		}
	}
}

/// A drained path stays beside a clean one to the same relay: past a draining relay
/// both are drained, and the cheaper drained one wins.
#[test]
fn cheapest_keeps_a_cheaper_drained_path() {
	let (source, a, d, target) = (0, 1, 2, 3);
	let links: BTreeMap<Id, Vec<(Id, Metric)>> = BTreeMap::from([
		(source, vec![(a, 10), (d, 1)]),
		(d, vec![(source, 1), (a, 1)]),
		(a, vec![(source, 10), (d, 1), (target, 1)]),
		(target, vec![(a, 1)]),
	]);
	let edges = |at: Id| links[&at].clone();
	let draining = |at: Id| at == a || at == d;
	let found = cheapest(source, false, &edges, &|_, _| false, &draining, &|_| true, &mut 0);
	assert_eq!(found[&a], ((false, 10, a as Node), vec![a]));
	assert_eq!(found[&target], ((true, 3, d as Node), vec![d, a, target]));
}

/// A link that closes while the network still joins both ends costs a flat domain
/// nothing at all, where link state floods it to every relay and the path vector
/// moves routes.
#[test]
fn a_flat_domain_rides_out_a_link_closing() {
	// Route messages and unfed relay-rounds for the busiest tree link closing, summed
	// over every single-domain shape.
	let close = |arm: Arm| {
		let mut sum = (0, 0);
		for shape in domains(9, 2) {
			let mut lab = Lab::new(arm, &shape.mesh);
			let Some((a, b)) = lab.tree_link(0, true, &shape.mesh.tree()) else {
				continue;
			};
			let cost = lab.measure(&|lab| lab.cut(a, b));
			sum.0 += cost.sent.msgs;
			sum.1 += cost.stale + cost.dark;
		}
		sum
	};
	assert_eq!(close(Arm::Layer(Layer::Flat)), (0, 0));
	let (ls, _) = close(Arm::Layer(Layer::LinkState));
	let (relay, _) = close(Arm::RELAY);
	assert!(ls > 0 && relay > 0, "link state {ls}, relay {relay}");
}

/// A link-state request keeps the sticky rule at every hop, not only where it starts:
/// X marks R and H, so R's path to H goes round by Y, and X forwards it that way
/// rather than by its own cheapest link to H.
#[test]
fn link_state_forwards_by_the_marks_a_request_arrived_with() {
	let (r, x, y, h) = (0, 1, 2, 3);
	let mesh = Mesh {
		relays: 4,
		links: vec![(r, x, 1), (x, h, 1), (x, y, 5), (y, h, 5)],
		ties: BTreeMap::from([((x, r), Tie::Up), ((x, h), Tie::Up)]),
		homes: vec![h],
		claims: 1,
	};
	let lab = Lab::new(Arm::Layer(Layer::LinkState), &mesh);
	let (publisher, id) = mesh.publisher(0);
	assert_eq!(lab.net.chase(r, id), Some((vec![x, y, h, publisher], true)));
	assert_eq!(lab.net.chase(x, id), Some((vec![h, publisher], true)));
}

/// A flat home that restarts before the feed's news of its death arrives still ends
/// with a session to every relay: the old identity's removal touches no newer
/// session, and a session one end alone holds reopens.
#[test]
fn a_flat_home_restarting_reopens_every_session() {
	let mesh = super::bakeoff::tiers(3, 3, true, 1);
	let mut lab = Lab::new(Arm::Layer(Layer::Flat), &mesh);
	lab.restart(mesh.homes[0]);
	assert!(lab.settle());
	assert_eq!(lab.check(), Ok(()));
}

/// A flat domain without warm routes pulls every relay's copy from the announcer, so
/// the links beside it carry one copy per relay, where a next-hop layer carries one.
#[test]
fn a_flat_domain_needs_warm_routes_to_fan_out() {
	let mesh = super::bakeoff::tiers(3, 3, true, 1);
	let lab = Lab::new(Arm::Layer(Layer::Flat), &mesh);
	let flat = copies(&lab, true);
	let ls = copies(&Lab::new(Arm::Layer(Layer::LinkState), &mesh), false);
	assert_eq!(ls.busiest, 1, "{ls:?}");
	assert!(flat.busiest > 3 * ls.busiest, "flat {flat:?} against link state {ls:?}");
	assert!(warm_bound(&lab).busiest < flat.busiest);
}

/// The tables in the PR body: every failure on every single-domain shape, for the
/// shipped router and both layers, then what settled routes cost to carry.
#[test]
#[ignore = "a sweep; prints the node layer's tables"]
fn benchmark_node_layer() {
	let sizes: Vec<usize> = std::env::var("MOQ_BAKEOFF_RELAYS").map_or(vec![12, 32], |sizes| {
		sizes
			.split(',')
			.map(|size| size.parse().expect("MOQ_BAKEOFF_RELAYS"))
			.collect()
	});
	println!(
		"shape,relays,links,failure,rule,route_messages,claim_items,settle_ms,fed_ms,unfed,loops,offline,unavailable,false_ends,revivals,lasting,hops,right"
	);
	for &relays in &sizes {
		for shape in domains(relays, 2) {
			for arm in LAYERS {
				for (failure, row) in fail(arm, &shape.mesh, relays > 16) {
					print_row(&shape, arm, failure, &row);
				}
			}
		}
	}
	println!(
		"shape,relays,rule,build_route_messages,build_claim_items,held_routes,held_offers,busiest_copies,total_copies"
	);
	for &relays in &sizes {
		for shape in domains(relays, 1) {
			for arm in LAYERS {
				let lab = Lab::new(arm, &shape.mesh);
				let (sent, held) = (lab.net.sent(), lab.net.held());
				let carry = copies(&lab, arm == Arm::Layer(Layer::Flat));
				println!(
					"{},{},{},{},{},{},{},{},{}",
					shape.name,
					shape.mesh.relays,
					arm.name(),
					sent.msgs,
					sent.claims,
					held.routes,
					held.offers,
					carry.busiest,
					carry.total
				);
				if arm == Arm::Layer(Layer::Flat) {
					let bound = warm_bound(&lab);
					println!(
						"{},{},flat-warm-bound,,,,,{},{}",
						shape.name, shape.mesh.relays, bound.busiest, bound.total
					);
				}
			}
		}
	}
}

/// What a domain costs to build and hold, and what one link closing and one transit
/// dying cost, as relays, announcing relays, and broadcasts grow.
#[test]
#[ignore = "a sweep; prints the node layer's growth table"]
fn benchmark_node_layer_growth() {
	println!(
		"grows,relays,links,publishers,announcers,broadcasts,rule,build_route_messages,build_claim_items,held_routes,held_claims,close_route_messages,death_route_messages,death_claim_items,death_unfed"
	);
	let tiers_of = |relays: usize, publishers: usize, claims: usize| {
		let (regions, edges) = match relays {
			..24 => (3, 3),
			24..48 => (4, 6),
			_ => (6, 8),
		};
		Mesh {
			claims,
			..super::bakeoff::tiers(regions, edges, true, publishers)
		}
	};
	let mut cases = Vec::new();
	for relays in [15, 32, 60] {
		for publishers in [1, 8, 32] {
			cases.push(("relays_x_publishers", tiers_of(relays, publishers, 1)));
		}
	}
	for claims in [1, 8, 64] {
		cases.push(("broadcasts", tiers_of(32, 8, claims)));
	}
	for (grows, mesh) in cases {
		for arm in LAYERS {
			let tree = mesh.tree();
			let mut lab = Lab::new(arm, &mesh);
			lab.coarse = true;
			let (built, held) = (lab.net.sent(), lab.net.held());
			let close = lab.tree_link(0, true, &tree).map(|(a, b)| {
				let cost = lab.measure(&|lab| lab.cut(a, b));
				lab.mend(a, b);
				assert!(lab.settle());
				cost.sent.msgs
			});
			let death = super::bakeoff::transit(&mesh, &tree).map(|relay| lab.measure(&|lab| lab.crash(relay)));
			println!(
				"{grows},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
				mesh.relays,
				mesh.links.len(),
				mesh.homes.len(),
				mesh.homes.iter().collect::<BTreeSet<_>>().len(),
				mesh.homes.len() * mesh.claims,
				arm.name(),
				built.msgs,
				built.claims,
				held.routes,
				held.claims,
				close.unwrap_or(0),
				death.map_or(0, |cost| cost.sent.msgs),
				death.map_or(0, |cost| cost.sent.claims),
				death.map_or(0, |cost| cost.stale + cost.dark),
			);
		}
	}
}

fn print_row(shape: &Shape, arm: Arm, failure: Failure, row: &Row) {
	let ms = |rounds: usize| rounds as f64 * HOP.as_secs_f64() * 1000.0;
	println!(
		"{},{},{},{},{},{},{},{:.0},{:.0},{},{},{},{},{},{},{},{},{}",
		shape.name,
		shape.mesh.relays,
		shape.mesh.links.len(),
		failure.as_str(),
		arm.name(),
		row.cost.sent.msgs,
		row.cost.sent.claims,
		ms(row.cost.rounds),
		ms(row.cost.right),
		row.cost.stale + row.cost.dark,
		row.cost.loops,
		row.cost.sent.offline,
		row.cost.sent.unavailable,
		row.faults.false_ends,
		row.faults.revivals,
		row.lasting,
		row.hops,
		row.right
			.as_ref()
			.map_or_else(|err| err.replace(',', ";"), |()| "yes".into()),
	);
}

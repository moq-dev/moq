//! The routing contract's fixtures, run against [`Router`] in a simulated network.
//!
//! [`Sim`] wires one router per node through ordered, holdable message queues,
//! so a fixture can deliver messages out of step and replay them late. Claims
//! are written with a trailing `/` where the contract means a wildcard; every
//! lite-07 claim already covers the paths below it, so the slash is dropped.

mod handoff;
mod routing;

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
};

use super::*;

pub(super) type Node = usize;

/// A claim's attributes, plus the revision its life began at so fixtures can
/// tell a revival from a fresh announcement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Item {
	pub seed: Metric,
	pub epoch: Option<u32>,
	pub since: Rev,
}

/// A contract path: a trailing `/` (a wildcard in the contract) is dropped.
pub(super) fn p(path: &str) -> Path {
	Path::new(path.strip_suffix('/').unwrap_or(path)).expect("a valid path")
}

/// A scoped publish grant: the credential subject that qualifies identities,
/// and the prefix standing in for the grant's pattern union.
#[derive(Clone, Copy, Debug)]
pub(super) struct Grant {
	pub subject: &'static str,
	pub scope: &'static str,
}

/// A qualified key, as a stand-in derivation keyed by the grant's subject.
pub(super) fn qualified(subject: &str, id: Id) -> Id {
	fnv(subject, &[id]) | QUALIFIED
}

fn fnv(label: &str, ids: &[u64]) -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for byte in label.bytes().chain(ids.iter().flat_map(|id| id.to_le_bytes())) {
		hash ^= u64::from(byte);
		hash = hash.wrapping_mul(0x0100_0000_01b3);
	}
	hash
}

/// One side's configuration of a link.
#[derive(Clone, Copy, Debug)]
pub(super) struct SimLink {
	pub cost: Metric,
	pub priority: i8,
	pub upstream: bool,
	pub grant: Option<Grant>,
}

impl SimLink {
	pub fn cost(cost: Metric) -> Self {
		Self {
			cost,
			priority: 0,
			upstream: false,
			grant: None,
		}
	}

	pub fn priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	pub fn upstream(mut self) -> Self {
		self.upstream = true;
		self
	}

	pub fn grant(mut self, grant: Grant) -> Self {
		self.grant = Some(grant);
		self
	}
}

#[derive(Clone, Debug)]
enum Msg {
	Route {
		id: Id,
		reach: Option<Reach>,
	},
	Batch {
		id: Id,
		changes: Vec<(Path, Option<Item>)>,
		floors: BTreeMap<Path, Rev>,
		interest: u32,
	},
	Request {
		id: Id,
		seqno: Seqno,
	},
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SimEvent {
	pub node: Node,
	pub id: Id,
	pub claim: String,
	pub kind: Kind,
	pub since: Rev,
	/// Restarts of the node, then how many times it forgot this claim.
	pub forgot: (u32, u32),
	/// Whether the origin had withdrawn or replaced this life of the claim by then.
	pub withdrawn: bool,
}

/// Messages sent, as protocol counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stats {
	pub route_msgs: usize,
	pub claim_msgs: usize,
	pub claim_items: usize,
	pub requests: usize,
	/// Requests a node sent the same neighbor again for the same seqno.
	pub repeated_requests: usize,
	/// Messages and echoes delivered.
	pub delivered: usize,
}

struct Change {
	rev: Rev,
	claim: Path,
	item: Option<Item>,
}

pub(super) struct Sim {
	nodes: Vec<Router<Node, Item>>,
	leaf: Vec<bool>,
	watch: Vec<Path>,
	restarts: Vec<u32>,
	/// The identity each origin node declared last, to spot a rotation.
	declared: Vec<Option<Id>>,
	links: Vec<Vec<Option<SimLink>>>,
	grants: BTreeSet<&'static str>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Msg>>,
	/// Echoed ROUTE_ENDs: unordered against the echoing node's own routes.
	acks: BTreeMap<(Node, Node), VecDeque<Id>>,
	held: BTreeSet<(Node, Node)>,
	history: BTreeMap<Id, Vec<Change>>,
	/// Every request sent, to spot a repeat.
	requested: BTreeSet<(Node, Node, Id, Seqno)>,
	pub stats: Stats,
	pub events: Vec<SimEvent>,
}

impl Sim {
	pub fn new(nodes: usize) -> Self {
		Self {
			nodes: (0..nodes).map(|node| Router::new(node as u64 + 1)).collect(),
			leaf: vec![false; nodes],
			watch: vec![Path::empty(); nodes],
			restarts: vec![0; nodes],
			declared: vec![None; nodes],
			links: vec![vec![None; nodes]; nodes],
			grants: BTreeSet::new(),
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			acks: BTreeMap::new(),
			held: BTreeSet::new(),
			history: BTreeMap::new(),
			requested: BTreeSet::new(),
			stats: Stats::default(),
			events: Vec::new(),
		}
	}

	/// A node that sends only identities it serves, like a browser.
	pub fn leaf(&mut self, node: Node) {
		self.leaf[node] = true;
	}

	/// Configure both sides of a link and open its session.
	pub fn link(&mut self, a: Node, b: Node, ab: SimLink, ba: SimLink) {
		self.links[a][b] = Some(ab);
		self.links[b][a] = Some(ba);
		for grant in [ab.grant, ba.grant].into_iter().flatten() {
			self.grants.insert(grant.subject);
		}
		self.connect(a, b);
	}

	/// A symmetric link of `cost`.
	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.link(a, b, SimLink::cost(cost), SimLink::cost(cost));
	}

	fn pair(a: Node, b: Node) -> (Node, Node) {
		(a.min(b), a.max(b))
	}

	fn config(&self, node: Node, peer: Node) -> Link {
		let side = self.links[node][peer].expect("a configured link");
		Link {
			cost: side.cost,
			priority: side.priority,
			upstream: side.upstream,
			proxy: !self.leaf[node],
			scope: side.grant.map(|grant| Scope {
				regions: vec![p(grant.scope)],
				qualify: Arc::new(move |id| qualified(grant.subject, id)),
			}),
		}
	}

	/// Open a session: each side opens a stream asking for its interest.
	pub fn connect(&mut self, a: Node, b: Node) {
		assert!(!self.connected(a, b), "{a}-{b} already up");
		self.up.insert(Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			let link = self.config(x, y);
			self.nodes[x].connect(y, link).expect("a positive cost");
		}
		for (x, y) in [(a, b), (b, a)] {
			let watch = self.watch[y].clone();
			self.nodes[x].peer_interest(y, watch).unwrap();
		}
		self.flush();
	}

	pub fn connected(&self, a: Node, b: Node) -> bool {
		self.up.contains(&Self::pair(a, b))
	}

	/// Close a session: in-flight messages and everything learned on it go.
	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.up.remove(&Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.acks.remove(&(x, y));
			self.held.remove(&(x, y));
			self.nodes[y].disconnect(x);
		}
		self.flush();
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn kill(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.disconnect(node, peer);
		}
		self.restarts[node] += 1;
		self.declared[node] = None;
		self.nodes[node] = Router::new(fnv("restart", &[node as u64, u64::from(self.restarts[node])]));
		let watch = self.watch[node].clone();
		self.nodes[node].watch(watch);
		self.flush();
	}

	/// Re-open every configured link of `node`.
	pub fn reconnect(&mut self, node: Node) {
		let peers: Vec<Node> = (0..self.nodes.len())
			.filter(|&peer| self.links[node][peer].is_some() && !self.connected(node, peer))
			.collect();
		for peer in peers {
			self.connect(node, peer);
		}
	}

	fn peers(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
		(0..self.nodes.len()).filter(move |&peer| self.links[node][peer].is_some() && self.connected(node, peer))
	}

	pub fn originate(&mut self, node: Node, id: Id, seqno: Seqno) {
		self.nodes[node].originate(id, seqno);
		self.declared[node] = Some(id);
		self.history.entry(id).or_default();
		self.flush();
	}

	pub fn forge(&mut self, node: Node, id: Id, seqno: Seqno) {
		self.nodes[node].forge(id, seqno);
		self.flush();
	}

	pub fn bump(&mut self, node: Node) {
		self.nodes[node].bump().unwrap();
		self.flush();
	}

	pub fn age(&mut self, node: Node, rev: Rev) {
		self.nodes[node].age(rev);
		self.flush();
	}

	/// The identity `node` declares.
	pub fn proven(&self, node: Node) -> Id {
		self.nodes[node].declared().expect("not an origin")
	}

	pub fn seqno(&self, node: Node) -> Seqno {
		self.nodes[node].seqno().expect("not an origin")
	}

	pub fn announce(&mut self, node: Node, claim: &str, seed: Metric, epoch: Option<u32>) {
		let claim = p(claim);
		let router = &self.nodes[node];
		let id = router.declared().expect("not an origin");
		let rev = router.revision().unwrap() + 1;
		let since = router
			.view(id)
			.and_then(|view| view.items.get(&claim))
			.map_or(rev, |item| item.since);
		let item = Item { seed, epoch, since };
		self.nodes[node].announce(claim.clone(), item).unwrap();
		self.record(node, claim, Some(item));
	}

	pub fn unannounce(&mut self, node: Node, claim: &str) {
		let claim = p(claim);
		self.nodes[node].unannounce(claim.clone()).unwrap();
		self.record(node, claim, None);
	}

	fn record(&mut self, node: Node, claim: Path, item: Option<Item>) {
		let router = &self.nodes[node];
		let id = router.declared().unwrap();
		let rev = router.revision().unwrap();
		if self.declared[node] == Some(id) {
			self.history.entry(id).or_default().push(Change { rev, claim, item });
		}
		self.flush();
	}

	/// The prefix `node` asks its neighbors for announcements under; each
	/// neighbor answers on a new stream with a complete snapshot.
	pub fn watch(&mut self, node: Node, prefix: &str) {
		let prefix = p(prefix);
		self.watch[node] = prefix.clone();
		self.nodes[node].watch(prefix.clone());
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.nodes[peer].peer_interest(node, prefix.clone()).unwrap();
		}
		self.flush();
	}

	pub fn watching(&self, node: Node) -> &Path {
		&self.watch[node]
	}

	pub fn goaway(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.nodes[peer].goaway(node);
		}
		self.flush();
	}

	pub fn collect(&mut self, node: Node, id: Id) {
		self.nodes[node].collect(id);
		self.flush();
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
	}

	/// Deliver one in-order message from `from` to `to`, even if held.
	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(msg) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		let router = &mut self.nodes[to];
		match msg {
			Msg::Route { id, reach } => router.route(from, id, reach),
			Msg::Batch {
				id,
				changes,
				floors,
				interest,
			} => router.batch(from, id, changes, floors, interest),
			Msg::Request { id, seqno } => router.request(from, id, seqno),
		}
		self.stats.delivered += 1;
		self.flush();
		true
	}

	/// Deliver one echoed ROUTE_END from `from` to `to`, even if held.
	pub fn deliver_ack(&mut self, from: Node, to: Node) -> bool {
		let Some(id) = self.acks.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		self.nodes[to].echo(from, id);
		self.stats.delivered += 1;
		self.flush();
		true
	}

	pub fn queued(&self, from: Node, to: Node) -> usize {
		self.queues.get(&(from, to)).map_or(0, VecDeque::len)
	}

	pub fn queued_acks(&self, from: Node, to: Node) -> usize {
		self.acks.get(&(from, to)).map_or(0, VecDeque::len)
	}

	pub fn busy(&self) -> Vec<(Node, Node)> {
		Self::nonempty(&self.queues)
	}

	pub fn busy_acks(&self) -> Vec<(Node, Node)> {
		Self::nonempty(&self.acks)
	}

	fn nonempty<T>(queues: &BTreeMap<(Node, Node), VecDeque<T>>) -> Vec<(Node, Node)> {
		queues
			.iter()
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(key, _)| *key)
			.collect()
	}

	/// Deliver round-robin until every unheld queue is empty. False if it never
	/// settles within a generous budget, which is how oscillation shows up.
	pub fn run(&mut self) -> bool {
		self.settle(10_000)
	}

	pub fn settle(&mut self, rounds: usize) -> bool {
		for _ in 0..rounds {
			let ready: Vec<(Node, Node)> = self.busy().into_iter().filter(|key| !self.held.contains(key)).collect();
			let acks: Vec<(Node, Node)> = self
				.busy_acks()
				.into_iter()
				.filter(|key| !self.held.contains(key))
				.collect();
			if ready.is_empty() && acks.is_empty() {
				return true;
			}
			for (from, to) in ready {
				self.deliver(from, to);
			}
			for (from, to) in acks {
				self.deliver_ack(from, to);
			}
		}
		false
	}

	/// Move every router's output onto the queues and log its reader events.
	fn flush(&mut self) {
		for node in 0..self.nodes.len() {
			for out in self.nodes[node].drain() {
				let (to, msg) = match out {
					Out::Route { to, id, reach } => {
						self.stats.route_msgs += 1;
						(to, Msg::Route { id, reach })
					}
					Out::Batch {
						to,
						id,
						changes,
						floors,
					} => {
						self.stats.claim_msgs += 1;
						self.stats.claim_items += changes.len();
						let interest = self.nodes[to].interest();
						(
							to,
							Msg::Batch {
								id,
								changes,
								floors,
								interest,
							},
						)
					}
					Out::Request { to, id, seqno } => {
						self.stats.requests += 1;
						if !self.requested.insert((node, to, id, seqno)) {
							self.stats.repeated_requests += 1;
						}
						(to, Msg::Request { id, seqno })
					}
					Out::Echo { to, id } => {
						self.stats.route_msgs += 1;
						self.acks.entry((node, to)).or_default().push_back(id);
						continue;
					}
				};
				self.queues.entry((node, to)).or_default().push_back(msg);
			}
			// A rotated origin starts a fresh history from the claims it carried.
			let declared = self.nodes[node].declared();
			if self.declared[node].is_some() && declared != self.declared[node] {
				let id = declared.expect("rotated");
				let router = &self.nodes[node];
				let rev = router.revision().unwrap();
				let changes = router
					.view(id)
					.unwrap()
					.items
					.iter()
					.map(|(claim, item)| Change {
						rev,
						claim: claim.clone(),
						item: Some(*item),
					})
					.collect();
				self.history.insert(id, changes);
				self.declared[node] = declared;
			}
			for event in self.nodes[node].events() {
				let origin = self.source(event.id).unwrap_or(event.id);
				let since = event.item.since;
				let withdrawn = self
					.truth(origin, event.claim.as_str(), self.revision(origin))
					.is_none_or(|item| item.since != since);
				self.events.push(SimEvent {
					node,
					id: event.id,
					claim: event.claim.as_str().to_string(),
					kind: event.kind,
					since,
					forgot: (self.restarts[node], event.forgot),
					withdrawn,
				});
			}
		}
	}

	/// Identities recomputed across every node.
	pub fn touched(&self) -> usize {
		self.nodes.iter().map(Router::touched).sum()
	}

	pub fn route(&self, node: Node, id: Id) -> Option<Selected<Node>> {
		self.nodes[node].selected(id)
	}

	pub fn export_route(&self, node: Node, id: Id) -> Option<Selected<Node>> {
		self.nodes[node].export(id)
	}

	pub fn starved(&self, node: Node, id: Id) -> bool {
		self.nodes[node].starved(id)
	}

	pub fn grounded(&self, node: Node, id: Id) -> bool {
		self.nodes[node].grounded(id)
	}

	pub fn holds(&self, node: Node, id: Id) -> bool {
		self.nodes[node].holds(id)
	}

	pub fn remembers(&self, node: Node, id: Id) -> bool {
		self.nodes[node].remembers(id)
	}

	pub fn floors(&self, node: Node, id: Id) -> usize {
		self.nodes[node].floors(id)
	}

	pub fn identities(&self, node: Node) -> usize {
		self.nodes[node].identities()
	}

	pub fn aliases(&self, node: Node) -> usize {
		self.nodes[node].aliases()
	}

	pub fn shown(&self, node: Node) -> &BTreeMap<(Id, Path), Shown<Item>> {
		self.nodes[node].shown()
	}

	pub fn shows(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		self.nodes[node].shows(id, &p(claim))
	}

	/// Retained state summed over every node.
	pub fn retained(&self) -> Retained {
		let mut out = Retained::default();
		for router in &self.nodes {
			let r = router.retained();
			out.items += r.items;
			out.floors += r.floors;
			out.heard += r.heard;
			out.sent += r.sent;
			out.echoes += r.echoes;
			out.pending += r.pending;
			out.distances += r.distances;
			out.certified += r.certified;
		}
		out
	}

	/// The origin's state of `claim` as of revision `rev`.
	pub fn truth(&self, id: Id, claim: &str, rev: Rev) -> Option<Item> {
		let claim = p(claim);
		self.history
			.get(&id)?
			.iter()
			.rev()
			.find(|change| change.rev <= rev && change.claim == claim)
			.and_then(|change| change.item)
	}

	pub fn revision(&self, id: Id) -> Rev {
		self.history
			.get(&id)
			.and_then(|history| history.last())
			.map_or(0, |change| change.rev)
	}

	/// Every claim each node knows must be the origin's state at the revision it
	/// knows that claim at. Returns the views checked.
	pub fn check(&self) -> Result<BTreeSet<(Node, Id)>, String> {
		let mut checked = BTreeSet::new();
		for (node, router) in self.nodes.iter().enumerate() {
			for (key, view) in router.views() {
				let Some(id) = self.source(key) else {
					continue;
				};
				checked.insert((node, key));
				let claims: BTreeSet<&Path> = self.history[&id].iter().map(|change| &change.claim).collect();
				for claim in claims {
					let Some(rev) = view.as_of(claim) else {
						if view.items.contains_key(claim) {
							return Err(format!("node {node} holds {id}'s {claim} with no floor"));
						}
						continue;
					};
					let known = view.items.get(claim).copied();
					let truth = self.truth(id, claim.as_str(), rev);
					if known != truth {
						return Err(format!(
							"node {node} holds {key}'s {claim} as {known:?} at revision {rev}, where the origin had {truth:?}"
						));
					}
				}
			}
		}
		Ok(checked)
	}

	/// The origin identity whose claims `key` carries.
	fn source(&self, key: Id) -> Option<Id> {
		if self.history.contains_key(&key) {
			return Some(key);
		}
		if key & QUALIFIED == 0 {
			return None;
		}
		self.history
			.keys()
			.copied()
			.find(|&inner| self.grants.iter().any(|subject| qualified(subject, inner) == key))
	}
}

pub(super) struct XorShift(pub u64);

impl XorShift {
	pub fn next(&mut self, below: u64) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0 % below
	}

	pub fn chance(&mut self, percent: u64) -> bool {
		self.next(100) < percent
	}
}

pub(super) fn kinds(events: &[SimEvent]) -> Vec<Kind> {
	events.iter().map(|event| event.kind).collect()
}

pub(super) fn at(sim: &Sim, node: Node) -> Vec<SimEvent> {
	sim.events.iter().filter(|event| event.node == node).cloned().collect()
}

/// The kinds `node` saw for `claim`.
pub(super) fn life(sim: &Sim, node: Node, claim: &str) -> Vec<Kind> {
	let claim = p(claim);
	sim.events
		.iter()
		.filter(|event| event.node == node && event.claim == claim.as_str())
		.map(|event| event.kind)
		.collect()
}

/// Starts of a claim's life that the same node had already shown ending.
pub(super) fn revivals(events: &[SimEvent]) -> usize {
	let mut ended = BTreeSet::new();
	let mut count = 0;
	for event in events {
		let life = (event.node, event.forgot, event.id, event.claim.clone(), event.since);
		match event.kind {
			Kind::End => {
				ended.insert(life);
			}
			Kind::Start if ended.contains(&life) => count += 1,
			_ => {}
		}
	}
	count
}

/// Ends shown while the origin still had that life of the claim.
pub(super) fn false_ends(events: &[SimEvent]) -> usize {
	events
		.iter()
		.filter(|event| event.kind == Kind::End && !event.withdrawn)
		.count()
}

/// Every node's next-hop chain toward `id` ends without revisiting a node.
pub(super) fn assert_loop_free(sim: &Sim, nodes: usize, id: Id) {
	for node in 0..nodes {
		let mut at = node;
		for hop in 0.. {
			let Some(next) = sim.route(at, id).and_then(|selected| selected.next) else {
				break;
			};
			assert!(hop <= nodes, "{id} loops from node {node}");
			at = next;
		}
	}
}

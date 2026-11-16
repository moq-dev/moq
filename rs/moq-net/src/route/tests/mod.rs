//! The routing contract's fixtures, run in a simulated network against
//! [`path::Router`], the rule the routes epic's Plan item 27 picked.
//!
//! [`Sim`] wires one router per node through ordered, holdable message queues,
//! so a fixture can deliver messages out of step and replay them late. Each round
//! of deliveries takes [`HOP`] on a mocked clock, and a quiet network jumps to the
//! next doubt timer. Claims are written with a trailing `/` where the contract
//! means a wildcard; every lite-07 claim already covers the paths below it, so
//! the slash is dropped.

mod bakeoff;
mod cache;
mod compare;
mod cost;
mod flap;
mod forward;
mod handoff;
mod interest;
mod multi;
mod multicast;
mod multicast_cost;
mod multicast_search;
mod residual;
mod routing;
mod scope;
mod search;

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
	time::Duration,
};

use kio::clock::Instant;

pub(super) use super::path::{AtOrigin, Doubt, Lift};
use super::*;

pub(super) type Node = usize;

/// How long one round of deliveries takes on the mocked clock.
pub(super) const HOP: Duration = Duration::from_millis(25);

/// Which route rule every node of a [`Sim`] runs: [`path::Router`], with its doubt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
	Path(Doubt),
}

impl Rule {
	/// The path rule as the model checks it.
	pub const PATH: Self = Self::Path(Doubt::DEFAULT);
	/// The comparison rule lifting doubt once no heard route names the node.
	pub const EARLY: Self = Self::Path(Doubt {
		lift: Lift::Unnamed,
		origin: AtOrigin::Blocked,
		..Doubt::DEFAULT
	});
	/// The replaced rule, where doubt also blocks every route to a doubted publisher.
	pub const ORIGIN: Self = Self::Path(Doubt {
		origin: AtOrigin::Blocked,
		..Doubt::DEFAULT
	});
	/// The rejected rule, where doubt never blocks a route to a doubted publisher, even
	/// on a session to it that opened during the doubt.
	pub const SPARED: Self = Self::Path(Doubt {
		origin: AtOrigin::Spared,
		..Doubt::DEFAULT
	});

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Path(Doubt {
				lift: Lift::Unnamed, ..
			}) => "path+early",
			Self::Path(Doubt {
				origin: AtOrigin::Blocked,
				..
			}) => "path+origin",
			Self::Path(Doubt {
				origin: AtOrigin::Spared,
				..
			}) => "path+spared",
			Self::Path(Doubt {
				origin: AtOrigin::Own, ..
			}) => "path",
		}
	}
}

/// Run each fixture named under the path rule, as a test each.
macro_rules! fixtures {
	($($name:ident),* $(,)?) => {
		mod path {
			$(#[test] fn $name() { super::$name(super::Rule::PATH) })*
		}
	};
}
pub(super) use fixtures;

/// A node's router.
type Relay = path::Router<Node, Item>;

fn relay(rule: Rule, hop: Id, seed: u64) -> Relay {
	let Rule::Path(doubt) = rule;
	path::Router::new(hop, seed, doubt)
}

/// A claim's attributes, plus the revision its life began at so fixtures can
/// tell a revival from a fresh announcement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Item {
	pub seed: Metric,
	pub epoch: Option<u32>,
	pub since: Rev,
}

impl crate::route::Item for Item {
	fn lowered(&self, _: u64) -> Self {
		*self
	}
}

/// `peer` replaces everything it asks `router` for with `prefixes`, in one edit.
pub(super) fn reask<A: crate::route::Item>(
	router: &mut path::Router<Node, A>,
	peer: Node,
	prefixes: &[&str],
) -> Result<(), Error> {
	let new: BTreeSet<Path> = prefixes.iter().map(|prefix| p(prefix)).collect();
	let old = router.answers(peer);
	let remove = old.difference(&new).cloned().collect();
	let add = new.difference(&old).cloned().collect();
	router.edit(peer, path::Edit { remove, add })
}

/// A contract path: a trailing `/` (a wildcard in the contract) is dropped.
pub(super) fn p(path: &str) -> Path {
	Path::new(path.strip_suffix('/').unwrap_or(path)).expect("a valid path")
}

/// A path rule offer crossing `hops`, each a node and the session it learned the
/// route on, the publisher first and the sender last.
pub(super) fn along(hops: &[(Id, u64)], metric: Metric) -> path::Offer {
	path::Offer {
		path: (hops.iter())
			.map(|&(node, session)| path::Hop { node, session })
			.collect(),
		draining: false,
		priority: i8::MAX,
		metric,
		down_only: false,
	}
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

/// How `node` names itself in a route's hops, fresh for each restart like its process.
pub(super) fn hop(node: Node, restarts: u32) -> Id {
	1_000 * (u64::from(restarts) + 1) + node as u64
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
	/// A route.
	Path {
		id: Id,
		prefix: Path,
		reach: Option<path::Reach>,
	},
	/// A route's claims.
	Claims {
		id: Id,
		prefix: Path,
		changes: Vec<(Path, Option<Item>)>,
		floor: Option<Rev>,
	},
	Dead(path::Dead),
	/// An edit of what the sender asks the receiver for.
	Ask(path::Edit),
	/// The marker where the receiver's oldest edit in flight takes effect.
	Edited,
	Synced {
		prefix: Path,
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

/// What the routers retain, summed over every node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct State {
	/// Claims in views of other publishers.
	pub items: usize,
	/// One per view under the path rule.
	pub floors: usize,
	/// Claims on routes heard.
	pub heard: usize,
	/// Claims on routes sent.
	pub sent: usize,
	/// What waits on another node: dead hops and doubts.
	pub pending: usize,
}

/// Messages sent, as protocol counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stats {
	/// Route messages: ROUTE and DEAD.
	pub route_msgs: usize,
	/// Routes those messages name: a DEAD counts itself and each route it moved.
	pub route_items: usize,
	/// Route messages that changed nothing but the hops: what naming them costs.
	pub hop_updates: usize,
	pub claim_msgs: usize,
	pub claim_items: usize,
	/// Messages delivered.
	pub delivered: usize,
}

#[derive(Clone, Debug)]
struct Change {
	rev: Rev,
	claim: Path,
	item: Option<Item>,
}

#[derive(Clone)]
pub(super) struct Sim {
	rule: Rule,
	nodes: Vec<Relay>,
	now: Instant,
	leaf: Vec<bool>,
	watch: Vec<BTreeSet<Path>>,
	restarts: Vec<u32>,
	/// The identity each origin node declared last, to spot a rotation.
	declared: Vec<Option<Id>>,
	links: Vec<Vec<Option<SimLink>>>,
	grants: BTreeSet<&'static str>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Msg>>,
	held: BTreeSet<(Node, Node)>,
	history: BTreeMap<Id, Vec<Change>>,
	/// The last finite route each node sent each neighbor, by prefix, to spot a change
	/// of hops alone.
	offered: BTreeMap<(Node, Node, Id, Path), path::Offer>,
	pub stats: Stats,
	/// The work of routers a restart replaced.
	spent: usize,
	pub events: Vec<SimEvent>,
}

impl Sim {
	/// `nodes` nodes running the path rule.
	pub fn new(nodes: usize) -> Self {
		Self::with(Rule::PATH, nodes)
	}

	/// `nodes` nodes running `rule`.
	pub fn with(rule: Rule, nodes: usize) -> Self {
		Self {
			rule,
			nodes: (0..nodes)
				.map(|node| relay(rule, hop(node, 0), node as u64 + 1))
				.collect(),
			now: Instant::ZERO,
			leaf: vec![false; nodes],
			watch: vec![BTreeSet::from([Path::empty()]); nodes],
			restarts: vec![0; nodes],
			declared: vec![None; nodes],
			links: vec![vec![None; nodes]; nodes],
			grants: BTreeSet::new(),
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			held: BTreeSet::new(),
			history: BTreeMap::new(),
			offered: BTreeMap::new(),
			stats: Stats::default(),
			spent: 0,
			events: Vec::new(),
		}
	}

	/// `node`'s router.
	pub fn path(&self, node: Node) -> &path::Router<Node, Item> {
		&self.nodes[node]
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
			// One session to a peer at a time.
			..Link::default()
		}
	}

	/// Open a session: each side asks for its interest by an edit it sends itself.
	pub fn connect(&mut self, a: Node, b: Node) {
		assert!(!self.connected(a, b), "{a}-{b} already up");
		self.up.insert(Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			let link = self.config(x, y);
			let named = hop(y, self.restarts[y]);
			self.nodes[x].connect(y, named, link).expect("a positive cost");
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
			self.held.remove(&(x, y));
			self.nodes[y].disconnect([x], self.now);
		}
		self.flush();
	}

	/// Every session of `node` closes at once, as a node shutting down closes them.
	pub fn isolate(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for &peer in &peers {
			self.up.remove(&Self::pair(node, peer));
			for key in [(node, peer), (peer, node)] {
				self.queues.remove(&key);
				self.held.remove(&key);
			}
			self.nodes[peer].disconnect([node], self.now);
		}
		self.nodes[node].disconnect(peers, self.now);
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
		let restarts = self.restarts[node];
		let seed = fnv("restart", &[node as u64, u64::from(restarts)]);
		self.spent += self.nodes[node].work();
		self.nodes[node] = relay(self.rule, hop(node, restarts), seed);
		let watch = self.watch[node].clone();
		self.nodes[node].watch(watch).expect("no prefix inside another");
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

	/// `node` starts originating `id`.
	pub fn originate(&mut self, node: Node, id: Id) {
		self.nodes[node].originate(id);
		self.declared[node] = Some(id);
		self.history.entry(id).or_default();
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

	pub fn announce(&mut self, node: Node, claim: &str, seed: Metric, epoch: Option<u32>) {
		let claim = p(claim);
		let router = &self.nodes[node];
		let id = router.declared().expect("not an origin");
		let rev = router.revision().unwrap() + 1;
		let since = (router.view(id, &Path::empty()))
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

	/// The prefix `node` asks its neighbors for announcements under: the node edits
	/// what it asks each neighbour for.
	pub fn watch(&mut self, node: Node, prefix: &str) {
		self.watch_all(node, &[prefix]);
	}

	/// `node`'s interest becomes `prefixes`, none inside another.
	pub fn watch_all(&mut self, node: Node, prefixes: &[&str]) {
		let set: BTreeSet<Path> = prefixes.iter().map(|prefix| p(prefix)).collect();
		self.watch[node] = set.clone();
		self.nodes[node].watch(set).expect("no prefix inside another");
		self.flush();
	}

	/// The one prefix `node` watches.
	pub fn watching(&self, node: Node) -> &Path {
		let mut prefixes = self.watch[node].iter();
		let prefix = prefixes.next().expect("a watched prefix");
		assert!(prefixes.next().is_none(), "node {node} watches several prefixes");
		prefix
	}

	/// Every prefix `node` watches.
	pub fn interest(&self, node: Node) -> &BTreeSet<Path> {
		&self.watch[node]
	}

	pub fn goaway(&mut self, node: Node) {
		let now = self.now;
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.nodes[peer].goaway(node, now);
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
		let now = self.now;
		let router = &mut self.nodes[to];
		router.clock(now);
		match msg {
			Msg::Path { id, prefix, reach } => router.route(from, id, prefix, reach),
			Msg::Claims {
				id,
				prefix,
				changes,
				floor,
			} => router.batch(from, id, prefix, changes, floor),
			Msg::Dead(dead) => router.dead(from, dead, now),
			// A router sends only edits it may: a refused one would close the session.
			Msg::Ask(edit) => router.edit(from, edit).expect("a valid edit"),
			Msg::Edited => router.edited(from),
			Msg::Synced { prefix } => router.synced(from, prefix),
		}
		self.stats.delivered += 1;
		self.flush();
		true
	}

	pub fn queued(&self, from: Node, to: Node) -> usize {
		self.queues.get(&(from, to)).map_or(0, VecDeque::len)
	}

	pub fn busy(&self) -> Vec<(Node, Node)> {
		Self::nonempty(&self.queues)
	}

	fn nonempty<T>(queues: &BTreeMap<(Node, Node), VecDeque<T>>) -> Vec<(Node, Node)> {
		queues
			.iter()
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(key, _)| *key)
			.collect()
	}

	/// Deliver round-robin until every unheld queue is empty and no doubt timer is
	/// left. False if it never settles within a generous budget, which is how
	/// oscillation shows up.
	pub fn run(&mut self) -> bool {
		self.settle(10_000)
	}

	/// Up to `rounds` rounds of [`Self::run`].
	pub fn settle(&mut self, rounds: usize) -> bool {
		self.rounds(rounds, true)
	}

	/// Deliver round-robin until every unheld queue is empty, firing only the doubt
	/// timers that come due meanwhile.
	pub fn flow(&mut self) -> bool {
		self.rounds(10_000, false)
	}

	/// Each round delivers the head of every unheld queue and takes [`HOP`]. A quiet
	/// round jumps to the next doubt timer if `wait`, and ends otherwise.
	fn rounds(&mut self, rounds: usize, wait: bool) -> bool {
		for _ in 0..rounds {
			let ready: Vec<(Node, Node)> = self.busy().into_iter().filter(|key| !self.held.contains(key)).collect();
			if ready.is_empty() {
				match self.deadline().filter(|_| wait) {
					Some(at) => {
						self.now = self.now.max(at);
						self.tick();
						continue;
					}
					None => return true,
				}
			}
			for (from, to) in ready {
				self.deliver(from, to);
			}
			self.now = self.now.saturating_add(HOP);
			self.tick();
		}
		false
	}

	pub fn now(&self) -> Instant {
		self.now
	}

	/// The next doubt timer on any node.
	pub fn deadline(&self) -> Option<Instant> {
		self.nodes.iter().filter_map(|router| router.deadline()).min()
	}

	/// Let `elapsed` pass, firing each doubt timer due by then.
	pub fn wait(&mut self, elapsed: Duration) {
		self.now = self.now.saturating_add(elapsed);
		self.tick();
	}

	/// Move the clock to `node`'s next doubt or dead-hop timer, unless already past
	/// it, and fire that node's timers due by then, leaving every other node's.
	pub fn fire(&mut self, node: Node) {
		let at = self.nodes[node].deadline().expect("a timer");
		self.now = self.now.max(at);
		let now = self.now;
		self.nodes[node].timeout(now);
		self.flush();
	}

	/// Fire every doubt timer due by now.
	fn tick(&mut self) {
		let now = self.now;
		for router in &mut self.nodes {
			if router.deadline().is_some_and(|at| at <= now) {
				router.timeout(now);
			}
		}
		self.flush();
	}

	/// Move every router's output onto the queues and log its reader events.
	fn flush(&mut self) {
		for node in 0..self.nodes.len() {
			self.flush_paths(node);
			self.flush_events(node);
		}
	}

	fn flush_paths(&mut self, node: Node) {
		let outs = self.nodes[node].drain();
		for out in outs {
			let (to, msg) = match out {
				path::Out::Route { to, id, prefix, reach } => {
					// A path names every node it crossed once, the sender last.
					let key = (node, to, id, prefix.clone());
					if let Some(path::Reach::Finite(offer)) = &reach {
						let me = hop(node, self.restarts[node]);
						assert_eq!(
							offer.path.last().map(|hop| hop.node),
							Some(me),
							"node {node}'s path for {id}"
						);
						let named: BTreeSet<Id> = offer.path.iter().map(|hop| hop.node).collect();
						assert_eq!(named.len(), offer.path.len(), "node {node} sent {id} round a loop");
						let was = self.offered.insert(key, offer.clone());
						let same = |was: path::Offer| {
							path::Offer {
								path: offer.path.clone(),
								..was
							} == *offer
						};
						self.stats.hop_updates += usize::from(was.is_some_and(same));
					} else {
						self.offered.remove(&key);
					}
					self.stats.route_msgs += 1;
					self.stats.route_items += 1;
					(to, Msg::Path { id, prefix, reach })
				}
				path::Out::Batch {
					to,
					id,
					prefix,
					changes,
					floor,
				} => {
					self.stats.claim_msgs += 1;
					self.stats.claim_items += changes.len();
					(
						to,
						Msg::Claims {
							id,
							prefix,
							changes,
							floor,
						},
					)
				}
				path::Out::Dead { to, dead } => {
					self.stats.route_msgs += 1;
					self.stats.route_items += 1 + match &dead {
						path::Dead::Closed { moved, .. } => moved.len(),
						path::Dead::Draining { .. } => 0,
					};
					(to, Msg::Dead(dead))
				}
				path::Out::Ask { to, edit } => (to, Msg::Ask(edit)),
				path::Out::Edited { to } => (to, Msg::Edited),
				path::Out::Synced { to, prefix } => (to, Msg::Synced { prefix }),
			};
			self.queues.entry((node, to)).or_default().push_back(msg);
		}
	}

	fn flush_events(&mut self, node: Node) {
		{
			// A rotated origin starts a fresh history from the claims it carried.
			let declared = self.nodes[node].declared();
			if self.declared[node].is_some() && declared != self.declared[node] {
				let id = declared.expect("rotated");
				let router = &self.nodes[node];
				let rev = router.revision().unwrap();
				let changes = (router.view(id, &Path::empty()))
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
			// The simulator reads full views directly. Consume change notifications
			// alongside events, as a driver does, so they retain no script history.
			drop(self.nodes[node].changed());
			for event in self.nodes[node].events() {
				let origin = self.source(event.id).unwrap_or(event.id);
				let since = event.item.since;
				let withdrawn = self
					.covering(origin, &event.claim, self.revision(origin))
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

	/// Routes, prefixes, and streams visited across every node.
	pub fn work(&self) -> usize {
		self.spent + (0..self.nodes.len()).map(|node| self.path(node).work()).sum::<usize>()
	}

	/// Identities recomputed across every node.
	pub fn touched(&self) -> usize {
		self.nodes.iter().map(|router| router.touched()).sum()
	}

	/// The neighbour `node` routes `id` through: the widest live route's.
	pub fn next_hop(&self, node: Node, id: Id) -> Option<Node> {
		(self.nodes[node].routes(id).find(|(_, route)| route.offer().is_some()))?
			.1
			.next()
	}

	/// The neighbour `node` routes `id` through, `Some(None)` at its publisher, or
	/// `None` with no live route.
	pub fn toward(&self, node: Node, id: Id) -> Option<Option<Node>> {
		(self.nodes[node].routes(id).find(|(_, route)| route.offer().is_some())).map(|(_, route)| route.next())
	}

	/// `peer` notices its session to `node` closed, which `node` has yet to.
	pub fn sever(&mut self, node: Node, peer: Node) {
		self.up.remove(&Self::pair(node, peer));
		for key in [(node, peer), (peer, node)] {
			self.queues.remove(&key);
			self.held.remove(&key);
		}
		self.nodes[peer].disconnect([node], self.now);
		self.flush();
	}

	/// Whether `node` has a live route for `id`.
	pub fn routed(&self, node: Node, id: Id) -> bool {
		self.nodes[node].routes(id).any(|(_, route)| route.offer().is_some())
	}

	/// The node a request from `node`'s reader for `claim` of `id` ends at, following
	/// [`path::Router::forward`] hop by hop: `None` where it stops short of a publisher.
	pub fn reaches(&self, node: Node, id: Id, claim: &Path) -> Option<Node> {
		let (mut at, mut id, mut from) = (node, id, None);
		for _ in 0..=self.nodes.len() {
			match self.path(at).forward(id, claim, from.take())? {
				path::Forward::Here => return Some(at),
				path::Forward::Next { next, named, prefix } => {
					from = Some((at, prefix));
					(at, id) = (next, named);
				}
			}
		}
		panic!("a request for {id} {claim:?} from node {node} loops");
	}

	pub fn holds(&self, node: Node, id: Id) -> bool {
		self.nodes[node].holds(id)
	}

	/// The floors `node` keeps for `id`: one per view.
	pub fn floors(&self, node: Node, id: Id) -> usize {
		self.nodes[node].views().filter(|((key, _), _)| *key == id).count()
	}

	pub fn shown(&self, node: Node) -> &BTreeMap<(Id, Path), Shown<Item>> {
		self.nodes[node].shown()
	}

	pub fn shows(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		let claim = p(claim);
		self.shown(node).get(&(id, claim)).map(|shown| shown.available)
	}

	/// What the routers retain, summed over every node.
	pub fn state(&self) -> State {
		let r = self.kept();
		State {
			items: r.items,
			floors: r.views,
			heard: r.heard,
			sent: r.sent,
			pending: r.dead + r.doubt,
		}
	}

	/// What waits on another node: dead hops and doubts.
	pub fn pending(&self) -> usize {
		self.state().pending
	}

	/// [`path::Router`]'s retained state summed over every node.
	pub fn kept(&self) -> path::Retained {
		let mut out = path::Retained::default();
		for node in 0..self.nodes.len() {
			let r = self.path(node).retained();
			out.items += r.items;
			out.views += r.views;
			out.heard += r.heard;
			out.sent += r.sent;
			out.routes += r.routes;
			out.offers += r.offers;
			out.dead += r.dead;
			out.doubt += r.doubt;
			out.draining += r.draining;
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

	/// The origin's most specific claim at or above `claim` as of revision `rev`:
	/// what a stream answering `claim` presents there.
	pub fn covering(&self, id: Id, claim: &Path, rev: Rev) -> Option<Item> {
		let history = self.history.get(&id)?;
		let ancestors: BTreeSet<&Path> = history
			.iter()
			.map(|change| &change.claim)
			.filter(|known| within(claim, known))
			.collect();
		ancestors
			.into_iter()
			.rev()
			.filter_map(|known| self.truth(id, known.as_str(), rev))
			.next()
	}

	/// The origin's claims as of revision `rev` as a stream answering `prefix`
	/// presents them.
	pub fn presented(&self, id: Id, prefix: &Path, rev: Rev) -> BTreeMap<Path, Item> {
		let claims: BTreeSet<&Path> = (self.history.get(&id).into_iter().flatten())
			.map(|change| &change.claim)
			.filter(|claim| within(claim, prefix))
			.collect();
		let mut out: BTreeMap<Path, Item> = claims
			.into_iter()
			.filter_map(|claim| Some((claim.clone(), self.truth(id, claim.as_str(), rev)?)))
			.collect();
		if let Some(item) = self.covering(id, prefix, rev) {
			out.entry(prefix.clone()).or_insert(item);
		}
		out
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
			for ((key, _), view) in router.views() {
				let key = *key;
				let Some(id) = self.source(key) else {
					continue;
				};
				checked.insert((node, key));
				// A floor's region presents the most specific claim covering it.
				let claims: BTreeSet<&Path> = (self.history[&id].iter().map(|change| &change.claim))
					.chain(view.items.keys())
					.chain(view.floors.keys())
					.collect();
				for claim in claims {
					let Some(rev) = view.as_of(claim) else {
						if view.items.contains_key(claim) {
							return Err(format!("node {node} holds {id}'s {claim} with no floor"));
						}
						continue;
					};
					let known = view.items.get(claim).copied();
					let truth = match view.floors.contains_key(claim) {
						true => self.covering(id, claim, rev),
						false => self.truth(id, claim.as_str(), rev),
					};
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

/// How many scenarios a sweep runs: `MOQ_ROUTE_SCENARIOS`, or the gate's 150.
/// Each sweep draws its scenarios from one seeded stream, so a larger count
/// replays the gate's first and continues past them. Run a large count under
/// nextest's `sweep` profile (`just rs test -P sweep`), which waits up to an hour.
pub(super) fn scenarios() -> usize {
	std::env::var("MOQ_ROUTE_SCENARIOS").map_or(150, |count| count.parse().expect("MOQ_ROUTE_SCENARIOS"))
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

/// Every node's next-hop chain toward `id` ends without revisiting a node. A chain may
/// come round only through a stale route, one whose next hop has moved since offering
/// it: the model's `LoopsAreStale`.
pub(super) fn assert_loop_free(sim: &Sim, nodes: usize, id: Id) {
	let claims: BTreeSet<Path> = (0..nodes)
		.flat_map(|node| {
			sim.path(node).routes(id).map(|(prefix, _)| prefix.clone()).chain(
				sim.path(node)
					.shown()
					.keys()
					.filter_map(move |(key, claim)| (*key == id).then_some(claim.clone())),
			)
		})
		.collect();
	for node in 0..nodes {
		for claim in &claims {
			assert_scoped_chain(sim, node, id, claim);
		}
	}
}

/// Follow an actual request, keeping the route prefix and translated identity at
/// each hop. A transient cycle must contain an offer its next hop no longer uses.
fn assert_scoped_chain(sim: &Sim, node: Node, id: Id, claim: &Path) {
	let (mut at, mut key, mut from) = (node, id, None);
	let mut chain: Vec<(Node, bool)> = Vec::new();
	while let Some(path::Forward::Next { next, named, prefix }) = sim.path(at).forward(key, claim, from.take()) {
		let heard = sim.path(at).heard_offer(key, &prefix, next).map(|offer| &offer.path);
		let current_prefix = match sim.path(next).forward(named, claim, Some((at, prefix.clone()))) {
			Some(path::Forward::Next { prefix, .. }) => Some(prefix),
			Some(path::Forward::Here) => Some(Path::empty()),
			None => None,
		};
		let current = current_prefix
			.as_ref()
			.and_then(|prefix| sim.path(next).route_for(named, prefix))
			.and_then(|route| route.offer())
			.map(|offer| &offer.path);
		chain.push((at, heard.is_none() || heard != current));
		if let Some(start) = chain.iter().position(|(seen, _)| *seen == next) {
			assert!(
				chain[start..].iter().any(|(_, stale)| *stale),
				"{id} {claim:?} loops from node {node} round {chain:?}"
			);
			break;
		}
		from = Some((at, prefix));
		(at, key) = (next, named);
	}
}

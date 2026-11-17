//! One script on [`path::Router`], which keeps its doubt until the timer fires. The
//! ignored benchmarks also compare the rejected early-collection policy, and the
//! replaced rule where doubt kept relays off every route to a doubted publisher.
//!
//! A [`Lab`] builds the same mesh on each, applies one change at a time, and
//! delivers in rounds: every message queued when a round starts crosses its link
//! in that round, so a round is one hop of delay. After each delivery it follows
//! every relay's next hops, and after each round it counts the relays whose route
//! does not lead to the publisher and those with no route while a path exists.
//! `benchmark_route_rules` prints the table; the tests pin what it shows.

use std::collections::BTreeSet;

use super::multi::{Broadcast, MultiSim, Suspect, Tie, Tune};
use super::multicast::RelaySim;
use super::*;

/// A router under comparison: today's rule, the multicast epic's prototype, or its
/// production router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arm {
	Path(Rule),
	Multi(Tune),
	/// [`relay::Router`]: the rule [`Self::KEPT`] measures, as it ships, its readers
	/// showing a lost claim unavailable for this long before offline.
	Relay(Duration),
}

impl Arm {
	/// Today's rule as it ships.
	pub const TODAY: Self = Self::Path(Rule::PATH);
	/// The epic's rule as written.
	pub const PLAIN: Self = Self::Multi(Tune {
		suspect: Suspect::Never,
		quiet: false,
		floor: false,
		batch: false,
		agree: false,
		drain: false,
		claimed: false,
		epoch: false,
	});
	/// The epic's rule with what the bake-off found worth adding.
	pub const TUNED: Self = Self::Multi(Tune {
		suspect: Suspect::Never,
		quiet: false,
		floor: true,
		batch: true,
		agree: false,
		drain: true,
		claimed: false,
		epoch: false,
	});
	/// [`Self::TUNED`], and a node with nothing to announce offers no route to itself.
	pub const LEAN: Self = Self::Multi(Tune {
		suspect: Suspect::Never,
		quiet: false,
		floor: true,
		batch: true,
		agree: false,
		drain: true,
		claimed: true,
		epoch: false,
	});
	/// [`Self::LEAN`], with the floor kept per pair of relays: what the bake-off recommends.
	pub const KEPT: Self = Self::Multi(Self::TUNE);
	/// The prototype's settings under [`Self::KEPT`]: floor, epoch, batch, drain, and claimed.
	pub const TUNE: Tune = Tune {
		suspect: Suspect::Never,
		quiet: false,
		floor: true,
		batch: true,
		agree: false,
		drain: true,
		claimed: true,
		epoch: true,
	};

	/// The production router as it ships.
	pub const RELAY: Self = Self::Relay(relay::debounce(HOP));

	/// The epic's rule with `tune` changed from how it is written.
	pub fn with(tune: impl FnOnce(&mut Tune)) -> Self {
		let mut out = Tune::default();
		tune(&mut out);
		Self::Multi(out)
	}

	pub fn name(self) -> String {
		let tune = match self {
			Self::Path(rule) => return rule.as_str().into(),
			Self::Multi(tune) => tune,
			Self::Relay(debounce) if debounce.is_zero() => return "relay-undebounced".into(),
			Self::Relay(_) => return "relay".into(),
		};
		let parts = [
			(tune.suspect == Suspect::Flagged, "+suspect"),
			(tune.suspect == Suspect::Local, "+local"),
			(tune.quiet, "+quiet"),
			(tune.floor, "+floor"),
			(tune.batch, "+batch"),
			(tune.agree, "+agree"),
			(tune.drain, "+drain"),
			(tune.claimed, "+claimed"),
			(tune.epoch, "+epoch"),
		];
		(parts.into_iter().filter(|(on, _)| *on)).fold("multi".to_string(), |name, (_, part)| name + part)
	}

	fn net(self, nodes: usize) -> Box<dyn Net> {
		match self {
			Self::Path(rule) => Box::new(Sim::with(rule, nodes)),
			Self::Multi(tune) => Box::new(MultiSim::new(tune, nodes)),
			Self::Relay(debounce) => Box::new(RelaySim::new(nodes, debounce)),
		}
	}
}

impl From<Rule> for Arm {
	fn from(rule: Rule) -> Self {
		Self::Path(rule)
	}
}

/// What the routers retain, summed over every node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Held {
	/// Routes selected: one per publisher today, one per forwarding node in the prototype.
	pub routes: usize,
	/// Routes heard, one per neighbour offering one, selected or not.
	pub offers: usize,
	/// Claims, in every copy a node keeps.
	pub claims: usize,
	/// What waits on another node: dead hops, doubts, and suspected nodes.
	pub pending: usize,
	/// What is kept for good: pairs of nodes with a session known closed.
	pub lasting: usize,
}

/// What readers were shown that the publishers never did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Faults {
	/// Ends shown while the publisher still claimed.
	pub false_ends: usize,
	/// Starts of a life the same reader had seen end.
	pub revivals: usize,
}

/// The scope every scoped link in a comparison grants.
const GRANT: Grant = Grant {
	subject: "mesh",
	scope: "",
};

/// The production rule, checked in ordinary CI.
const RULES: [Rule; 1] = [Rule::PATH];

/// The rules compared, in the report's order.
pub(super) const COMPARED: [Rule; 3] = [Rule::EARLY, Rule::ORIGIN, Rule::PATH];

/// A queue of messages from one node to another.
pub(super) type Chan = (Node, Node);

/// What a router sent and showed so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sent {
	/// Route messages of every kind.
	pub msgs: usize,
	/// Routes those messages name.
	pub items: usize,
	/// Claims sent, each once per session it crossed.
	pub claims: usize,
	/// Claims a reader saw go offline: its relay let go of their route.
	pub offline: usize,
	/// Claims a reader saw turn unavailable: its relay holds their route.
	pub unavailable: usize,
	/// Claims a reader saw start.
	pub starts: usize,
}

impl std::ops::Sub for Sent {
	type Output = Self;

	fn sub(self, was: Self) -> Self {
		Self {
			msgs: self.msgs - was.msgs,
			items: self.items - was.items,
			claims: self.claims - was.claims,
			offline: self.offline - was.offline,
			unavailable: self.unavailable - was.unavailable,
			starts: self.starts - was.starts,
		}
	}
}

/// A simulated network either router runs in.
pub(super) trait Net {
	/// `node` passes on no route it learned, like a client.
	fn client(&mut self, node: Node);
	fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.link(a, b, cost, Tie::Peer);
	}
	/// Configure a link and open it.
	fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie);
	/// `node` sends GOAWAY on every session.
	fn goaway(&mut self, node: Node);
	fn open(&mut self, a: Node, b: Node);
	fn close(&mut self, a: Node, b: Node);
	/// The process at `node` is gone: nothing more passes between it and `peer`,
	/// either way, though `peer` has yet to notice.
	fn mute(&mut self, node: Node, peer: Node);
	/// `peer` notices its session to `node` closed. `node`, which is gone, does nothing.
	fn sever(&mut self, node: Node, peer: Node);
	/// `node` restarts with no sessions and no state.
	fn reset(&mut self, node: Node);
	fn publish(&mut self, node: Node, id: Id);
	fn claim(&mut self, node: Node, claim: &str, live: bool);
	/// The queues with something to deliver, and how much.
	fn ready(&self) -> Vec<(Chan, usize)>;
	fn step(&mut self, chan: Chan);
	/// One hop of delay passes. Whether a timer still runs.
	fn tick(&mut self) -> bool {
		false
	}
	/// The nodes a request from `from` for publisher `id` visits, and whether it
	/// ended at the publisher: `None` with no route. A chain longer than the network
	/// loops.
	fn chase(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)>;
	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool>;
	fn sent(&self) -> Sent;
	fn held(&self) -> Held;
	/// The session floors `nodes` keep, where the rule keeps any.
	fn floors(&self, _nodes: &[Node]) -> usize {
		0
	}
	fn faults(&self) -> Faults;
	/// Routes, claims, and streams visited.
	fn work(&self) -> usize;
}

pub(super) fn tally(kinds: impl Iterator<Item = Kind>) -> (usize, usize, usize) {
	let (mut offline, mut unavailable, mut starts) = (0, 0, 0);
	for kind in kinds {
		match kind {
			Kind::Offline => offline += 1,
			Kind::Unavailable => unavailable += 1,
			Kind::Start => starts += 1,
			_ => {}
		}
	}
	(offline, unavailable, starts)
}

impl Net for Sim {
	fn client(&mut self, node: Node) {
		self.leaf(node);
	}

	fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		let side = SimLink::cost(cost);
		match tie {
			Tie::Peer => Sim::link(self, a, b, side, side),
			Tie::Up => Sim::link(self, a, b, side.upstream(), side),
			Tie::Scoped => Sim::link(self, a, b, side.grant(GRANT), side.grant(GRANT)),
		}
	}

	fn goaway(&mut self, node: Node) {
		Sim::goaway(self, node);
	}

	fn open(&mut self, a: Node, b: Node) {
		self.connect(a, b);
	}

	fn close(&mut self, a: Node, b: Node) {
		self.disconnect(a, b);
	}

	fn mute(&mut self, node: Node, peer: Node) {
		self.hold(node, peer);
		self.hold(peer, node);
	}

	fn sever(&mut self, node: Node, peer: Node) {
		Sim::sever(self, node, peer);
	}

	fn reset(&mut self, node: Node) {
		self.kill(node);
	}

	fn publish(&mut self, node: Node, id: Id) {
		self.originate(node, id);
	}

	fn claim(&mut self, node: Node, claim: &str, live: bool) {
		match live {
			true => self.announce(node, claim, 0, None),
			false => self.unannounce(node, claim),
		}
	}

	fn ready(&self) -> Vec<(Chan, usize)> {
		(self.busy().into_iter())
			.filter(|chan| !self.held.contains(chan))
			.map(|(from, to)| ((from, to), self.queued(from, to)))
			.collect()
	}

	fn step(&mut self, (from, to): Chan) {
		self.deliver(from, to);
	}

	fn tick(&mut self) -> bool {
		self.wait(HOP);
		self.deadline().is_some()
	}

	fn chase(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)> {
		let mut hops = Vec::new();
		if self.grants.is_empty() {
			let mut at = from;
			self.toward(at, id)?;
			while hops.len() <= self.nodes.len() {
				match self.toward(at, id) {
					None => return Some((hops, false)),
					Some(None) => return Some((hops, true)),
					Some(Some(next)) => {
						hops.push(next);
						at = next;
					}
				}
			}
			return Some((hops, false));
		}
		// Past a scoped link the publisher goes by its qualified identity.
		let claim = p(&Mesh::claim(0));
		let key = [id, qualified(GRANT.subject, id)]
			.into_iter()
			.find(|key| self.path(from).forward(*key, &claim, None).is_some())?;
		let (mut at, mut key, mut prev) = (from, key, None);
		while hops.len() <= self.nodes.len() {
			match self.path(at).forward(key, &claim, prev.take()) {
				None => return Some((hops, false)),
				Some(path::Forward::Here) => return Some((hops, true)),
				Some(path::Forward::Next { next, named, prefix }) => {
					prev = Some((at, prefix));
					hops.push(next);
					(at, key) = (next, named);
				}
			}
		}
		Some((hops, false))
	}

	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		(self.shows(node, id, claim)).or_else(|| self.shows(node, qualified(GRANT.subject, id), claim))
	}

	fn held(&self) -> Held {
		let kept = self.kept();
		Held {
			routes: kept.routes,
			offers: kept.offers,
			claims: kept.items + kept.heard + kept.sent,
			pending: kept.dead + kept.doubt + kept.draining,
			lasting: 0,
		}
	}

	fn faults(&self) -> Faults {
		Faults {
			false_ends: false_ends(&self.events),
			revivals: revivals(&self.events),
		}
	}

	fn work(&self) -> usize {
		Sim::work(self)
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
}

impl Net for MultiSim {
	fn client(&mut self, node: Node) {
		MultiSim::client(self, node);
	}

	fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		MultiSim::link(self, a, b, cost, tie);
	}

	fn goaway(&mut self, node: Node) {
		MultiSim::goaway(self, node);
	}

	fn open(&mut self, a: Node, b: Node) {
		MultiSim::open(self, a, b);
	}

	fn close(&mut self, a: Node, b: Node) {
		MultiSim::close(self, a, b);
	}

	fn mute(&mut self, node: Node, peer: Node) {
		self.hold(node, peer);
		self.hold(peer, node);
	}

	fn sever(&mut self, node: Node, peer: Node) {
		MultiSim::sever(self, node, peer);
	}

	fn reset(&mut self, node: Node) {
		MultiSim::reset(self, node);
	}

	fn publish(&mut self, node: Node, id: Id) {
		MultiSim::publish(self, node, id);
	}

	fn claim(&mut self, node: Node, claim: &str, live: bool) {
		MultiSim::claim(self, node, claim, live);
	}

	fn ready(&self) -> Vec<(Chan, usize)> {
		MultiSim::ready(self)
	}

	fn step(&mut self, (from, to): Chan) {
		self.deliver(from, to);
	}

	fn tick(&mut self) -> bool {
		self.flush();
		false
	}

	fn chase(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)> {
		MultiSim::chase(self, from, &(id, p(&Mesh::claim(0))))
	}

	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		let broadcast: Broadcast = (id, p(claim));
		self.router(node)?.shown(&broadcast)
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
		let kept = self.kept();
		Held {
			routes: kept.used,
			offers: kept.routes,
			claims: kept.items + kept.heard + kept.foreign,
			pending: kept.pending,
			lasting: kept.closed,
		}
	}

	fn faults(&self) -> Faults {
		Faults {
			false_ends: multi::false_ends(&self.events),
			revivals: multi::revivals(&self.events),
		}
	}

	fn work(&self) -> usize {
		MultiSim::work(self)
	}
}

/// A mesh of relays, each publisher attached to one of them.
#[derive(Clone, Debug)]
pub(super) struct Mesh {
	pub relays: usize,
	pub links: Vec<(Node, Node, Metric)>,
	/// The links that are not between peers, keyed as `links` orders their ends.
	pub ties: BTreeMap<(Node, Node), Tie>,
	/// The relay each publisher attaches to.
	pub homes: Vec<Node>,
	/// Claims each publisher announces.
	pub claims: usize,
}

impl Mesh {
	/// A deterministic cheapest-path tree, shared by every rule when choosing
	/// which link or transit relay to fail. Ties use the next relay's number.
	pub fn tree(&self) -> Vec<Option<Node>> {
		let home = self.homes[0];
		let mut best = vec![None; self.relays];
		best[home] = Some((0, home));
		let mut pending = BTreeSet::from([(0, home)]);
		while let Some((cost, at)) = pending.pop_first() {
			if best[at].is_none_or(|(best, _)| cost != best) {
				continue;
			}
			for &(a, b, link) in &self.links {
				let peer = match at {
					_ if at == a => b,
					_ if at == b => a,
					_ => continue,
				};
				let candidate = (cost + link, at);
				if best[peer].is_none_or(|best| candidate < best) {
					best[peer] = Some(candidate);
					pending.insert((candidate.0, peer));
				}
			}
		}
		best.into_iter()
			.enumerate()
			.map(|(node, best)| best.and_then(|(_, next)| (node != home).then_some(next)))
			.collect()
	}

	/// A ring, the publisher on relay 0.
	pub fn ring(relays: usize) -> Self {
		let links = (0..relays).map(|a| (a, (a + 1) % relays, 1)).collect();
		Self {
			relays,
			links,
			ties: BTreeMap::new(),
			homes: vec![0],
			claims: 4,
		}
	}

	/// Every relay linked to every other, with `publishers` on relay 0.
	pub fn clique(relays: usize, publishers: usize) -> Self {
		let links = (0..relays)
			.flat_map(|a| (a + 1..relays).map(move |b| (a, b, 1)))
			.collect();
		Self {
			relays,
			links,
			ties: BTreeMap::new(),
			homes: vec![0; publishers],
			claims: 4,
		}
	}

	/// The mesh sweep's shape: a random spanning tree, then up to as many links
	/// again, with two publishers on random relays.
	pub fn sparse(relays: usize, rng: &mut XorShift) -> Self {
		let mut linked = BTreeSet::new();
		for b in 1..relays {
			linked.insert((rng.next(b as u64) as usize, b));
		}
		for _ in 0..rng.next(relays as u64 + 1) {
			let (a, b) = (rng.next(relays as u64) as usize, rng.next(relays as u64) as usize);
			if a != b {
				linked.insert((a.min(b), a.max(b)));
			}
		}
		let links = linked.into_iter().map(|(a, b)| (a, b, 1 + rng.next(3))).collect();
		let homes = (0..2).map(|_| rng.next(relays as u64) as usize).collect();
		Self {
			relays,
			links,
			ties: BTreeMap::new(),
			homes,
			claims: 4,
		}
	}

	pub fn publisher(&self, index: usize) -> (Node, Id) {
		(self.relays + index, 100 + index as Id)
	}

	pub fn claim(index: usize) -> String {
		format!("live/{index}")
	}
}

/// How a relay's route fares against the mesh as it really is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walk {
	/// Its next hops lead to the publisher over open links.
	Reached,
	/// They stop at a relay with no route, or cross a closed link.
	Broken,
	Loop,
}

/// What converging on one change cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Cost {
	pub sent: Sent,
	/// Hops of delay until nothing was left to deliver.
	pub rounds: usize,
	/// Deliveries after which some relay's next hops led round a loop.
	pub loops: usize,
	/// Relays, summed over rounds, whose route did not lead to the publisher.
	pub stale: usize,
	/// Relays, summed over rounds, with no route while a path to the publisher existed.
	pub dark: usize,
	/// Hops of delay until every relay's route led to the publisher for good.
	pub right: usize,
	/// Whether it settled at all.
	pub settled: bool,
}

pub(super) struct Lab {
	pub net: Box<dyn Net>,
	pub mesh: Mesh,
	/// Open links, relay to relay and publisher to relay.
	up: BTreeSet<(Node, Node)>,
	/// Links opened beyond the mesh's own.
	linked: BTreeSet<(Node, Node)>,
	down: BTreeSet<Node>,
	order: XorShift,
	/// The order a dying relay's neighbours notice in: its own, so every rule sees the
	/// same failure whatever it sent before.
	notice: XorShift,
	cost: Cost,
	/// Look for loops once a round rather than after every delivery, on meshes too
	/// large to walk that often.
	pub coarse: bool,
}

fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

impl Lab {
	/// Build `mesh` on `rule`'s router, with every publisher announcing, and let it settle.
	pub fn new(arm: impl Into<Arm>, mesh: &Mesh) -> Self {
		Self::with(arm.into().net(mesh.relays + mesh.homes.len()), mesh)
	}

	/// Build `mesh` on `net`, with a node for every relay and publisher.
	pub fn with(net: Box<dyn Net>, mesh: &Mesh) -> Self {
		let mut lab = Self {
			net,
			mesh: mesh.clone(),
			up: BTreeSet::new(),
			linked: BTreeSet::new(),
			down: BTreeSet::new(),
			order: XorShift(0x5eed_0001),
			notice: XorShift(0x5eed_0002),
			cost: Cost::default(),
			coarse: false,
		};
		for &(a, b, cost) in &mesh.links {
			// A tie is keyed from the relay that marks the link.
			match (mesh.ties.get(&(a, b)), mesh.ties.get(&(b, a))) {
				(Some(&tie), _) => lab.net.link(a, b, cost, tie),
				(None, Some(&tie)) => lab.net.link(b, a, cost, tie),
				(None, None) => lab.net.link(a, b, cost, Tie::Peer),
			}
			lab.up.insert(pair(a, b));
		}
		lab.settle();
		for index in 0..mesh.homes.len() {
			lab.arrive(index);
		}
		lab
	}

	/// Publisher `index` joins its relay for the first time and announces its claims.
	pub fn arrive(&mut self, index: usize) {
		let (node, id) = self.mesh.publisher(index);
		let home = self.mesh.homes[index];
		self.net.client(node);
		self.net.join(node, home, 1);
		self.up.insert(pair(node, home));
		self.net.publish(node, id);
		for claim in 0..self.mesh.claims {
			self.net.claim(node, &Mesh::claim(claim), true);
		}
		self.settle();
	}

	pub fn is_up(&self, a: Node, b: Node) -> bool {
		self.up.contains(&pair(a, b))
	}

	fn peers(&self, node: Node) -> Vec<Node> {
		(self.up.iter())
			.filter_map(|&(a, b)| match node {
				_ if node == a => Some(b),
				_ if node == b => Some(a),
				_ => None,
			})
			.collect()
	}

	/// The relays with a path to publisher `index` over open links.
	pub fn reach(&self, index: usize) -> BTreeSet<Node> {
		self.reach_over(&self.up, index)
	}

	fn reach_over(&self, up: &BTreeSet<(Node, Node)>, index: usize) -> BTreeSet<Node> {
		let (node, _) = self.mesh.publisher(index);
		// A route that crossed a marked link never crosses another, so a relay is
		// reached with or without one behind it.
		let mut seen = BTreeSet::from([(node, false)]);
		let mut queue = vec![(node, false)];
		while let Some((at, marked)) = queue.pop() {
			for &(a, b) in up {
				let peer = match at {
					_ if at == a => b,
					_ if at == b => a,
					_ => continue,
				};
				// `peer` learns it from `at`, and marks it if `at` is its upstream. A
				// scoped link starts the claim afresh.
				let tie = |from: Node, to: Node| self.mesh.ties.get(&(from, to)).copied();
				if marked && tie(at, peer) == Some(Tie::Up) {
					continue;
				}
				let scoped = tie(at, peer).or(tie(peer, at)) == Some(Tie::Scoped);
				let marked = !scoped && (marked || tie(peer, at) == Some(Tie::Up));
				// A publisher forwards nothing, and neither does a relay that died,
				// whether or not its neighbours have noticed.
				if peer < self.mesh.relays && !self.down.contains(&peer) && seen.insert((peer, marked)) {
					queue.push((peer, marked));
				}
			}
		}
		(seen.into_iter().map(|(relay, _)| relay))
			.filter(|&relay| relay != node)
			.collect()
	}

	fn walk(&self, from: Node, index: usize) -> Option<Walk> {
		let (origin, id) = self.mesh.publisher(index);
		let nodes = self.mesh.relays + self.mesh.homes.len();
		let (hops, here) = self.net.chase(from, id)?;
		let mut at = from;
		for &next in &hops {
			if !self.is_up(at, next) || self.down.contains(&next) {
				return Some(Walk::Broken);
			}
			at = next;
		}
		Some(match () {
			_ if hops.len() > nodes => Walk::Loop,
			_ if here && at == origin => Walk::Reached,
			_ => Walk::Broken,
		})
	}

	/// Whether any relay's next hops lead round a loop.
	fn looping(&self) -> bool {
		(0..self.mesh.homes.len())
			.any(|index| (0..self.mesh.relays).any(|relay| self.walk(relay, index) == Some(Walk::Loop)))
	}

	/// Relays whose next hops loop, whose route does not lead to its publisher, and
	/// with no route where a path exists.
	pub fn wrong(&self) -> (usize, usize, usize) {
		let (mut loops, mut stale, mut dark) = (0, 0, 0);
		for index in 0..self.mesh.homes.len() {
			let reach = self.reach(index);
			for relay in (0..self.mesh.relays).filter(|relay| !self.down.contains(relay)) {
				match self.walk(relay, index) {
					Some(Walk::Reached) => {}
					Some(Walk::Loop) => {
						loops += 1;
						stale += 1;
					}
					Some(Walk::Broken) => stale += 1,
					None => dark += usize::from(reach.contains(&relay)),
				}
			}
		}
		(loops, stale, dark)
	}

	/// Deliver everything queued now: one hop of delay. False if nothing was and no
	/// timer runs.
	pub fn round(&mut self) -> bool {
		let mut ready = self.net.ready();
		if ready.is_empty() && !self.net.tick() && self.net.ready().is_empty() {
			return false;
		}
		let idle = ready.is_empty();
		// Queues drain in a seeded random order, each in its own.
		for i in (1..ready.len()).rev() {
			ready.swap(i, self.order.next(i as u64 + 1) as usize);
		}
		for (chan, count) in ready {
			for _ in 0..count {
				self.net.step(chan);
				self.cost.loops += usize::from(!self.coarse && self.looping());
			}
		}
		if !idle {
			self.net.tick();
		}
		self.cost.rounds += 1;
		self.note();
		true
	}

	/// Count the relays left wrong as the latest round ends.
	fn note(&mut self) {
		let (loops, stale, dark) = self.wrong();
		self.cost.loops += usize::from(self.coarse && loops > 0);
		self.cost.stale += stale;
		self.cost.dark += dark;
		if stale + dark > 0 {
			self.cost.right = self.cost.rounds + 1;
		}
	}

	/// Deliver in rounds until nothing is left. False if that never happens.
	pub fn settle(&mut self) -> bool {
		for _ in 0..2_000 {
			if !self.round() {
				return true;
			}
		}
		false
	}

	/// Apply `change`, settle, and report what that cost.
	pub fn measure(&mut self, change: &dyn Fn(&mut Self)) -> Cost {
		let before = self.net.sent();
		self.cost = Cost::default();
		change(self);
		// What the change left wrong before any message moved.
		self.note();
		self.cost.settled = self.settle();
		self.cost.sent = self.net.sent() - before;
		self.cost
	}

	pub fn cut(&mut self, a: Node, b: Node) {
		self.net.close(a, b);
		self.up.remove(&pair(a, b));
	}

	pub fn mend(&mut self, a: Node, b: Node) {
		self.net.open(a, b);
		self.up.insert(pair(a, b));
	}

	/// Open a link the mesh does not have, configuring it the first time.
	pub fn attach(&mut self, a: Node, b: Node) {
		match self.linked.insert(pair(a, b)) {
			true => self.net.join(a, b, 1),
			false => self.net.open(a, b),
		}
		self.up.insert(pair(a, b));
	}

	/// The process at `relay` exits. Each neighbour notices in turn, a hop of delay apart.
	pub fn crash(&mut self, relay: Node) {
		let mut peers = self.peers(relay);
		for &peer in &peers {
			self.net.mute(relay, peer);
		}
		for i in (1..peers.len()).rev() {
			peers.swap(i, self.notice.next(i as u64 + 1) as usize);
		}
		self.down.insert(relay);
		for peer in peers {
			self.net.sever(relay, peer);
			self.up.remove(&pair(relay, peer));
			self.round();
		}
		self.net.reset(relay);
	}

	/// Let `rounds` hops of delay pass, delivering whatever is queued in each.
	pub fn pass(&mut self, rounds: usize) {
		for _ in 0..rounds {
			if !self.round() {
				self.cost.rounds += 1;
				self.note();
			}
		}
	}

	/// The process at `relay` exits, and its neighbours notice spread evenly over
	/// `rounds` hops of delay, as an idle timeout leaves them.
	pub fn crash_over(&mut self, relay: Node, rounds: usize) {
		let mut peers = self.peers(relay);
		for &peer in &peers {
			self.net.mute(relay, peer);
		}
		for i in (1..peers.len()).rev() {
			peers.swap(i, self.notice.next(i as u64 + 1) as usize);
		}
		self.down.insert(relay);
		let gap = rounds / peers.len().max(2).saturating_sub(1);
		for (i, peer) in peers.into_iter().enumerate() {
			if i > 0 {
				self.pass(gap);
			}
			self.net.sever(relay, peer);
			self.up.remove(&pair(relay, peer));
		}
		self.net.reset(relay);
	}

	/// Every node `node` has an open link to.
	pub fn links_of(&self, node: Node) -> Vec<Node> {
		self.peers(node)
	}

	/// The hops every relay's request crosses to each publisher, summed: what the
	/// settled routes cost to use.
	pub fn hops(&self) -> usize {
		(0..self.mesh.homes.len())
			.flat_map(|index| (0..self.mesh.relays).map(move |relay| (relay, index)))
			// Only a walk that reaches its publisher over open links is in use.
			.filter(|&(relay, index)| self.walk(relay, index) == Some(Walk::Reached))
			.filter_map(|(relay, index)| self.net.chase(relay, self.mesh.publisher(index).1))
			.map(|(hops, _)| hops.len())
			.sum()
	}

	/// `relay` starts again with no state, and its links and publishers reconnect.
	pub fn revive(&mut self, relay: Node) {
		self.down.remove(&relay);
		let links: Vec<(Node, Node)> = (self.mesh.links.iter().map(|&(a, b, _)| (a, b)))
			.chain((0..self.mesh.homes.len()).map(|index| (self.mesh.publisher(index).0, self.mesh.homes[index])))
			.filter(|&(a, b)| a == relay || b == relay)
			.collect();
		for (a, b) in links {
			if !self.down.contains(&a) && !self.down.contains(&b) && !self.is_up(a, b) {
				self.mend(a, b);
			}
		}
	}

	/// `relay` restarts at once: every session of its closes together and reopens.
	pub fn restart(&mut self, relay: Node) {
		let peers = self.peers(relay);
		for &peer in &peers {
			self.net.mute(relay, peer);
		}
		for peer in peers {
			self.net.sever(relay, peer);
			self.up.remove(&pair(relay, peer));
		}
		self.net.reset(relay);
		self.revive(relay);
		// Unlike after a crash, the links the mesh lacks reconnect too, such as a
		// publisher's second home.
		let linked: Vec<(Node, Node)> = (self.linked.iter().copied())
			.filter(|&(a, b)| (a == relay || b == relay) && !self.is_up(a, b))
			.collect();
		for (a, b) in linked {
			self.mend(a, b);
		}
	}

	/// Deliver one message from a queue `pick` chooses. False if none waits.
	pub fn deliver(&mut self, pick: &mut XorShift) -> bool {
		let ready = self.net.ready();
		if ready.is_empty() {
			return false;
		}
		let (chan, _) = ready[pick.next(ready.len() as u64) as usize];
		self.net.step(chan);
		true
	}

	/// Every relay with a path to a publisher routes to it and shows its claims as
	/// available, and every other relay holds nothing of it.
	pub fn check(&self) -> Result<(), String> {
		for index in 0..self.mesh.homes.len() {
			let (_, id) = self.mesh.publisher(index);
			let reach = self.reach(index);
			for relay in (0..self.mesh.relays).filter(|relay| !self.down.contains(relay)) {
				let walk = self.walk(relay, index);
				let shows = self.net.showing(relay, id, &Mesh::claim(0));
				let want = match reach.contains(&relay) {
					true => (Some(Walk::Reached), Some(true)),
					false => (None, None),
				};
				if (walk, shows) != want {
					return Err(format!(
						"relay {relay} has {walk:?} to publisher {index} and shows {shows:?}, not {want:?}"
					));
				}
			}
		}
		Ok(())
	}

	/// The link of a publisher's cheapest-path tree that the most relays route
	/// over, among those whose loss leaves every relay a path (`detour`) or cuts
	/// some off.
	pub fn tree_link(&self, index: usize, detour: bool, tree: &[Option<Node>]) -> Option<(Node, Node)> {
		let mut uses: BTreeMap<(Node, Node), usize> = BTreeMap::new();
		for relay in 0..self.mesh.relays {
			let mut at = relay;
			while let Some(next) = tree[at] {
				*uses.entry(pair(at, next)).or_default() += 1;
				at = next;
			}
		}
		let before = self.reach(index).len();
		let mut links: Vec<((Node, Node), usize)> = uses.into_iter().collect();
		links.sort_by_key(|&(link, uses)| (std::cmp::Reverse(uses), link));
		links.into_iter().map(|(link, _)| link).find(|&(a, b)| {
			let mut up = self.up.clone();
			up.remove(&pair(a, b));
			(self.reach_over(&up, index).len() == before) == detour
		})
	}
}

/// The changes the report measures, in the order a case applies them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Change {
	/// A publisher joins and announces its claims.
	Arrive,
	/// A link relays route over closes, and every relay still has a path.
	Detour,
	/// That link returns.
	Shortcut,
	/// A link closes and cuts some relays off.
	Cut,
	/// That link returns.
	Mend,
	/// A relay that others route through dies. Its neighbours notice one by one.
	Transit,
	/// It restarts.
	Restart,
	/// The publisher's own relay dies.
	Home,
	/// The publisher's session closes.
	Leave,
	/// The publisher's session closes and it reconnects to the same relay at once.
	Rejoin,
	/// The publisher's session closes and it reconnects at once through another relay.
	Move,
	/// The publisher withdraws one claim.
	Withdraw,
}

impl Change {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Arrive => "publisher_arrives",
			Self::Detour => "link_down_detour",
			Self::Shortcut => "link_up_shortcut",
			Self::Cut => "link_down_cut",
			Self::Mend => "link_up_mend",
			Self::Transit => "transit_relay_dies",
			Self::Restart => "relay_restarts",
			Self::Home => "home_relay_dies",
			Self::Leave => "publisher_leaves",
			Self::Rejoin => "publisher_rejoins",
			Self::Move => "publisher_moves",
			Self::Withdraw => "claim_withdrawn",
		}
	}
}

/// Run every change on `mesh` under `rule`, each from a settled mesh, checking the
/// mesh is right whenever it settles.
pub(super) fn run(arm: impl Into<Arm>, mesh: &Mesh) -> Vec<(Change, Cost)> {
	let rule: Arm = arm.into();
	let mut out = Vec::new();
	let mut lab = Lab::new(rule, mesh);
	let tree = mesh.tree();
	let mut apply = |lab: &mut Lab, change: Change, apply: &dyn Fn(&mut Lab)| {
		let cost = lab.measure(apply);
		assert!(cost.settled, "{}: {change:?} never settles on {mesh:?}", rule.name());
		if let Err(err) = lab.check() {
			panic!("{}: after {change:?} on {mesh:?}: {err}", rule.name());
		}
		out.push((change, cost));
	};
	// The last publisher leaves a settled mesh and comes back.
	let last = mesh.homes.len() - 1;
	let (node, _) = mesh.publisher(last);
	let home = mesh.homes[last];
	apply(&mut lab, Change::Leave, &|lab| lab.cut(node, home));
	apply(&mut lab, Change::Arrive, &|lab| lab.mend(node, home));
	apply(&mut lab, Change::Rejoin, &|lab| {
		lab.cut(node, home);
		lab.mend(node, home);
	});
	// A relay next to its home, so the publisher's new route takes a detour no longer
	// than the old one did.
	let next = (mesh.links.iter()).find_map(|&(a, b, _)| match home {
		_ if a == home => Some(b),
		_ if b == home => Some(a),
		_ => None,
	});
	if let Some(next) = next {
		apply(&mut lab, Change::Move, &|lab| {
			lab.cut(node, home);
			lab.attach(node, next);
		});
		lab.cut(node, next);
		lab.mend(node, home);
		assert!(lab.settle());
	}

	for (detour, down, up) in [
		(true, Change::Detour, Change::Shortcut),
		(false, Change::Cut, Change::Mend),
	] {
		if let Some((a, b)) = lab.tree_link(0, detour, &tree) {
			apply(&mut lab, down, &|lab| lab.cut(a, b));
			apply(&mut lab, up, &|lab| lab.mend(a, b));
		}
	}

	// The relay the most others route through, the publisher's own aside.
	let (node, _) = mesh.publisher(0);
	let mut through = vec![0; mesh.relays];
	for relay in 0..mesh.relays {
		let mut at = relay;
		while let Some(next) = tree[at] {
			through[next] += 1;
			at = next;
		}
	}
	let transit = (0..mesh.relays)
		.filter(|relay| !mesh.homes.contains(relay) && through[*relay] > 0)
		.max_by_key(|&relay| (through[relay], std::cmp::Reverse(relay)));
	if let Some(relay) = transit {
		apply(&mut lab, Change::Transit, &|lab| lab.crash(relay));
		apply(&mut lab, Change::Restart, &|lab| lab.revive(relay));
	}

	apply(&mut lab, Change::Withdraw, &|lab| {
		lab.net.claim(node, &Mesh::claim(1), false)
	});
	lab.net.claim(node, &Mesh::claim(1), true);
	assert!(lab.settle());

	let home = mesh.homes[0];
	apply(&mut lab, Change::Home, &|lab| lab.crash(home));
	out
}

/// What random churn showed, summed over its cases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Churn {
	pub steps: usize,
	/// Steps after which some relay's next hops led round a loop.
	pub loops: usize,
	/// Relays, summed over steps, whose route did not lead to the publisher.
	pub stale: usize,
	/// Relays, summed over steps, with no route while a path to the publisher existed.
	pub dark: usize,
	pub sent: Sent,
}

/// The script behind `sweep_no_route_outgrows_a_loop_free_path`, on either router:
/// random meshes of three to five relays where links flap, the publisher's most of
/// all, relays restart, and every message is delivered in a random order. Each rule
/// sees the same meshes and the same changes at the same steps. Once the churn
/// stops the mesh must settle and be right. With `two_homes` the publisher also
/// joins a second relay, and its flaps pick either link.
pub(super) fn churn(arm: impl Into<Arm>, cases: std::ops::Range<usize>, two_homes: bool) -> Churn {
	let rule: Arm = arm.into();
	let mut total = Churn::default();
	for case in cases {
		let mut script = XorShift(0x0bad_c0de + case as u64 * 0x9e37_79b9);
		let mut pick = XorShift(0x5eed_0002 + case as u64);
		let relays = 3 + script.next(3) as usize;
		let mut links = Vec::new();
		for a in 0..relays {
			for b in a + 1..relays {
				if b == a + 1 || script.chance(50) {
					links.push((a, b, 1 + script.next(3)));
				}
			}
		}
		let mesh = Mesh {
			relays,
			links,
			ties: BTreeMap::new(),
			homes: vec![script.next(relays as u64) as usize],
			claims: 2,
		};
		let mut lab = Lab::new(rule, &mesh);
		let (node, _) = mesh.publisher(0);
		let mut homes = vec![mesh.homes[0]];
		if two_homes {
			homes.push((mesh.homes[0] + 1 + script.next(relays as u64 - 1) as usize) % relays);
			lab.attach(node, homes[1]);
			assert!(lab.settle());
		}
		let before = lab.net.sent();
		for _ in 0..300 {
			match script.next(40) {
				0 | 1 => {
					let (a, b) = match script.chance(60) {
						true if two_homes => (node, homes[script.next(2) as usize]),
						true => (node, mesh.homes[0]),
						false => {
							let (a, b, _) = mesh.links[script.next(mesh.links.len() as u64) as usize];
							(a, b)
						}
					};
					match lab.is_up(a, b) {
						true => lab.cut(a, b),
						false => lab.mend(a, b),
					}
				}
				2 if case % 4 == 0 && script.chance(25) => {
					let relay = script.next(relays as u64) as usize;
					lab.restart(relay);
					assert!(homes.iter().all(|&home| home != relay || lab.is_up(node, home)));
				}
				_ => {
					lab.deliver(&mut pick);
				}
			}
			// A step is one delivery, so a hop of delay is several of them.
			if total.steps % 8 == 7 {
				lab.net.tick();
			}
			let (loops, stale, dark) = lab.wrong();
			total.steps += 1;
			total.loops += usize::from(loops > 0);
			total.stale += stale;
			total.dark += dark;
		}
		assert!(lab.settle(), "{}: case {case} never settles on {mesh:?}", rule.name());
		if let Err(err) = lab.check() {
			panic!("{}: case {case} on {mesh:?}: {err}", rule.name());
		}
		let sent = lab.net.sent() - before;
		total.sent = Sent {
			msgs: total.sent.msgs + sent.msgs,
			items: total.sent.items + sent.items,
			claims: total.sent.claims + sent.claims,
			offline: total.sent.offline + sent.offline,
			unavailable: total.sent.unavailable + sent.unavailable,
			starts: total.sent.starts + sent.starts,
		};
	}
	total
}

/// Under churn every rule settles and is right once it does.
#[test]
fn every_rule_settles_after_churn() {
	for rule in RULES {
		for two_homes in [false, true] {
			churn(rule, 0..60, two_homes);
		}
	}
}

/// Loops, stale routes, and messages under the same churn, per rule.
#[test]
#[ignore = "a benchmark; Nightly reports it"]
fn benchmark_route_churn() {
	// Nightly's report reads stdout and keeps the columns it has a unit for, so the
	// message counts go there and the whole table to stderr. A claim sent is one
	// message on the wire.
	println!("rule,route_messages,claim_messages");
	eprintln!("rule,steps,loops,stale,dark,msgs,items,claims,offline,unavailable");
	for rule in COMPARED {
		let churn = churn(rule, 0..400, false);
		let (rule, steps, sent) = (rule.as_str(), churn.steps, churn.sent);
		println!("{rule},{},{}", sent.msgs, sent.claims);
		eprintln!(
			"{rule},{steps},{},{},{},{},{},{},{},{}",
			churn.loops, churn.stale, churn.dark, sent.msgs, sent.items, sent.claims, sent.offline, sent.unavailable,
		);
	}
}

/// The same churn with the publisher on two relays: what keeping its other link
/// usable while one flaps costs, per rule.
#[test]
#[ignore = "a benchmark; Nightly reports it"]
fn benchmark_route_churn_two_homes() {
	println!("rule,homes,route_messages,claim_messages");
	eprintln!("rule,steps,loops,stale,dark,msgs,items,claims,offline,unavailable");
	for rule in COMPARED {
		let churn = churn(rule, 0..400, true);
		let (rule, steps, sent) = (rule.as_str(), churn.steps, churn.sent);
		println!("{rule},2,{},{}", sent.msgs, sent.claims);
		eprintln!(
			"{rule},{steps},{},{},{},{},{},{},{},{}",
			churn.loops, churn.stale, churn.dark, sent.msgs, sent.items, sent.claims, sent.offline, sent.unavailable,
		);
	}
}

/// Each rule's router carries every change on every shape, and is right whenever
/// it settles.
#[test]
fn every_rule_converges_on_every_shape() {
	let mut rng = XorShift(0x6d6f_7100);
	let mut meshes = vec![Mesh::ring(6), Mesh::clique(6, 1), Mesh::clique(4, 3)];
	meshes.extend((0..8).map(|_| Mesh::sparse(6, &mut rng)));
	for mesh in &meshes {
		for rule in RULES {
			run(rule, mesh);
		}
	}
}

/// Path changes carry no claim changes; a claim withdrawal changes no route.
/// Counts and recovery times are measurements in the benchmarks below.
#[test]
fn path_changes_keep_claims_and_withdrawals_keep_routes() {
	for mesh in [Mesh::clique(6, 1), Mesh::ring(6)] {
		let costs = RULES.map(|rule| run(rule, &mesh).into_iter().collect::<BTreeMap<_, _>>());
		for costs in &costs {
			assert!(costs.values().all(|cost| cost.settled && cost.loops == 0));
		}
		for path in &costs {
			for change in [Change::Leave, Change::Detour, Change::Home] {
				assert_eq!(path[&change].sent.claims, 0);
			}
			assert_eq!(path[&Change::Detour].sent.offline, 0, "a detour keeps claims");
			assert_eq!(path[&Change::Withdraw].sent.msgs, 0, "claims alone change no route");
		}
	}
}

/// Per change, mesh size, and rule, over fixed seeds: the mean of what converging cost.
#[test]
#[ignore = "a benchmark; Nightly reports it"]
fn benchmark_route_rules() {
	// As above: message counts to stdout for the report, which keeps the largest mesh
	// alone, and the whole table to stderr.
	println!("shape,relays,change,rule,route_messages,claim_messages,loop_messages,reroute_ms");
	eprintln!("shape,relays,change,rule,msgs,items,claims,rounds,right,stale,dark,loops,offline,unavailable");
	for relays in [3, 6, 12, 24] {
		let mut rng = XorShift(0x6d6f_7100 + relays as u64);
		let mut shapes = vec![
			("ring", vec![Mesh::ring(relays)]),
			("clique", vec![Mesh::clique(relays, 1)]),
		];
		shapes.push(("clique_8_publishers", vec![Mesh::clique(relays, 8)]));
		shapes.push(("sparse", (0..16).map(|_| Mesh::sparse(relays, &mut rng)).collect()));
		for (shape, meshes) in &shapes {
			for rule in COMPARED {
				let mut all: BTreeMap<Change, Vec<Cost>> = BTreeMap::new();
				for mesh in meshes {
					for (change, cost) in run(rule, mesh) {
						all.entry(change).or_default().push(cost);
					}
				}
				for (change, costs) in all {
					let mean =
						|field: fn(&Cost) -> usize| costs.iter().map(field).sum::<usize>() as f64 / costs.len() as f64;
					let (change, rule) = (change.as_str(), rule.as_str());
					if relays == 24 {
						println!(
							"{shape},{relays},{change},{rule},{:.1},{:.1},{:.1},{:.1}",
							mean(|cost| cost.sent.msgs),
							mean(|cost| cost.sent.claims),
							mean(|cost| cost.loops),
							mean(|cost| cost.right) * HOP.as_secs_f64() * 1000.0,
						);
					}
					eprintln!(
						"{shape},{relays},{change},{rule},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1}",
						mean(|cost| cost.sent.msgs),
						mean(|cost| cost.sent.items),
						mean(|cost| cost.sent.claims),
						mean(|cost| cost.rounds),
						mean(|cost| cost.right),
						mean(|cost| cost.stale),
						mean(|cost| cost.dark),
						mean(|cost| cost.loops),
						mean(|cost| cost.sent.offline),
						mean(|cost| cost.sent.unavailable),
					);
				}
			}
		}
	}
}

//! One script on both rules: [`Router`], which keeps Babel's distance, and
//! [`path::Router`], which keeps its doubt until the timer fires. The ignored
//! benchmarks also compare the rejected early-collection policy.
//!
//! A [`Lab`] builds the same mesh on each, applies one change at a time, and
//! delivers in rounds: every message queued when a round starts crosses its link
//! in that round, so a round is one hop of delay. After each delivery it follows
//! every relay's next hops, and after each round it counts the relays whose route
//! does not lead to the publisher and those with no route while a path exists.
//! `benchmark_route_rules` prints the table; the tests pin what it shows.

use std::collections::BTreeSet;

use super::*;

/// The production router and its accepted replacement, checked in ordinary CI.
const RULES: [Rule; 2] = [Rule::Distance, Rule::PATH];

/// The rules compared, in the report's order.
pub(super) const COMPARED: [Rule; 3] = [Rule::Distance, Rule::EARLY, Rule::PATH];

/// A queue of messages, or of echoed ROUTE_ENDs, from one node to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Chan {
	Msg(Node, Node),
	Ack(Node, Node),
}

/// What a router sent and showed so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sent {
	/// Route messages of every kind, requests and echoes included.
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
	fn join(&mut self, a: Node, b: Node, cost: Metric);
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
	/// The neighbour `node` routes `id` through: `Some(None)` at the publisher.
	fn next(&self, node: Node, id: Id) -> Option<Option<Node>>;
	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool>;
	fn sent(&self) -> Sent;
}

fn tally(kinds: impl Iterator<Item = Kind>) -> (usize, usize, usize) {
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

	fn join(&mut self, a: Node, b: Node, cost: Metric) {
		Sim::join(self, a, b, cost);
	}

	fn open(&mut self, a: Node, b: Node) {
		self.connect(a, b);
	}

	fn close(&mut self, a: Node, b: Node) {
		self.disconnect(a, b);
		self.closed(a);
		self.closed(b);
	}

	fn mute(&mut self, node: Node, peer: Node) {
		self.hold(node, peer);
		self.hold(peer, node);
	}

	fn sever(&mut self, node: Node, peer: Node) {
		Sim::sever(self, node, peer);
		self.closed(peer);
	}

	fn reset(&mut self, node: Node) {
		self.kill(node);
	}

	fn publish(&mut self, node: Node, id: Id) {
		self.originate(node, id, 0);
	}

	fn claim(&mut self, node: Node, claim: &str, live: bool) {
		match live {
			true => self.announce(node, claim, 0, None),
			false => self.unannounce(node, claim),
		}
	}

	fn ready(&self) -> Vec<(Chan, usize)> {
		let msgs = (self.busy().into_iter()).map(|(from, to)| (Chan::Msg(from, to), self.queued(from, to)));
		let acks = (self.busy_acks().into_iter()).map(|(from, to)| (Chan::Ack(from, to), self.queued_acks(from, to)));
		let held = |chan: &Chan| match *chan {
			Chan::Msg(from, to) | Chan::Ack(from, to) => self.held.contains(&(from, to)),
		};
		msgs.chain(acks).filter(|(chan, _)| !held(chan)).collect()
	}

	fn step(&mut self, chan: Chan) {
		match chan {
			Chan::Msg(from, to) => self.deliver(from, to),
			Chan::Ack(from, to) => self.deliver_ack(from, to),
		};
	}

	fn tick(&mut self) -> bool {
		self.wait(HOP);
		self.deadline().is_some()
	}

	fn next(&self, node: Node, id: Id) -> Option<Option<Node>> {
		self.toward(node, id)
	}

	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		self.shows(node, id, claim)
	}

	fn sent(&self) -> Sent {
		let (offline, unavailable, starts) = tally(self.events.iter().map(|event| event.kind));
		Sent {
			msgs: self.stats.route_msgs + self.stats.requests,
			items: self.stats.route_items + self.stats.requests,
			claims: self.stats.claim_items,
			offline,
			unavailable,
			starts,
		}
	}
}

impl Sim {
	/// A publisher advances its seqno when a session its route went out on closes,
	/// as `route::Node` has it do. A new session lengthens no path, so it does not.
	/// The path rule has no seqno.
	fn closed(&mut self, node: Node) {
		if self.declared[node].is_some() && !self.paths() {
			self.bump(node);
		}
	}
}

/// A mesh of relays, each publisher attached to one of them.
#[derive(Clone, Debug)]
pub(super) struct Mesh {
	pub relays: usize,
	pub links: Vec<(Node, Node, Metric)>,
	/// The relay each publisher attaches to.
	pub homes: Vec<Node>,
	/// Claims each publisher announces.
	pub claims: usize,
}

impl Mesh {
	/// A deterministic cheapest-path tree, shared by every rule when choosing
	/// which link or transit relay to fail. Ties use the next relay's number.
	fn tree(&self) -> Vec<Option<Node>> {
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
			homes,
			claims: 4,
		}
	}

	pub fn publisher(&self, index: usize) -> (Node, Id) {
		(self.relays + index, 100 + index as Id)
	}

	fn claim(index: usize) -> String {
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
	cost: Cost,
}

fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

impl Lab {
	/// Build `mesh` on `rule`'s router, with every publisher announcing, and let it settle.
	pub fn new(rule: Rule, mesh: &Mesh) -> Self {
		let nodes = mesh.relays + mesh.homes.len();
		let mut lab = Self {
			net: Box::new(Sim::with(rule, nodes)),
			mesh: mesh.clone(),
			up: BTreeSet::new(),
			linked: BTreeSet::new(),
			down: BTreeSet::new(),
			order: XorShift(0x5eed_0001),
			cost: Cost::default(),
		};
		for &(a, b, cost) in &mesh.links {
			lab.net.join(a, b, cost);
			lab.up.insert(pair(a, b));
		}
		lab.settle();
		for index in 0..mesh.homes.len() {
			lab.arrive(index);
		}
		lab
	}

	/// Publisher `index` joins its relay for the first time and announces its claims.
	fn arrive(&mut self, index: usize) {
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
		let mut seen = BTreeSet::from([node]);
		let mut queue = vec![node];
		while let Some(at) = queue.pop() {
			for &(a, b) in up {
				let peer = match at {
					_ if at == a => b,
					_ if at == b => a,
					_ => continue,
				};
				// A publisher forwards nothing, and neither does a relay that died,
				// whether or not its neighbours have noticed.
				if peer < self.mesh.relays && !self.down.contains(&peer) && seen.insert(peer) {
					queue.push(peer);
				}
			}
		}
		seen.remove(&node);
		seen
	}

	fn walk(&self, from: Node, index: usize) -> Option<Walk> {
		let (origin, id) = self.mesh.publisher(index);
		let nodes = self.mesh.relays + self.mesh.homes.len();
		self.net.next(from, id)?;
		let mut at = from;
		for _ in 0..=nodes {
			match self.net.next(at, id) {
				None => return Some(Walk::Broken),
				Some(None) if at == origin => return Some(Walk::Reached),
				Some(None) => return Some(Walk::Broken),
				Some(Some(next)) if !self.is_up(at, next) || self.down.contains(&next) => return Some(Walk::Broken),
				Some(Some(next)) => at = next,
			}
		}
		Some(Walk::Loop)
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
				self.cost.loops += usize::from(self.looping());
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
		let (_, stale, dark) = self.wrong();
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
			peers.swap(i, self.order.next(i as u64 + 1) as usize);
		}
		self.down.insert(relay);
		for peer in peers {
			self.net.sever(relay, peer);
			self.up.remove(&pair(relay, peer));
			self.round();
		}
		self.net.reset(relay);
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
	fn tree_link(&self, index: usize, detour: bool, tree: &[Option<Node>]) -> Option<(Node, Node)> {
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
pub(super) fn run(rule: Rule, mesh: &Mesh) -> Vec<(Change, Cost)> {
	let mut out = Vec::new();
	let mut lab = Lab::new(rule, mesh);
	let tree = mesh.tree();
	let mut apply = |lab: &mut Lab, change: Change, apply: &dyn Fn(&mut Lab)| {
		let cost = lab.measure(apply);
		assert!(cost.settled, "{}: {change:?} never settles on {mesh:?}", rule.as_str());
		if let Err(err) = lab.check() {
			panic!("{}: after {change:?} on {mesh:?}: {err}", rule.as_str());
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
/// stops the mesh must settle and be right.
pub(super) fn churn(rule: Rule, cases: std::ops::Range<usize>) -> Churn {
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
			homes: vec![script.next(relays as u64) as usize],
			claims: 2,
		};
		let mut lab = Lab::new(rule, &mesh);
		let (node, _) = mesh.publisher(0);
		let before = lab.net.sent();
		for _ in 0..300 {
			match script.next(40) {
				0 | 1 => {
					let (a, b) = match script.chance(60) {
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
				2 if case % 4 == 0 && script.chance(25) => lab.restart(script.next(relays as u64) as usize),
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
		assert!(lab.settle(), "{}: case {case} never settles on {mesh:?}", rule.as_str());
		if let Err(err) = lab.check() {
			panic!("{}: case {case} on {mesh:?}: {err}", rule.as_str());
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

/// Under churn every rule settles and is right once it does, and the distance
/// never loops, even for a step.
#[test]
fn every_rule_settles_after_churn() {
	for rule in RULES {
		let cost = churn(rule, 0..60);
		if rule == Rule::Distance {
			assert_eq!(cost.loops, 0);
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
		let churn = churn(rule, 0..400);
		let (rule, steps, sent) = (rule.as_str(), churn.steps, churn.sent);
		println!("{rule},{},{}", sent.msgs, sent.claims);
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
		for path in &costs[1..] {
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

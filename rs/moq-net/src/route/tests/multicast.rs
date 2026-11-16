//! [`relay::Router`], the multicast epic's rule as it ships, on the bake-off's network.
//!
//! [`RelaySim`] wires one router per relay through [`MultiSim`]'s ordered, holdable
//! queues, so the bake-off's [`Lab`] measures it beside the prototype. The interest
//! exchange that opens a session (ANNOUNCE_REQUEST, its marker, and SYNCED) crosses at
//! once and is not counted, as it travels with the session's setup; everything after
//! it waits its turn. Each round of deliveries takes [`HOP`] on a mocked clock, which
//! only the readers' debounce reads.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use kio::clock::Instant;

use crate::route::relay::{self, Claim, Msg, Next, NodeId, Port, Role};

use super::bakeoff::{Failure, Row, carries, fail, shapes};
use super::compare::{Arm, Chan, Faults, Held, Lab, Mesh, Net, Sent, churn, run, tally};
use super::multi::{self, Seen, Tie};
use super::*;

pub(super) type Router = relay::Router<Node, Id, ()>;
type Message = Msg<Id, ()>;

/// What sits at a simulated node.
#[derive(Clone, Debug)]
enum Spot {
	Relay(Box<Router>),
	/// A publisher: an endpoint that never forwards.
	Endpoint {
		/// The Instance its next claim serves.
		id: Option<Id>,
		/// Its claims, each with the Instance it serves.
		claims: BTreeSet<(Path, Id)>,
		/// What each relay asked it for.
		asked: BTreeMap<Node, BTreeSet<Path>>,
	},
}

/// A network of [`relay::Router`]s and endpoints.
#[derive(Clone)]
pub(super) struct RelaySim {
	spots: Vec<Spot>,
	restarts: Vec<u32>,
	/// Each side's cost and mark, keyed from that side.
	ports: BTreeMap<(Node, Node), (Metric, bool)>,
	ties: BTreeMap<(Node, Node), Tie>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Message>>,
	held: BTreeSet<(Node, Node)>,
	lives: BTreeMap<multi::Broadcast, u64>,
	pub stats: Stats,
	/// Messages each node sent, the interest exchange left out.
	pub sent: BTreeMap<Node, usize>,
	pub events: Vec<Seen>,
	spent: usize,
	now: Instant,
	debounce: Duration,
	/// Each forwarding node's claims at every revision, kept past its restart.
	histories: BTreeMap<NodeId, Vec<relay::Claims<Id, ()>>>,
	mutant: Option<relay::Mutant>,
	/// Whether each router rebuilds its indexes after every input to compare.
	checked: bool,
	/// A relay that resets comes back under a new identity, as a device replaced by
	/// another, rather than as a new start of the same relay.
	pub mint: bool,
	/// Every router sends what it has after each change to the network, as the bake-off
	/// runs it; the search flushes each router as a step of its own instead.
	pub auto: bool,
}

fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

/// A claim as its publisher makes it.
fn own() -> Claim<()> {
	Claim {
		item: (),
		cost: 0,
		priority: i8::MAX,
		draining: false,
		chain: Vec::new(),
	}
}

impl RelaySim {
	/// `nodes` relays, whose readers show a lost claim unavailable for `debounce`.
	pub fn new(nodes: usize, debounce: Duration) -> Self {
		let mut sim = Self {
			spots: Vec::new(),
			restarts: vec![0; nodes],
			ports: BTreeMap::new(),
			ties: BTreeMap::new(),
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			held: BTreeSet::new(),
			lives: BTreeMap::new(),
			stats: Stats::default(),
			sent: BTreeMap::new(),
			events: Vec::new(),
			spent: 0,
			now: Instant::ZERO,
			debounce,
			histories: BTreeMap::new(),
			mutant: None,
			checked: true,
			mint: false,
			auto: true,
		};
		sim.spots = (0..nodes).map(|node| Spot::Relay(Box::new(sim.fresh(node)))).collect();
		sim
	}

	/// How `node` names itself on a path as it runs now.
	pub fn id(&self, node: Node) -> NodeId {
		let generation = u64::from(self.restarts[node]);
		match self.mint {
			true => NodeId {
				relay: node as Id | generation << 32,
				start: 1,
			},
			false => NodeId {
				relay: node as Id,
				start: generation + 1,
			},
		}
	}

	fn fresh(&self, node: Node) -> Router {
		let mut router = Router::new(self.id(node), self.debounce);
		router.clock(self.now);
		if let Some(mutant) = self.mutant {
			router.mutate(mutant);
		}
		if !self.checked {
			router.unchecked();
		}
		router
	}

	/// Stop every router rebuilding its indexes after each input, now and after a restart:
	/// for a benchmark's mesh, where it would cost more than the routers.
	pub fn unchecked(&mut self) {
		self.checked = false;
		for node in 0..self.spots.len() {
			if let Some(router) = self.relay(node) {
				router.unchecked();
			}
		}
	}

	/// Plant `mutant` in every router, now and after a restart.
	pub fn mutate(&mut self, mutant: relay::Mutant) {
		self.mutant = Some(mutant);
		for node in 0..self.spots.len() {
			if let Some(router) = self.relay(node) {
				router.mutate(mutant);
			}
		}
	}

	/// `origin`'s claims at revision `floor`, as it announced them.
	pub fn truth(&self, origin: NodeId, floor: Rev) -> Option<&relay::Claims<Id, ()>> {
		let current = (0..self.spots.len())
			.filter_map(|node| self.router(node))
			.find(|router| router.me() == origin)
			.map(Router::history);
		let history = current.or_else(|| self.histories.get(&origin).map(Vec::as_slice))?;
		history.get(usize::try_from(floor).ok()?)
	}

	/// Whether `node` has output waiting for its next flush.
	pub fn pending(&self, node: Node) -> bool {
		self.router(node).is_some_and(Router::pending)
	}

	/// `node` sends what it has: [`Self::flush`] for one router.
	pub fn flush_node(&mut self, node: Node) {
		let Some(router) = self.relay(node) else {
			return;
		};
		let out = router.drain();
		let mut exchanged = Vec::new();
		for (to, msg) in out {
			match msg {
				Msg::Ask(_) | Msg::Edited | Msg::Synced(_) => exchanged.push((node, to, msg)),
				msg => self.send(node, to, msg),
			}
		}
		for (from, to, msg) in exchanged {
			self.exchange(from, to, msg);
		}
		self.show();
	}

	/// `node`'s next reader deadline passes.
	pub fn fire(&mut self, node: Node) {
		let Some(router) = self.relay(node) else {
			return;
		};
		if let Some(at) = router.deadline() {
			router.timeout(at);
		}
		self.show();
	}

	/// The nodes with a reader deadline set.
	pub fn timers(&self) -> Vec<Node> {
		(0..self.spots.len())
			.filter(|&node| self.router(node).is_some_and(|router| router.deadline().is_some()))
			.collect()
	}

	/// The open links.
	pub fn up(&self) -> &BTreeSet<(Node, Node)> {
		&self.up
	}

	/// The claims the publisher at `node` makes now, each with its Instance.
	pub fn claims(&self, node: Node) -> &BTreeSet<(Path, Id)> {
		match &self.spots[node] {
			Spot::Endpoint { claims, .. } => claims,
			Spot::Relay(_) => panic!("node {node} is a relay"),
		}
	}

	/// Hash what decides the network's future, leaving out what only records its past.
	pub fn fingerprint(&self, hasher: &mut impl std::hash::Hasher) {
		for spot in &self.spots {
			match spot {
				Spot::Relay(router) => router.fingerprint(hasher),
				Spot::Endpoint { id, claims, asked } => digest(hasher, &(id, claims, asked)),
			}
		}
		digest(hasher, &(&self.restarts, &self.up, &self.queues, &self.held));
	}

	pub fn router(&self, node: Node) -> Option<&Router> {
		match &self.spots[node] {
			Spot::Relay(router) => Some(router),
			Spot::Endpoint { .. } => None,
		}
	}

	fn relay(&mut self, node: Node) -> Option<&mut Router> {
		match &mut self.spots[node] {
			Spot::Relay(router) => Some(router),
			Spot::Endpoint { .. } => None,
		}
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

	pub fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		// Each side's port describes the node at the other end.
		self.ports.insert((a, b), (cost, tie == Tie::Up));
		self.ports.insert((b, a), (cost, false));
		self.ties.insert(pair(a, b), tie);
		self.open(a, b);
	}

	/// How `x` sees `y` on the session opening now.
	fn port(&self, x: Node, y: Node) -> Port {
		let (cost, upstream) = self.ports[&(x, y)];
		let role = match (&self.spots[y], self.ties[&pair(x, y)]) {
			(Spot::Endpoint { .. }, _) => Role::Endpoint,
			(_, Tie::Scoped) => Role::Scoped(vec![Path::empty()]),
			_ => Role::Routing(self.id(y)),
		};
		Port {
			cost,
			priority: 0,
			upstream,
			role,
		}
	}

	pub fn open(&mut self, a: Node, b: Node) {
		assert!(self.up.insert(pair(a, b)), "{a}-{b} already up");
		for (x, y) in [(a, b), (b, a)] {
			let port = self.port(x, y);
			if let Some(router) = self.relay(x) {
				router.connect(y, port).expect("a new session");
			}
		}
		if self.auto {
			self.flush();
		}
	}

	/// `peer` lets go of its session to `node`.
	fn drop_session(&mut self, node: Node, peer: Node) {
		match &mut self.spots[peer] {
			Spot::Relay(router) => router.disconnect(node),
			Spot::Endpoint { asked, .. } => {
				asked.remove(&node);
			}
		}
	}

	pub fn close(&mut self, a: Node, b: Node) {
		self.up.remove(&pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.held.remove(&(x, y));
			self.drop_session(x, y);
		}
		if self.auto {
			self.flush();
		}
	}

	/// `peer` notices its session to `node` closed, which `node` has yet to.
	pub fn sever(&mut self, node: Node, peer: Node) {
		self.up.remove(&pair(node, peer));
		for key in [(node, peer), (peer, node)] {
			self.queues.remove(&key);
			self.held.remove(&key);
		}
		self.drop_session(node, peer);
		if self.auto {
			self.flush();
		}
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn reset(&mut self, node: Node) {
		for peer in self.peers(node) {
			self.close(node, peer);
		}
		self.restarts[node] += 1;
		if let Spot::Relay(router) = &self.spots[node] {
			self.spent += router.work_done();
			self.histories.insert(router.me(), router.history().to_vec());
			self.spots[node] = Spot::Relay(Box::new(self.fresh(node)));
		}
	}

	pub fn goaway(&mut self, node: Node) {
		for peer in self.peers(node) {
			if let Some(router) = self.relay(peer) {
				router.goaway(node);
			}
		}
		if self.auto {
			self.flush();
		}
	}

	pub fn client(&mut self, node: Node) {
		self.spots[node] = Spot::Endpoint {
			id: None,
			claims: BTreeSet::new(),
			asked: BTreeMap::new(),
		};
	}

	pub fn publish(&mut self, node: Node, named: Id) {
		let Spot::Endpoint { id, .. } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		*id = Some(named);
	}

	pub fn claim(&mut self, node: Node, claim: &str, live: bool) {
		let claim = p(claim);
		let Spot::Endpoint { id, claims, asked } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		let change = match live {
			true if !claims.iter().any(|(path, _)| *path == claim) => {
				let id = id.expect("a publisher");
				claims.insert((claim.clone(), id));
				*self.lives.entry((id, claim.clone())).or_default() += 1;
				(id, relay::Change::Live(own()))
			}
			false => match claims.iter().find(|(path, _)| *path == claim).cloned() {
				Some(gone) => {
					claims.remove(&gone);
					(gone.1, relay::Change::Ended)
				}
				None => return,
			},
			_ => return,
		};
		let (id, change) = change;
		let tell: Vec<Node> = (asked.iter())
			.filter(|(_, prefixes)| prefixes.iter().any(|prefix| within(&claim, prefix)))
			.map(|(peer, _)| *peer)
			.collect();
		for peer in tell {
			let changes = vec![((claim.clone(), id), change.clone())];
			self.send(node, peer, Msg::Announce { changes });
		}
	}

	fn send(&mut self, from: Node, to: Node, msg: Message) {
		// What a dead process still had to say goes nowhere once its link is down, and
		// never onto the session that replaces it.
		if !self.up.contains(&pair(from, to)) {
			return;
		}
		match &msg {
			Msg::Route { .. } | Msg::Dead { .. } | Msg::Drain { .. } => {
				self.stats.route_msgs += 1;
				self.stats.route_items += 1;
			}
			Msg::Claims { changes, .. } | Msg::Announce { changes } => {
				self.stats.claim_msgs += 1;
				self.stats.claim_items += changes.len();
			}
			Msg::Ask(_) | Msg::Edited | Msg::Synced(_) => unreachable!("the interest exchange crosses at once"),
		}
		*self.sent.entry(from).or_default() += 1;
		self.queues.entry((from, to)).or_default().push_back(msg);
	}

	/// Hand `to` a message of the interest exchange, and answer it at once if `to` is a
	/// publisher.
	fn exchange(&mut self, from: Node, to: Node, msg: Message) {
		let Spot::Endpoint { claims, asked, .. } = &mut self.spots[to] else {
			let router = self.relay(to).expect("a relay");
			if let Err(err) = router.recv(from, msg.clone()) {
				panic!("{to} refused {msg:?} from {from}: {err}");
			}
			return;
		};
		// A publisher asks nothing, and answers what it is asked.
		let Msg::Ask(edit) = msg else {
			return;
		};
		let asks = asked.entry(from).or_default();
		for prefix in &edit.remove {
			asks.remove(prefix);
		}
		asks.extend(edit.add.iter().cloned());
		let changes: Vec<_> = (claims.iter())
			.filter(|(claim, _)| edit.add.iter().any(|prefix| within(claim, prefix)))
			.map(|claim| (claim.clone(), relay::Change::Live(own())))
			.collect();
		self.exchange(to, from, Msg::Edited);
		if !changes.is_empty() {
			self.send(to, from, Msg::Announce { changes });
		}
		for prefix in edit.add {
			self.exchange(to, from, Msg::Synced(prefix));
		}
	}

	/// The endpoint at `node` asks `relay` for `prefix`.
	pub fn ask(&mut self, node: Node, relay: Node, prefix: &str) {
		let edit = relay::Edit {
			remove: Vec::new(),
			add: vec![p(prefix)],
		};
		self.exchange(node, relay, Msg::Ask(edit));
		if self.auto {
			self.flush();
		}
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn ready(&self) -> Vec<((Node, Node), usize)> {
		(self.queues.iter())
			.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
			.map(|(key, queue)| (*key, queue.len()))
			.collect()
	}

	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(msg) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};

		if let Some(router) = self.relay(to)
			&& let Err(err) = router.recv(from, msg.clone())
		{
			panic!("{to} refused {msg:?} from {from}: {err}");
		}
		self.stats.delivered += 1;
		// A relay reads a burst first and answers when the round ends.
		self.show();
		true
	}

	fn live(&self, broadcast: &multi::Broadcast) -> bool {
		self.spots.iter().enumerate().any(|(node, spot)| match spot {
			Spot::Endpoint { claims, .. } => {
				claims.contains(&(broadcast.1.clone(), broadcast.0)) && !self.peers(node).is_empty()
			}
			Spot::Relay(_) => false,
		})
	}

	/// Move every router's output onto the queues, the interest exchange across at once,
	/// and log the reader events.
	pub fn flush(&mut self) {
		loop {
			let mut exchanged = Vec::new();
			for node in 0..self.spots.len() {
				let Some(router) = self.relay(node) else {
					continue;
				};
				for (to, msg) in router.drain() {
					match msg {
						Msg::Ask(_) | Msg::Edited | Msg::Synced(_) => exchanged.push((node, to, msg)),
						msg => self.send(node, to, msg),
					}
				}
			}
			if exchanged.is_empty() {
				break;
			}
			for (from, to, msg) in exchanged {
				self.exchange(from, to, msg);
			}
		}
		self.show();
	}

	/// One hop of delay passes: what the round left goes out, and a reader whose debounce
	/// ran out shows its claim offline. Whether a debounce still runs.
	pub fn tick(&mut self) -> bool {
		self.flush();
		self.now = self.now.saturating_add(HOP);
		let now = self.now;
		let mut waiting = false;
		for node in 0..self.spots.len() {
			if let Some(router) = self.relay(node) {
				router.timeout(now);
				waiting |= router.deadline().is_some();
			}
		}
		self.show();
		waiting
	}

	fn show(&mut self) {
		for node in 0..self.spots.len() {
			let Some(router) = self.relay(node) else {
				continue;
			};
			let forgot = router.forgets();
			for event in router.events() {
				let broadcast = (event.id, event.claim);
				let seen = Seen {
					node,
					live: self.live(&broadcast),
					life: self.lives.get(&broadcast).copied().unwrap_or(0),
					broadcast,
					kind: event.kind,
					forgot,
				};
				self.events.push(seen);
			}
		}
	}

	/// The nodes a request from `from` for `claim` of `id` visits, and whether it ended at
	/// a publisher. `None` when `from` knows no claim it can reach. A chain longer than
	/// the network loops.
	pub fn chase(&self, from: Node, id: Id, claim: &Path) -> Option<(Vec<Node>, bool)> {
		let broadcast = (claim.clone(), id);
		let (mut at, mut named, mut hops) = (from, None, Vec::new());
		while hops.len() <= self.spots.len() {
			let router = match &self.spots[at] {
				Spot::Endpoint { claims, .. } => {
					return Some((hops, claims.contains(&(claim.clone(), id))));
				}
				Spot::Relay(router) => router,
			};
			let Some(origin) = named.or_else(|| router.pick(&broadcast)) else {
				return (!hops.is_empty()).then_some((hops, false));
			};
			match router.step(origin, &broadcast) {
				// A request names the forwarding node its first hop chose.
				Some(Next::Via(next)) => {
					named = Some(origin);
					hops.push(next);
					at = next;
				}
				// Past a claim's announcer the next node chooses again.
				Some(Next::Local(next)) => {
					named = None;
					hops.push(next);
					at = next;
				}
				None => return Some((hops, false)),
			}
		}
		Some((hops, false))
	}

	pub fn work(&self) -> usize {
		self.spent
			+ (0..self.spots.len())
				.filter_map(|node| self.router(node))
				.map(Router::work_done)
				.sum::<usize>()
	}

	pub fn kept(&self) -> relay::Kept {
		let mut out = relay::Kept::default();
		for router in (0..self.spots.len()).filter_map(|node| self.router(node)) {
			let kept = router.kept();
			out.routes += kept.routes;
			out.used += kept.used;
			out.hops += kept.hops;
			out.sent += kept.sent;
			out.items += kept.items;
			out.heard += kept.heard;
			out.told += kept.told;
			out.pending += kept.pending;
			out.closed += kept.closed;
			out.waiting += kept.waiting;
		}
		out
	}
}

impl Net for RelaySim {
	fn client(&mut self, node: Node) {
		RelaySim::client(self, node);
	}

	fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		RelaySim::link(self, a, b, cost, tie);
	}

	fn goaway(&mut self, node: Node) {
		RelaySim::goaway(self, node);
	}

	fn open(&mut self, a: Node, b: Node) {
		RelaySim::open(self, a, b);
	}

	fn close(&mut self, a: Node, b: Node) {
		RelaySim::close(self, a, b);
	}

	fn mute(&mut self, node: Node, peer: Node) {
		self.hold(node, peer);
		self.hold(peer, node);
	}

	fn sever(&mut self, node: Node, peer: Node) {
		RelaySim::sever(self, node, peer);
	}

	fn reset(&mut self, node: Node) {
		RelaySim::reset(self, node);
	}

	fn publish(&mut self, node: Node, id: Id) {
		RelaySim::publish(self, node, id);
	}

	fn claim(&mut self, node: Node, claim: &str, live: bool) {
		RelaySim::claim(self, node, claim, live);
	}

	fn ready(&self) -> Vec<(Chan, usize)> {
		RelaySim::ready(self)
	}

	fn step(&mut self, (from, to): Chan) {
		self.deliver(from, to);
	}

	fn tick(&mut self) -> bool {
		RelaySim::tick(self)
	}

	fn chase(&self, from: Node, id: Id) -> Option<(Vec<Node>, bool)> {
		RelaySim::chase(self, from, id, &p(&Mesh::claim(0)))
	}

	fn showing(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		let shown = self.router(node)?.shown().get(&(id, p(claim)))?;
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
		let kept = self.kept();
		Held {
			routes: kept.used,
			offers: kept.routes,
			claims: kept.items + kept.heard + kept.told,
			pending: kept.pending,
			lasting: kept.closed,
		}
	}

	fn floors(&self, nodes: &[Node]) -> usize {
		(nodes.iter())
			.filter_map(|&node| self.router(node))
			.map(|router| router.kept().closed)
			.sum()
	}

	fn faults(&self) -> Faults {
		Faults {
			false_ends: multi::false_ends(&self.events),
			revivals: multi::revivals(&self.events),
		}
	}

	fn work(&self) -> usize {
		RelaySim::work(self)
	}
}

/// It carries every change of the path rule's comparison, and is right whenever it
/// settles.
#[test]
fn the_router_converges_on_every_shape() {
	let mut rng = XorShift(0x6d6f_7100);
	let mut meshes = vec![Mesh::ring(6), Mesh::clique(6, 1), Mesh::clique(4, 3)];
	meshes.extend((0..8).map(|_| Mesh::sparse(6, &mut rng)));
	for mesh in &meshes {
		run(Arm::RELAY, mesh);
	}
}

/// Under churn it settles and is right once it does.
#[test]
fn the_router_settles_after_churn() {
	for two_homes in [false, true] {
		churn(Arm::RELAY, 0..60, two_homes);
	}
}

/// Every failure on every shape of the bake-off: it settles, every relay with a path
/// routes to its publisher, no reader sees an end the publisher never made or a life it
/// had seen end, and nothing waits on another node once it settles.
#[test]
fn the_router_carries_every_failure_on_every_shape() {
	carries(Arm::RELAY, 9);
}

/// The same at the quest's largest size, where the debounce still outlasts every failure.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn sweep_the_router_carries_every_failure_at_32_relays() {
	carries(Arm::RELAY, 32);
	assert_eq!(flaps(Arm::RELAY, 32), Vec::<String>::new());
}

/// Each failure on each shape at `relays` that showed a reader a claim start again, though
/// it brings nothing back, with how many times.
fn flaps(arm: Arm, relays: usize) -> Vec<String> {
	let mut out = Vec::new();
	for shape in shapes(relays, 2) {
		for (failure, row) in fail(arm, &shape.mesh, relays > 16) {
			let returns = matches!(failure, Failure::Heal | Failure::Restart);
			if !returns && row.cost.sent.starts > 0 {
				out.push(format!("{} on {}: {}", failure.as_str(), shape.name, row.cost.sent.starts));
			}
		}
	}
	out
}

/// The readers' debounce, a hop for each node a route may cross, outlasts the hunting a
/// failure causes: a failure that brings nothing back never shows a reader a claim start
/// again, only unavailable for a while. Without it a reader lets go and takes the claim
/// back on most failures, and on every relay when a publisher moves.
#[test]
fn the_debounce_outlasts_a_failure() {
	assert_eq!(flaps(Arm::RELAY, 9), Vec::<String>::new());
	let undebounced = flaps(Arm::Relay(Duration::ZERO), 9);
	assert!(
		undebounced.iter().any(|flap| flap.starts_with(Failure::Move.as_str())),
		"{undebounced:?}"
	);
}

/// What one failure's row says, by field.
fn routing(row: &Row) -> Vec<(&'static str, String)> {
	let cost = &row.cost;
	vec![
		("route_msgs", format!("{}", cost.sent.msgs)),
		("claim_items", format!("{}", cost.sent.claims)),
		("settle_rounds", format!("{}", cost.rounds)),
		("starts", format!("{}", cost.sent.starts)),
		("offline", format!("{}", cost.sent.offline)),
		("fed_rounds", format!("{}", cost.right)),
		("stale", format!("{}", cost.stale)),
		("dark", format!("{}", cost.dark)),
		("loops", format!("{}", cost.loops)),
		("settled", format!("{}", cost.settled)),
		("faults", format!("{:?}", row.faults)),
		("pending", format!("{}", row.pending)),
		("hops", format!("{}", row.hops)),
		("right", format!("{:?}", row.right)),
		("answers", format!("{:?}", row.answers)),
	]
}

/// The fields of two rows that differ, each as the prototype's and this router's.
fn differ(kept: &Row, relay: &Row) -> Vec<String> {
	(routing(kept).into_iter().zip(routing(relay)))
		.filter(|((_, a), (_, b))| a != b)
		.map(|((field, a), (_, b))| match field {
			"answers" => "answers".to_string(),
			_ => format!("{field} {a}->{b}"),
		})
		.collect()
}

/// The production router, its readers letting go at once as the prototype's do, gives
/// the prototype's answers on every shape and failure of the bake-off at the same cost:
/// the same next hops for every request once it settles, the same route messages, the
/// same rounds, and the same reader events.
///
/// It sends fewer claims in one case. A relay that loses every route to a forwarding node
/// and then hears a lagging neighbour's copy of its claims within one burst passes them
/// on in the prototype, at a floor its neighbours already hold, on routes it withdraws
/// in the same burst (`home_dies` on `tiers`); this router withdraws those routes first.
#[test]
fn the_router_gives_the_prototypes_answers() {
	let mut out = Vec::new();
	for shape in shapes(9, 2) {
		let kept = fail(Arm::KEPT, &shape.mesh, false);
		let relay = fail(Arm::Relay(Duration::ZERO), &shape.mesh, false);
		assert_eq!(kept.len(), relay.len(), "{}", shape.name);
		for ((failure, kept), (_, relay)) in kept.iter().zip(&relay) {
			let fields = differ(kept, relay);
			// Fewer claims is the one field allowed to differ.
			let fewer = kept.cost.sent.claims > relay.cost.sent.claims;
			if fields.len() > usize::from(fewer) {
				out.push(format!("{} on {}: {}", failure.as_str(), shape.name, fields.join(", ")));
			}
		}
	}
	assert!(out.is_empty(), "{}", out.join("\n"));
}

/// Deliver and flush round by round, the clock advancing a hop each, until nothing is in
/// flight and no debounce runs.
pub(super) fn quiesce(sim: &mut RelaySim) {
	for _ in 0..200 {
		let ready = sim.ready();
		for &((from, to), count) in &ready {
			for _ in 0..count {
				sim.deliver(from, to);
			}
		}
		let waiting = sim.tick();
		if ready.is_empty() && !waiting && sim.ready().is_empty() {
			return;
		}
	}
	panic!("never settles");
}

/// Two relays that share a draining neighbour and a scoped link each hold the other's
/// copy of its claim. The copy says it is draining too, so neither prefers it to its own
/// route and the two never trade places.
#[test]
fn a_claim_across_a_grant_keeps_its_drain() {
	let mut sim = RelaySim::new(4, relay::debounce(HOP));
	sim.client(3);
	sim.publish(3, 100);
	sim.link(0, 1, 1, Tie::Peer);
	sim.link(0, 2, 1, Tie::Peer);
	sim.link(1, 2, 1, Tie::Scoped);
	sim.link(3, 0, 1, Tie::Peer);
	sim.claim(3, "live/0", true);
	quiesce(&mut sim);
	// Both read the GOAWAY before either hears from the other.
	sim.goaway(0);
	quiesce(&mut sim);
	let broadcast = (p("live/0"), 100);
	for relay in [1, 2] {
		let router = sim.router(relay).expect("a relay");
		assert_eq!(router.pick(&broadcast), Some(sim.id(0)), "relay {relay}");
	}
}

/// A scoped neighbour that sends GOAWAY is draining for the claims it gave, so a node
/// holding the same claim from another neighbour announces that one instead.
#[test]
fn a_scoped_neighbour_draining_drains_its_claims() {
	// A publisher on 0 and 2, each of which grants 1 the claim, 0 the cheaper.
	let mut sim = RelaySim::new(4, relay::debounce(HOP));
	sim.client(3);
	sim.publish(3, 100);
	sim.link(3, 0, 1, Tie::Peer);
	sim.link(3, 2, 1, Tie::Peer);
	sim.link(0, 1, 1, Tie::Scoped);
	sim.link(2, 1, 2, Tie::Scoped);
	sim.claim(3, "live/0", true);
	quiesce(&mut sim);
	let broadcast = (p("live/0"), 100);
	let source = |sim: &RelaySim| sim.router(1).expect("a relay").step(sim.id(1), &broadcast);
	assert_eq!(source(&sim), Some(Next::Local(0)));
	sim.goaway(0);
	quiesce(&mut sim);
	assert_eq!(source(&sim), Some(Next::Local(2)));
}

/// A relay that takes a newer copy of a node's claims from another neighbour tells its
/// own neighbours the floor even when nothing in the copy differs from what it had, so
/// one that heard a claim in between, by a route since gone, lets go of it.
#[test]
fn a_newer_floor_travels_with_nothing_changed() {
	// 0 announces. 3 hears it through 1, and later 2; 4 hears it through 3 and 5.
	let mut sim = RelaySim::new(7, relay::debounce(HOP));
	sim.client(6);
	sim.publish(6, 100);
	for (a, b) in [(0, 1), (0, 2), (1, 3), (3, 4), (0, 5), (5, 4), (6, 0)] {
		sim.link(a, b, 1, Tie::Peer);
	}
	sim.claim(6, "live/a", true);
	quiesce(&mut sim);
	let shown = |sim: &RelaySim, relay: Node| sim.showing(relay, 100, "live/b");

	// 3 misses the claim starting, which reaches 4 through 5.
	sim.hold(1, 3);
	sim.hold(4, 3);
	sim.claim(6, "live/b", true);
	quiesce(&mut sim);
	assert_eq!((shown(&sim, 3), shown(&sim, 4)), (None, Some(true)));

	// 5 misses it ending, and then 4 loses its route through 5.
	sim.hold(0, 5);
	sim.claim(6, "live/b", false);
	quiesce(&mut sim);
	sim.close(5, 4);
	quiesce(&mut sim);
	assert_eq!(shown(&sim, 4), Some(true), "nothing has told it yet");

	// 3 hears a newer copy from 2 that matches what it holds.
	sim.link(2, 3, 1, Tie::Peer);
	quiesce(&mut sim);
	assert_eq!(shown(&sim, 4), None);
}

/// A relay that restarts is a new node, but its neighbours file the session floor per
/// pair of relays with the restart in the session's order, so a restart replaces the
/// entry instead of adding one.
#[test]
fn a_session_floor_keeps_an_entry_per_pair_of_relays() {
	let mut sim = RelaySim::new(3, relay::debounce(HOP));
	for (a, b) in [(0, 1), (1, 2), (0, 2)] {
		sim.link(a, b, 1, Tie::Peer);
	}
	quiesce(&mut sim);
	let mut kept = Vec::new();
	for _ in 0..8 {
		sim.reset(0);
		quiesce(&mut sim);
		for peer in [1, 2] {
			sim.open(0, peer);
		}
		quiesce(&mut sim);
		kept.push(sim.kept().closed);
	}
	// Two ways round each of the three links at most.
	assert!(kept.iter().all(|&pairs| pairs <= 6), "{kept:?}");
	assert_eq!(kept[0], kept[7], "{kept:?}");
}


/// The floor's growth with identities that never return: each round a relay under a new
/// identity joins a ring as a chord with a publisher of its own, announces, and leaves for
/// good. Every relay that heard a route across one of its sessions keeps that pair's floor
/// for as long as it runs, at most one entry per direction of each departed session: the
/// epoch bounds only restarts of one relay, so nothing here replaces or removes an entry.
/// The quest's PR brings eviction as a decision.
#[test]
fn a_relay_that_leaves_for_good_leaves_its_floors() {
	let (stable, rounds) = (8, 12);
	let mut sim = RelaySim::new(stable + 2 * rounds + 1, relay::debounce(HOP));
	let home = stable + 2 * rounds;
	sim.client(home);
	sim.publish(home, 1);
	for a in 0..stable {
		sim.link(a, (a + 1) % stable, 1, Tie::Peer);
	}
	sim.link(home, 0, 1, Tie::Peer);
	sim.claim(home, "live/0", true);
	quiesce(&mut sim);
	let floors = |sim: &RelaySim| -> Vec<usize> {
		(0..stable)
			.map(|relay| sim.router(relay).expect("a relay").kept().closed)
			.collect()
	};
	assert_eq!(floors(&sim), vec![0; stable]);
	let mut sums = Vec::new();
	for round in 0..rounds {
		let (relay, client) = (stable + 2 * round, stable + 2 * round + 1);
		let (a, b) = (round % stable, (round + stable / 2) % stable);
		sim.client(client);
		sim.publish(client, 100 + round as Id);
		sim.link(relay, a, 1, Tie::Peer);
		sim.link(relay, b, 1, Tie::Peer);
		sim.link(client, relay, 1, Tie::Peer);
		sim.claim(client, &format!("live/r{round}"), true);
		quiesce(&mut sim);
		for peer in [a, b, client] {
			sim.close(relay, peer);
		}
		quiesce(&mut sim);
		let kept = floors(&sim);
		println!("after {} departures: {kept:?}", round + 1);
		// Two sessions each, a pair each way.
		assert!(kept.iter().all(|&entries| entries <= 4 * (round + 1)), "{kept:?}");
		sums.push(kept.iter().sum::<usize>());
	}
	assert!(sums.windows(2).all(|pair| pair[1] > pair[0]), "{sums:?}");
}

/// What the floors cost on the bake-off's shapes as half the relays are replaced, one at
/// a time and over and over, each restarting under its own identity or coming back under
/// a new one: the floor entries a relay of the other half, which never restarts, holds on
/// average after each round of replacements. A restart clears a relay's own table, so a
/// replaced relay's says nothing about growth.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn benchmark_relay_floor_growth() {
	println!("shape,relays,links,identity,replaced,floors_per_lasting_relay");
	for relays in [12, 32] {
		for shape in shapes(relays, 2) {
			let mesh = &shape.mesh;
			// The odd relays come and go; the even ones, every publisher's first home
			// among them, stay.
			let replaced: Vec<Node> = (1..mesh.relays).step_by(2).collect();
			let lasting: Vec<Node> = (0..mesh.relays).step_by(2).collect();
			for mint in [false, true] {
				let mut sim = RelaySim::new(mesh.relays + mesh.homes.len(), relay::debounce(HOP));
				sim.mint = mint;
				sim.unchecked();
				let mut lab = Lab::with(Box::new(sim), mesh);
				let mut count = 0;
				for _ in 0..4 {
					for &relay in &replaced {
						lab.crash(relay);
						lab.settle();
						lab.revive(relay);
						lab.settle();
						count += 1;
					}
					let held = lab.net.floors(&lasting);
					println!(
						"{},{},{},{},{count},{:.1}",
						shape.name,
						mesh.relays,
						mesh.links.len(),
						if mint { "new" } else { "same" },
						held as f64 / lasting.len() as f64,
					);
				}
			}
		}
	}
}

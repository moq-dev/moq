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

use super::bakeoff::{Row, carries, fail, shapes};
use super::compare::{Arm, Chan, Faults, Held, Mesh, Net, Sent, churn, run, tally};
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
	pub events: Vec<Seen>,
	spent: usize,
	now: Instant,
	debounce: Duration,
	/// Each forwarding node's claims at every revision, kept past its restart.
	histories: BTreeMap<NodeId, Vec<relay::Claims<Id, ()>>>,
	mutant: Option<relay::Mutant>,
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
			events: Vec::new(),
			spent: 0,
			now: Instant::ZERO,
			debounce,
			histories: BTreeMap::new(),
			mutant: None,
			auto: true,
		};
		sim.spots = (0..nodes).map(|node| Spot::Relay(Box::new(sim.fresh(node)))).collect();
		sim
	}

	/// How `node` names itself on a path as it runs now.
	pub fn id(&self, node: Node) -> NodeId {
		NodeId {
			relay: node as Id,
			start: u64::from(self.restarts[node]) + 1,
		}
	}

	fn fresh(&self, node: Node) -> Router {
		let mut router = Router::new(self.id(node), self.debounce);
		router.clock(self.now);
		if let Some(mutant) = self.mutant {
			router.mutate(mutant);
		}
		router
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

/// The same at the quest's largest size.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn sweep_the_router_carries_every_failure_at_32_relays() {
	carries(Arm::RELAY, 32);
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


//! A reference model of the proposed per-origin route layer, small enough to read.
//!
//! It is not the implementation. It exists so the routing contracts have
//! deterministic counterexamples before the lite-07 wire is fixed: every rule a
//! contract depends on is a switch here, and each fixture runs a scenario under
//! the rule that fails and the rule that holds.
//!
//! Routing is Babel (RFC 8966) keyed by routing identity: each node selects one
//! feasible route per identity and view, advertises it to every neighbor whose
//! session is up, and only the identity's own node advances its seqno. Messages
//! travel in order on each directed session and can be held to replay them late.
//! Claims (an identity serving a prefix, at a seed cost and optional epoch) are
//! known everywhere unless a fixture hides one from a node, which stands in for
//! scoping, interest, or a claim that has not arrived yet.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
};

pub type Node = usize;

/// An opaque routing identity, one per process incarnation.
pub type Id = u64;

/// A route metric: the configured cost, then the hop count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Metric {
	pub cost: u64,
	pub hops: u32,
}

impl Metric {
	pub const ZERO: Self = Self { cost: 0, hops: 0 };

	/// The total a requester compares: the claim's seed plus the route metric.
	pub fn seeded(self, seed: u64) -> Self {
		Self {
			cost: self.cost + seed,
			hops: self.hops,
		}
	}
}

/// How metrics compare and accumulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
	/// Today's single cost, saturating at the ceiling. Hops never order routes,
	/// so a zero-cost link or a saturated cost does not increase the metric.
	CostOnly { ceiling: u64 },
	/// Cost, then hops, compared lexicographically. Crossing a link always
	/// increases it, and exceeding the ceiling is unreachable rather than capped.
	Lexicographic { ceiling: u64 },
}

impl Rule {
	pub const LEXICOGRAPHIC: Self = Self::Lexicographic { ceiling: u64::MAX / 2 };

	fn key(self, metric: Metric) -> (u64, u32) {
		match self {
			Self::CostOnly { .. } => (metric.cost, 0),
			Self::Lexicographic { .. } => (metric.cost, metric.hops),
		}
	}

	fn less(self, a: Metric, b: Metric) -> bool {
		self.key(a) < self.key(b)
	}

	/// The metric after crossing a link of `cost`, or `None` when unreachable.
	fn add(self, metric: Metric, cost: u64) -> Option<Metric> {
		let hops = metric.hops + 1;
		match self {
			Self::CostOnly { ceiling } => Some(Metric {
				cost: metric.cost.saturating_add(cost).min(ceiling),
				hops,
			}),
			Self::Lexicographic { ceiling } => {
				let cost = metric.cost.checked_add(cost).filter(|cost| *cost <= ceiling)?;
				Some(Metric { cost, hops })
			}
		}
	}
}

/// What a node may advertise to an upstream link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
	/// No restriction: every link transits.
	Anything,
	/// routes.md's sketch: one selected route per identity, carrying a down-only
	/// bit set when learned on an upstream link, never sent to an upstream link.
	DownOnly,
	/// Two selections per identity: one over every route, and an export view over
	/// routes not learned on an upstream link. Each has its own feasibility.
	Views,
}

/// The cross-origin tie-break after cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TieBreak {
	/// Hash the path and the serving identity: every node agrees.
	PathIdentity,
	/// Hash the path, identity, next hop, and this node, standing in for
	/// today's hash over a hop chain that differs per node: nodes may disagree.
	PathHere,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
	pub rule: Rule,
	pub export: Export,
	pub tie: TieBreak,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			rule: Rule::LEXICOGRAPHIC,
			export: Export::Views,
			tie: TieBreak::PathIdentity,
		}
	}
}

/// One side's configuration of a link to a neighbor.
#[derive(Clone, Copy, Debug)]
pub struct Link {
	pub cost: u64,
	/// The application's local priority for this connection; higher wins.
	pub priority: i8,
	/// This side marks the neighbor upstream.
	pub upstream: bool,
	/// This side marks the neighbor downstream, as RFC 9234's provider marks a
	/// customer: everything sent to it is down-only, and anything it sends is
	/// taken from its export view alone.
	pub downstream: bool,
	/// This side admitted the neighbor under a namespace scope: an identity it
	/// advertises is keyed to the admission, so it cannot stand in for the same
	/// identity learned elsewhere. See [`scoped_id`].
	pub scoped: bool,
}

impl Default for Link {
	fn default() -> Self {
		Self {
			cost: 1,
			priority: 0,
			upstream: false,
			downstream: false,
			scoped: false,
		}
	}
}

impl Link {
	pub fn cost(cost: u64) -> Self {
		Self {
			cost,
			..Self::default()
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

	pub fn downstream(mut self) -> Self {
		self.downstream = true;
		self
	}

	pub fn scoped(mut self) -> Self {
		self.scoped = true;
		self
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum View {
	All,
	Export,
}

/// One advertisement: a (seqno, metric) per view, `None` when unreachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Update {
	id: Id,
	all: Option<(u32, Metric)>,
	export: Option<(u32, Metric)>,
}

/// The route a node selected for an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selected {
	/// The neighbor it goes through, or `None` when this node is the identity.
	pub next: Option<Node>,
	pub seqno: u32,
	/// This node's metric.
	pub metric: Metric,
	/// The metric the next hop advertised.
	pub advertised: Metric,
	/// Whether an upstream link is on the path (only under [`Export::DownOnly`]).
	pub down_only: bool,
}

#[derive(Default)]
struct State {
	own: Option<(Id, u32)>,
	/// Whether this node advertises identities other than its own.
	transit: bool,
	rx: BTreeMap<Id, BTreeMap<Node, Update>>,
	fd: BTreeMap<(Id, View), (u32, Metric)>,
	sent: BTreeMap<(Node, Id), Update>,
}

/// An identity serving everything under a prefix.
#[derive(Clone, Copy, Debug)]
pub struct Claim {
	pub id: Id,
	/// A trailing `/` claims the subtree; otherwise only the exact path.
	pub prefix: &'static str,
	pub seed: u64,
	pub epoch: Option<u32>,
}

impl Claim {
	fn covers(&self, path: &str) -> bool {
		path == self.prefix || (self.prefix.ends_with('/') && path.starts_with(self.prefix))
	}
}

/// How a transit hop treats a request it forwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forward {
	/// Every hop reselects among all origins; optionally never back to the requester.
	PathOnly { exclude_requester: bool },
	/// No wire change: a transit hop forwards only to a candidate whose
	/// advertised total is strictly below its own best total (RFC 5286's
	/// downstream condition).
	Downhill,
	/// The first hop names the identity; later hops follow their route to it.
	Pinned,
	/// The first hop names a bound; later hops reselect within it.
	Bounded,
}

/// What a forwarded request carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Carried {
	pinned: Option<Id>,
	bound: Option<(u64, u32)>,
	epoch: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
	Served { by: Id, path: Vec<Node> },
	Loop { path: Vec<Node> },
	Stalled { path: Vec<Node> },
}

/// Which existing upstream subscription a forwarded request may join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
	/// Any subscription for the path, as today.
	Path,
	/// One serving the same explicit epoch.
	Epoch,
	/// One following a route to the same identity.
	Identity,
	/// One whose total is within the request's bound.
	Bound,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
	id: Id,
	next: Option<Node>,
	seed: u64,
	total: Metric,
	advertised: Metric,
	priority: i8,
	prefix: usize,
	epoch: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
enum Up {
	Serve,
	Stalled,
	Pending(Node, Node, Carried),
	Joined(usize),
}

#[derive(Clone, Copy, Debug)]
struct Front {
	node: Node,
	id: Id,
	epoch: Option<u32>,
	total: Metric,
	up: Up,
}

pub struct Net {
	cfg: Config,
	nodes: Vec<State>,
	links: BTreeMap<(Node, Node), Link>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Update>>,
	held: BTreeSet<(Node, Node)>,
	claims: Vec<Claim>,
	hidden: BTreeSet<(Node, usize)>,
}

impl Net {
	pub fn new(cfg: Config, nodes: usize) -> Self {
		Self {
			cfg,
			nodes: (0..nodes)
				.map(|_| State {
					transit: true,
					..State::default()
				})
				.collect(),
			links: BTreeMap::new(),
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			held: BTreeSet::new(),
			claims: Vec::new(),
			hidden: BTreeSet::new(),
		}
	}

	/// A node that advertises only its own identity, like a browser.
	pub fn leaf(&mut self, node: Node) {
		self.nodes[node].transit = false;
	}

	/// Configure both sides of a link and open its session.
	pub fn link(&mut self, a: Node, b: Node, ab: Link, ba: Link) {
		self.links.insert((a, b), ab);
		self.links.insert((b, a), ba);
		self.connect(a, b);
	}

	/// A symmetric link of `cost`.
	pub fn join(&mut self, a: Node, b: Node, cost: u64) {
		self.link(a, b, Link::cost(cost), Link::cost(cost));
	}

	fn pair(a: Node, b: Node) -> (Node, Node) {
		(a.min(b), a.max(b))
	}

	/// Open a session; each side replays what it currently advertises.
	pub fn connect(&mut self, a: Node, b: Node) {
		assert!(self.links.contains_key(&(a, b)), "unknown link {a}-{b}");
		self.up.insert(Self::pair(a, b));
		self.advertise(a);
		self.advertise(b);
	}

	/// Close a session: in-flight messages and everything learned on it go.
	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.up.remove(&Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.held.remove(&(x, y));
			let state = &mut self.nodes[y];
			for routes in state.rx.values_mut() {
				routes.remove(&x);
			}
			state.rx.retain(|_, routes| !routes.is_empty());
			self.nodes[x].sent.retain(|(n, _), _| *n != y);
		}
		self.advertise(a);
		self.advertise(b);
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn kill(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.disconnect(node, peer);
		}
		let transit = self.nodes[node].transit;
		self.nodes[node] = State {
			transit,
			..State::default()
		};
	}

	/// Re-open every configured link of `node`.
	pub fn reconnect(&mut self, node: Node) {
		let peers: Vec<Node> = self.links.keys().filter(|(a, _)| *a == node).map(|(_, b)| *b).collect();
		for peer in peers {
			self.connect(node, peer);
		}
	}

	fn peers(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
		self.up.iter().filter_map(move |&(a, b)| match node {
			n if n == a => Some(b),
			n if n == b => Some(a),
			_ => None,
		})
	}

	/// `node` starts originating `id` at seqno `seqno`.
	pub fn originate(&mut self, node: Node, id: Id, seqno: u32) {
		self.nodes[node].own = Some((id, seqno));
		self.advertise(node);
	}

	/// The origin advances its seqno.
	pub fn bump(&mut self, node: Node) {
		let own = self.nodes[node].own.as_mut().expect("not an origin");
		own.1 += 1;
		self.advertise(node);
	}

	/// Forget everything about `id` at `node`, as source garbage collection does.
	pub fn collect(&mut self, node: Node, id: Id) {
		let state = &mut self.nodes[node];
		state.rx.remove(&id);
		state.fd.retain(|(fid, _), _| *fid != id);
		self.advertise(node);
	}

	pub fn claim(&mut self, id: Id, prefix: &'static str, seed: u64, epoch: Option<u32>) -> usize {
		self.claims.push(Claim {
			id,
			prefix,
			seed,
			epoch,
		});
		self.claims.len() - 1
	}

	/// `node` does not know claim `index`.
	pub fn hide(&mut self, node: Node, index: usize) {
		self.hidden.insert((node, index));
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
	}

	/// Deliver one in-order message from `from` to `to`, even if held.
	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(update) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		self.receive(to, from, update);
		true
	}

	/// Deliver round-robin until every unheld queue is empty. False if it never
	/// settles within a generous budget, which is how oscillation shows up.
	pub fn run(&mut self) -> bool {
		for _ in 0..10_000 {
			let ready: Vec<(Node, Node)> = self
				.queues
				.iter()
				.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
				.map(|(key, _)| *key)
				.collect();
			if ready.is_empty() {
				return true;
			}
			for (from, to) in ready {
				self.deliver(from, to);
			}
		}
		false
	}

	fn receive(&mut self, at: Node, from: Node, mut update: Update) {
		let link = self.links[&(at, from)];
		if link.downstream {
			update.all = update.export;
		}
		if link.scoped {
			update.id = scoped_id(update.id, from);
		}
		let routes = self.nodes[at].rx.entry(update.id).or_default();
		if update.all.is_none() && update.export.is_none() {
			routes.remove(&from);
		} else {
			routes.insert(from, update);
		}
		self.advertise(at);
	}

	/// The route `node` selected for `id` over every route.
	pub fn route(&self, node: Node, id: Id) -> Option<Selected> {
		self.select(node, id, View::All)
	}

	/// The route `node` would offer an upstream link for `id`.
	pub fn export_route(&self, node: Node, id: Id) -> Option<Selected> {
		match self.cfg.export {
			Export::Views => self.select(node, id, View::Export),
			Export::DownOnly => self.route(node, id).filter(|route| !route.down_only),
			Export::Anything => self.route(node, id),
		}
	}

	/// Whether `node` holds a route to `id` but none is feasible: it needs the
	/// origin to advance its seqno.
	pub fn starved(&self, node: Node, id: Id) -> bool {
		self.route(node, id).is_none()
			&& self.nodes[node]
				.rx
				.get(&id)
				.is_some_and(|routes| routes.values().any(|update| update.all.is_some()))
	}

	fn select(&self, node: Node, id: Id, view: View) -> Option<Selected> {
		let state = &self.nodes[node];
		if let Some((own, seqno)) = state.own
			&& own == id
		{
			return Some(Selected {
				next: None,
				seqno,
				metric: Metric::ZERO,
				advertised: Metric::ZERO,
				down_only: false,
			});
		}
		let rule = self.cfg.rule;
		let fd = state.fd.get(&(id, view)).copied();
		let feasible = |seqno: u32, advertised: Metric| match fd {
			None => true,
			Some((fd_seqno, fd_metric)) => seqno > fd_seqno || (seqno == fd_seqno && rule.less(advertised, fd_metric)),
		};
		state
			.rx
			.get(&id)?
			.iter()
			.filter_map(|(&next, update)| {
				let link = self.links[&(node, next)];
				let offered = match view {
					View::All => update.all,
					View::Export if link.upstream => None,
					View::Export => update.export,
				};
				let (seqno, advertised) = offered.filter(|(seqno, advertised)| feasible(*seqno, *advertised))?;
				let metric = rule.add(advertised, link.cost)?;
				let selected = Selected {
					next: Some(next),
					seqno,
					metric,
					advertised,
					down_only: link.upstream || update.export.is_none(),
				};
				Some(((Reverse(link.priority), rule.key(metric), next), selected))
			})
			.min_by_key(|(rank, _)| *rank)
			.map(|(_, selected)| selected)
	}

	/// Recompute every selection at `node`, record feasibility distances for what
	/// it advertises, and queue any change for each neighbor.
	fn advertise(&mut self, node: Node) {
		let state = &self.nodes[node];
		let mut ids: BTreeSet<Id> = state.rx.keys().copied().collect();
		ids.extend(state.own.map(|(id, _)| id));
		ids.extend(state.sent.keys().map(|(_, id)| *id));

		for id in ids {
			let own = self.nodes[node].own.is_some_and(|(own, _)| own == id);
			if !own && !self.nodes[node].transit {
				continue;
			}
			let all = self.select(node, id, View::All);
			let export = match self.cfg.export {
				Export::Views => self.select(node, id, View::Export),
				Export::DownOnly => all.filter(|route| !route.down_only),
				Export::Anything => all,
			};
			for (view, selected) in [(View::All, all), (View::Export, export)] {
				if let Some(selected) = selected {
					self.record(node, id, view, selected);
				}
			}

			let peers: Vec<Node> = self.peers(node).collect();
			for peer in peers {
				let link = self.links[&(node, peer)];
				let pair = |route: Option<Selected>| route.map(|route| (route.seqno, route.metric));
				let mut update = if link.upstream {
					Update {
						id,
						all: pair(export),
						export: pair(export),
					}
				} else {
					Update {
						id,
						all: pair(all),
						export: pair(export),
					}
				};
				if link.downstream {
					update.export = None;
				}
				let previous = self.nodes[node].sent.get(&(peer, id)).copied();
				let silent = update.all.is_none() && update.export.is_none();
				if previous == Some(update) || (previous.is_none() && silent) {
					continue;
				}
				if silent {
					self.nodes[node].sent.remove(&(peer, id));
				} else {
					self.nodes[node].sent.insert((peer, id), update);
				}
				self.queues.entry((node, peer)).or_default().push_back(update);
			}
		}
	}

	/// Babel's feasibility distance: reset by a newer seqno, lowered by a better metric.
	fn record(&mut self, node: Node, id: Id, view: View, selected: Selected) {
		let rule = self.cfg.rule;
		let fd = self.nodes[node]
			.fd
			.entry((id, view))
			.or_insert((selected.seqno, selected.metric));
		if selected.seqno > fd.0 || (selected.seqno == fd.0 && rule.less(selected.metric, fd.1)) {
			*fd = (selected.seqno, selected.metric);
		}
	}

	fn candidates(&self, node: Node, path: &str, from: Option<Node>) -> Vec<Candidate> {
		let from_upstream = from.is_some_and(|from| self.links[&(node, from)].upstream);
		let mut out = Vec::new();
		for (index, claim) in self.claims.iter().enumerate() {
			if !claim.covers(path) || self.hidden.contains(&(node, index)) {
				continue;
			}
			let route = if from_upstream {
				self.export_route(node, claim.id)
			} else {
				self.route(node, claim.id)
			};
			let Some(route) = route else { continue };
			out.push(Candidate {
				id: claim.id,
				next: route.next,
				seed: claim.seed,
				total: route.metric.seeded(claim.seed),
				advertised: route.advertised,
				priority: route.next.map_or(0, |next| self.links[&(node, next)].priority),
				prefix: claim.prefix.len(),
				epoch: claim.epoch,
			});
		}
		out
	}

	#[allow(clippy::type_complexity)]
	fn rank(
		&self,
		node: Node,
		path: &str,
		c: &Candidate,
	) -> (Reverse<usize>, Reverse<Option<u32>>, Reverse<i8>, (u64, u32), u64) {
		let tie = match self.cfg.tie {
			TieBreak::PathIdentity => fnv(path, &[c.id]),
			TieBreak::PathHere => fnv(path, &[c.id, c.next.unwrap_or(node) as u64, node as u64]),
		};
		(
			Reverse(c.prefix),
			Reverse(c.epoch),
			Reverse(c.priority),
			self.cfg.rule.key(c.total),
			tie,
		)
	}

	/// The candidate `node` uses for a request arriving from `from` carrying `carried`.
	fn choose(&self, node: Node, path: &str, from: Option<Node>, mode: Forward, carried: Carried) -> Option<Candidate> {
		let rule = self.cfg.rule;
		let mut candidates = self.candidates(node, path, from);
		if let Some(pinned) = carried.pinned {
			// A pinned request needs a route to the identity, not knowledge of its claim.
			let from_upstream = from.is_some_and(|from| self.links[&(node, from)].upstream);
			let route = if from_upstream {
				self.export_route(node, pinned)
			} else {
				self.route(node, pinned)
			}?;
			return Some(Candidate {
				id: pinned,
				next: route.next,
				seed: 0,
				total: route.metric,
				advertised: route.advertised,
				priority: 0,
				prefix: 0,
				epoch: carried.epoch,
			});
		}
		match mode {
			Forward::PathOnly {
				exclude_requester: true,
			} => candidates.retain(|c| from.is_none() || c.next != from),
			Forward::Downhill if from.is_some() => {
				let best = candidates.iter().map(|c| rule.key(c.total)).min();
				candidates.retain(|c| {
					c.next.is_none() || best.is_some_and(|best| rule.key(c.advertised.seeded(c.seed)) < best)
				});
			}
			_ => {}
		}
		if let Some(bound) = carried.bound {
			candidates.retain(|c| rule.key(c.total) <= bound);
		}
		candidates.into_iter().min_by_key(|c| self.rank(node, path, c))
	}

	fn carry(&self, mode: Forward, chosen: &Candidate) -> Carried {
		Carried {
			pinned: (mode == Forward::Pinned).then_some(chosen.id),
			bound: (mode == Forward::Bounded).then(|| self.cfg.rule.key(chosen.advertised.seeded(chosen.seed))),
			epoch: chosen.epoch,
		}
	}

	/// The identity a reader at `at` would pick for `path`.
	pub fn resolve(&self, at: Node, path: &str) -> Option<Id> {
		self.choose(at, path, None, Forward::Pinned, Carried::default())
			.map(|chosen| chosen.id)
	}

	/// Follow a request for `path` that a reader at `at` makes.
	pub fn request(&self, at: Node, path: &str, mode: Forward) -> Outcome {
		let mut path_nodes = vec![at];
		let mut node = at;
		let mut from = None;
		let mut carried = Carried::default();
		let mut seen = BTreeSet::new();
		loop {
			if !seen.insert((node, from, carried)) {
				return Outcome::Loop { path: path_nodes };
			}
			let Some(chosen) = self.choose(node, path, from, mode, carried) else {
				return Outcome::Stalled { path: path_nodes };
			};
			let Some(next) = chosen.next else {
				return Outcome::Served {
					by: chosen.id,
					path: path_nodes,
				};
			};
			// A pin or bound is set by the first hop and kept or tightened after.
			if from.is_none() || mode == Forward::Bounded {
				carried = self.carry(mode, &chosen);
			}
			from = Some(node);
			node = next;
			path_nodes.push(node);
		}
	}

	/// Readers at every node in `openers` subscribe to `path` at once; each
	/// relay's forwarded request may join a subscription the relay already has.
	/// Returns the nodes of a subscription cycle that no source feeds, if any.
	pub fn subscribe_all(
		&self,
		path: &str,
		openers: &[Node],
		mode: Forward,
		aggregate: Aggregate,
	) -> Option<Vec<Node>> {
		let mut fronts: Vec<Front> = Vec::new();
		let open = |net: &Self, node: Node, from: Option<Node>, carried: Carried| -> Front {
			match net.choose(node, path, from, mode, carried) {
				None => Front {
					node,
					id: 0,
					epoch: None,
					total: Metric::ZERO,
					up: Up::Stalled,
				},
				Some(chosen) => {
					let carried = if from.is_none() || mode == Forward::Bounded {
						net.carry(mode, &chosen)
					} else {
						carried
					};
					Front {
						node,
						id: chosen.id,
						epoch: chosen.epoch,
						total: chosen.total,
						up: match chosen.next {
							None => Up::Serve,
							Some(next) => Up::Pending(node, next, carried),
						},
					}
				}
			}
		};
		for &node in openers {
			fronts.push(open(self, node, None, Carried::default()));
		}

		let rule = self.cfg.rule;
		let mut index = 0;
		while index < fronts.len() {
			if let Up::Pending(from, to, carried) = fronts[index].up {
				let joins = fronts.iter().position(|front| {
					front.node == to
						&& !matches!(front.up, Up::Stalled)
						&& match aggregate {
							Aggregate::Path => true,
							Aggregate::Epoch => carried.epoch.is_some() && front.epoch == carried.epoch,
							Aggregate::Identity => carried.pinned == Some(front.id),
							Aggregate::Bound => carried.bound.is_some_and(|bound| rule.key(front.total) <= bound),
						}
				});
				let target = joins.unwrap_or_else(|| {
					fronts.push(open(self, to, Some(from), carried));
					fronts.len() - 1
				});
				fronts[index].up = Up::Joined(target);
			}
			index += 1;
		}

		for start in 0..fronts.len() {
			let mut seen = Vec::new();
			let mut at = start;
			loop {
				if seen.contains(&at) {
					return Some(seen.iter().map(|&i| fronts[i].node).collect());
				}
				seen.push(at);
				match fronts[at].up {
					Up::Joined(next) => at = next,
					_ => break,
				}
			}
		}
		None
	}
}

/// The key under which a relay files `id` learned through a scoped admission of `via`.
pub fn scoped_id(id: Id, via: Node) -> Id {
	id | ((via as u64 + 1) << 48)
}

/// FNV-1a, as `origin::fnv_key` uses, over a path and some ids.
pub fn fnv(path: &str, ids: &[u64]) -> u64 {
	const PRIME: u64 = 0x0000_0100_0000_01b3;
	let mut hash: u64 = 0x420C0DECB00B;
	for byte in path.bytes().chain(ids.iter().flat_map(|id| id.to_le_bytes())) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(PRIME);
	}
	hash
}

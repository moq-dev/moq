//! A reference model of the proposed per-origin route layer, small enough to read.
//!
//! It is not the implementation. It exists so the routing contract in
//! `quest/m1/cluster-routing/routes.md` has deterministic counterexamples before
//! the lite-07 wire is fixed: every rule the contract depends on is a switch
//! here, and each fixture runs a scenario under the rule that fails and the rule
//! that holds.
//!
//! Routing is Babel (RFC 8966) keyed by routing identity: each node selects one
//! feasible route per identity, advertises it to every neighbor whose session is
//! up, and only the identity's own node advances its seqno. Messages travel in
//! order on each directed session and can be held to replay them late. Claims
//! (an identity serving a prefix, at a seed cost and optional epoch) are known
//! everywhere unless a fixture hides one from a node, which stands in for
//! scoping, interest, or a claim that has not arrived yet.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
	iter,
};

pub type Node = usize;

/// An opaque routing identity: the hash of a per-process key, or [`scoped`].
pub type Id = u64;

/// Set on every [`scoped`] identity and never on a key-derived one, so any
/// relay recognizes a scoped key.
const SCOPED: Id = 1 << 63;

/// A route metric: the sum of the configured link costs.
pub type Metric = u64;

/// How link costs accumulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
	/// Today's single cost: any link cost, saturating at the ceiling, so a
	/// zero-cost link or a saturated total does not increase the metric.
	Saturating { ceiling: Metric },
	/// Strictly positive link costs with checked addition: a total beyond the
	/// ceiling is unreachable rather than capped.
	Checked { ceiling: Metric },
}

impl Rule {
	/// The checked rule up to the largest QUIC varint.
	pub const CHECKED: Self = Self::Checked { ceiling: (1 << 62) - 1 };

	fn admits(self, cost: Metric) -> bool {
		match self {
			Self::Saturating { .. } => true,
			Self::Checked { .. } => cost > 0,
		}
	}

	/// The metric after adding `cost`, or `None` when unreachable.
	fn add(self, metric: Metric, cost: Metric) -> Option<Metric> {
		match self {
			Self::Saturating { ceiling } => Some(metric.saturating_add(cost).min(ceiling)),
			Self::Checked { ceiling } => metric.checked_add(cost).filter(|metric| *metric <= ceiling),
		}
	}
}

/// What a node may advertise to an upstream link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
	/// No restriction: every link transits.
	Anything,
	/// One selected route per identity, carrying a down-only bit set when it was
	/// learned on an upstream link or from a down-only advertisement, and never
	/// sent to an upstream link.
	DownOnly,
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
	/// A relay that accepted a request from an upstream link ends it once its
	/// route turns down-only. Without it the relay keeps following that route.
	pub recheck: bool,
	/// A scoped connection may forward an already scoped identity, which the
	/// receiver files again under its own scope. Otherwise that route is refused.
	pub rescope: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			rule: Rule::CHECKED,
			export: Export::DownOnly,
			tie: TieBreak::PathIdentity,
			recheck: true,
			rescope: false,
		}
	}
}

/// One side's configuration of a link to a neighbor.
#[derive(Clone, Copy, Debug)]
pub struct Link {
	pub cost: Metric,
	/// The application's local priority for this connection; higher wins.
	pub priority: i8,
	/// This side marks the neighbor upstream.
	pub upstream: bool,
	/// This side admitted the neighbor's publish grant under this scope: an
	/// identity the neighbor forwards, other than its own, is filed under
	/// [`scoped`], and claims count under that key only inside the scope.
	pub scope: Option<&'static str>,
}

impl Default for Link {
	fn default() -> Self {
		Self {
			cost: 1,
			priority: 0,
			upstream: false,
			scope: None,
		}
	}
}

impl Link {
	pub fn cost(cost: Metric) -> Self {
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

	pub fn scope(mut self, scope: &'static str) -> Self {
		self.scope = Some(scope);
		self
	}
}

/// What a node advertises for one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Advert {
	seqno: u32,
	metric: Metric,
	down_only: bool,
}

/// One message: an advertisement, or `None` to retract it.
#[derive(Clone, Copy, Debug)]
struct Update {
	id: Id,
	advert: Option<Advert>,
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
	/// The identity this node's sessions prove, if it originates one.
	proven: Option<Id>,
	/// Identities this node serves itself, with their seqnos: its proven one,
	/// plus any it forges.
	origins: BTreeMap<Id, u32>,
	/// Whether this node advertises identities it does not serve.
	transit: bool,
	rx: BTreeMap<Id, BTreeMap<Node, Advert>>,
	/// For a key this node filed under a scope, the identity the neighbor named.
	filed: BTreeMap<(Node, Id), Id>,
	fd: BTreeMap<Id, (u32, Metric)>,
	sent: BTreeMap<(Node, Id), Advert>,
}

/// An identity serving everything under a prefix.
#[derive(Clone, Copy, Debug)]
pub struct Claim {
	pub id: Id,
	/// A trailing `/` claims the subtree; otherwise only the exact path.
	pub prefix: &'static str,
	pub seed: Metric,
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
	bound: Option<Metric>,
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
	seed: Metric,
	total: Metric,
	advertised: Metric,
	draining: bool,
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
	/// `(at, peer)`: `peer` sent `at` a GOAWAY on their current session.
	draining: BTreeSet<(Node, Node)>,
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
			draining: BTreeSet::new(),
			queues: BTreeMap::new(),
			held: BTreeSet::new(),
			claims: Vec::new(),
			hidden: BTreeSet::new(),
		}
	}

	/// A node that advertises only identities it serves, like a browser.
	pub fn leaf(&mut self, node: Node) {
		self.nodes[node].transit = false;
	}

	/// Configure both sides of a link and open its session.
	pub fn link(&mut self, a: Node, b: Node, ab: Link, ba: Link) {
		let rule = self.cfg.rule;
		assert!(
			rule.admits(ab.cost) && rule.admits(ba.cost),
			"zero-cost link {a}-{b} refused"
		);
		self.links.insert((a, b), ab);
		self.links.insert((b, a), ba);
		self.connect(a, b);
	}

	/// A symmetric link of `cost`.
	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
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
			self.draining.remove(&(y, x));
			let state = &mut self.nodes[y];
			for routes in state.rx.values_mut() {
				routes.remove(&x);
			}
			state.rx.retain(|_, routes| !routes.is_empty());
			state.filed.retain(|(from, _), _| *from != x);
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

	/// `node` starts originating `id` at seqno `seqno`, proving it on every session.
	pub fn originate(&mut self, node: Node, id: Id, seqno: u32) {
		assert_eq!(id & SCOPED, 0, "a key-derived identity is never scoped");
		self.nodes[node].proven = Some(id);
		self.nodes[node].origins.insert(id, seqno);
		self.advertise(node);
	}

	/// `node` advertises `id` as if forwarding it, without proving it.
	pub fn forge(&mut self, node: Node, id: Id, seqno: u32) {
		self.nodes[node].origins.insert(id, seqno);
		self.advertise(node);
	}

	/// The origin advances its seqno.
	pub fn bump(&mut self, node: Node) {
		let state = &mut self.nodes[node];
		let id = state.proven.expect("not an origin");
		*state.origins.get_mut(&id).unwrap() += 1;
		self.advertise(node);
	}

	/// `node` sends GOAWAY on every session: each peer ranks it below every
	/// other eligible route while the session stays up.
	pub fn goaway(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.draining.insert((peer, node));
			self.advertise(peer);
		}
	}

	/// Forget everything about `id` at `node`, as source garbage collection does.
	pub fn collect(&mut self, node: Node, id: Id) {
		let state = &mut self.nodes[node];
		state.rx.remove(&id);
		state.fd.remove(&id);
		self.advertise(node);
	}

	pub fn claim(&mut self, id: Id, prefix: &'static str, seed: Metric, epoch: Option<u32>) -> usize {
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
		self.settle(10_000)
	}

	/// [`Net::run`] for at most `rounds` rounds.
	pub fn settle(&mut self, rounds: usize) -> bool {
		for _ in 0..rounds {
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

	fn receive(&mut self, at: Node, from: Node, update: Update) {
		let mut id = update.id;
		if let Some(scope) = self.links[&(at, from)].scope
			&& self.nodes[from].proven != Some(id)
		{
			if id & SCOPED != 0 && !self.cfg.rescope {
				return;
			}
			let key = scoped(scope, id);
			self.nodes[at].filed.insert((from, key), id);
			id = key;
		}
		let state = &mut self.nodes[at];
		let routes = state.rx.entry(id).or_default();
		match update.advert {
			Some(advert) => routes.insert(from, advert),
			None => routes.remove(&from),
		};
		if routes.is_empty() {
			state.rx.remove(&id);
		}
		self.advertise(at);
	}

	/// The identity `node` names for `id` in a request it sends `next`: the one
	/// `next` advertised, if `node` filed it under `next`'s scope.
	fn name(&self, node: Node, next: Node, id: Id) -> Id {
		self.nodes[node].filed.get(&(next, id)).copied().unwrap_or(id)
	}

	/// How many identities `node` holds routes to.
	pub fn identities(&self, node: Node) -> usize {
		self.nodes[node].rx.len()
	}

	/// The route `node` selected for `id`.
	pub fn route(&self, node: Node, id: Id) -> Option<Selected> {
		let state = &self.nodes[node];
		if let Some(&seqno) = state.origins.get(&id) {
			return Some(Selected {
				next: None,
				seqno,
				metric: 0,
				advertised: 0,
				down_only: false,
			});
		}
		let fd = state.fd.get(&id).copied();
		let feasible = |advert: &Advert| match fd {
			None => true,
			Some((seqno, metric)) => advert.seqno > seqno || (advert.seqno == seqno && advert.metric < metric),
		};
		state
			.rx
			.get(&id)?
			.iter()
			.filter(|(_, advert)| feasible(advert))
			.filter_map(|(&next, advert)| {
				let link = self.links[&(node, next)];
				let metric = self.cfg.rule.add(advert.metric, link.cost)?;
				let selected = Selected {
					next: Some(next),
					seqno: advert.seqno,
					metric,
					advertised: advert.metric,
					down_only: self.cfg.export == Export::DownOnly && (link.upstream || advert.down_only),
				};
				let draining = self.draining.contains(&(node, next));
				Some(((draining, Reverse(link.priority), metric, next), selected))
			})
			.min_by_key(|(rank, _)| *rank)
			.map(|(_, selected)| selected)
	}

	/// The route `node` would offer an upstream link for `id`.
	pub fn export_route(&self, node: Node, id: Id) -> Option<Selected> {
		self.route(node, id).filter(|route| !route.down_only)
	}

	/// The route `node` follows for `id` on behalf of a request from `from`.
	fn route_from(&self, node: Node, id: Id, from: Option<Node>) -> Option<Selected> {
		let upstream = from.is_some_and(|from| self.links[&(node, from)].upstream);
		if upstream && self.cfg.recheck {
			self.export_route(node, id)
		} else {
			self.route(node, id)
		}
	}

	/// Whether `node` holds a reachable route to `id` but none is feasible: it
	/// needs the origin to advance its seqno.
	pub fn starved(&self, node: Node, id: Id) -> bool {
		self.route(node, id).is_none()
			&& self.nodes[node].rx.get(&id).is_some_and(|routes| {
				routes.iter().any(|(&next, advert)| {
					self.cfg
						.rule
						.add(advert.metric, self.links[&(node, next)].cost)
						.is_some()
				})
			})
	}

	/// Recompute every selection at `node`, record feasibility distances for what
	/// it advertises, and queue any change for each neighbor.
	fn advertise(&mut self, node: Node) {
		let state = &self.nodes[node];
		let mut ids: BTreeSet<Id> = state.rx.keys().copied().collect();
		ids.extend(state.origins.keys());
		ids.extend(state.sent.keys().map(|(_, id)| *id));
		let peers: Vec<Node> = self.peers(node).collect();

		for id in ids {
			if !self.nodes[node].transit && !self.nodes[node].origins.contains_key(&id) {
				continue;
			}
			let selected = self.route(node, id);
			if let Some(selected) = selected {
				self.record(node, id, selected);
			}
			for &peer in &peers {
				let upstream = self.links[&(node, peer)].upstream;
				let advert = selected
					.filter(|route| !(upstream && route.down_only))
					.map(|route| Advert {
						seqno: route.seqno,
						metric: route.metric,
						down_only: route.down_only,
					});
				let sent = &mut self.nodes[node].sent;
				if sent.get(&(peer, id)).copied() == advert {
					continue;
				}
				match advert {
					Some(advert) => sent.insert((peer, id), advert),
					None => sent.remove(&(peer, id)),
				};
				self.queues
					.entry((node, peer))
					.or_default()
					.push_back(Update { id, advert });
			}
		}
	}

	/// Babel's feasibility distance: reset by a newer seqno, lowered by a better metric.
	fn record(&mut self, node: Node, id: Id, selected: Selected) {
		let fd = self.nodes[node]
			.fd
			.entry(id)
			.or_insert((selected.seqno, selected.metric));
		if selected.seqno > fd.0 || (selected.seqno == fd.0 && selected.metric < fd.1) {
			*fd = (selected.seqno, selected.metric);
		}
	}

	fn candidates(&self, node: Node, path: &str, from: Option<Node>) -> Vec<Candidate> {
		let scopes: BTreeSet<&str> = self.links.values().filter_map(|link| link.scope).collect();
		let mut out = Vec::new();
		for (index, claim) in self.claims.iter().enumerate() {
			if !claim.covers(path) || self.hidden.contains(&(node, index)) {
				continue;
			}
			// A claim counts under its bare identity, and under a scoped key only inside that scope.
			let inside = scopes.iter().filter(|scope| claim.prefix.starts_with(**scope));
			for id in iter::once(claim.id).chain(inside.map(|scope| scoped(scope, claim.id))) {
				let Some(route) = self.route_from(node, id, from) else {
					continue;
				};
				let Some(total) = self.cfg.rule.add(route.metric, claim.seed) else {
					continue;
				};
				out.push(Candidate {
					id,
					next: route.next,
					seed: claim.seed,
					total,
					advertised: route.advertised,
					draining: route.next.is_some_and(|next| self.draining.contains(&(node, next))),
					priority: route.next.map_or(0, |next| self.links[&(node, next)].priority),
					prefix: claim.prefix.len(),
					epoch: claim.epoch,
				});
			}
		}
		out
	}

	#[allow(clippy::type_complexity)]
	fn rank(
		&self,
		node: Node,
		path: &str,
		c: &Candidate,
	) -> (Reverse<usize>, Reverse<Option<u32>>, bool, Reverse<i8>, Metric, u64) {
		let tie = match self.cfg.tie {
			TieBreak::PathIdentity => fnv(path, &[c.id]),
			TieBreak::PathHere => fnv(path, &[c.id, c.next.unwrap_or(node) as u64, node as u64]),
		};
		(
			Reverse(c.prefix),
			Reverse(c.epoch),
			c.draining,
			Reverse(c.priority),
			c.total,
			tie,
		)
	}

	/// The candidate `node` uses for a request arriving from `from` carrying `carried`.
	fn choose(&self, node: Node, path: &str, from: Option<Node>, mode: Forward, carried: Carried) -> Option<Candidate> {
		if let Some(pinned) = carried.pinned {
			// A pinned request needs a route to the identity, not knowledge of its claim.
			let route = self.route_from(node, pinned, from)?;
			return Some(Candidate {
				id: pinned,
				next: route.next,
				seed: 0,
				total: route.metric,
				advertised: route.advertised,
				draining: false,
				priority: 0,
				prefix: 0,
				epoch: carried.epoch,
			});
		}
		let mut candidates = self.candidates(node, path, from);
		match mode {
			Forward::PathOnly {
				exclude_requester: true,
			} => candidates.retain(|c| from.is_none() || c.next != from),
			Forward::Downhill if from.is_some() => {
				let best = candidates.iter().map(|c| c.total).min();
				candidates.retain(|c| c.next.is_none() || best.is_some_and(|best| c.advertised + c.seed < best));
			}
			_ => {}
		}
		if let Some(bound) = carried.bound {
			candidates.retain(|c| c.total <= bound);
		}
		candidates.into_iter().min_by_key(|c| self.rank(node, path, c))
	}

	/// What `node` sends `next` for `chosen`: a pin or bound is set by the first
	/// hop and kept or tightened after, and a pin is named as `next` knows it.
	fn carry(
		&self,
		node: Node,
		next: Node,
		mode: Forward,
		first: bool,
		chosen: &Candidate,
		carried: Carried,
	) -> Carried {
		let carried = if first || mode == Forward::Bounded {
			Carried {
				pinned: (mode == Forward::Pinned).then_some(chosen.id),
				bound: (mode == Forward::Bounded).then_some(chosen.advertised + chosen.seed),
				epoch: chosen.epoch,
			}
		} else {
			carried
		};
		Carried {
			pinned: carried.pinned.map(|id| self.name(node, next, id)),
			..carried
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
			carried = self.carry(node, next, mode, from.is_none(), &chosen, carried);
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
					total: 0,
					up: Up::Stalled,
				},
				Some(chosen) => Front {
					node,
					id: chosen.id,
					epoch: chosen.epoch,
					total: chosen.total,
					up: match chosen.next {
						None => Up::Serve,
						Some(next) => Up::Pending(
							node,
							next,
							net.carry(node, next, mode, from.is_none(), &chosen, carried),
						),
					},
				},
			}
		};
		for &node in openers {
			fronts.push(open(self, node, None, Carried::default()));
		}

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
							Aggregate::Bound => carried.bound.is_some_and(|bound| front.total <= bound),
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

/// The key a relay files `id` under when a connection admitted under `scope`
/// forwards it. It depends on the scope, not the connection, so every uplink
/// under one grant reaches the same key.
pub fn scoped(scope: &str, id: Id) -> Id {
	fnv(scope, &[id]) | SCOPED
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

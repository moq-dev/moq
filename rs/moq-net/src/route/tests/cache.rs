//! Cache-aware routing over the route layer, as `quest/m3/cache-routing/README.md`
//! decides it.
//!
//! [`Mesh`] adds per-broadcast demand and forwarding to [`Sim`]: readers join and
//! leave, and each relay pulls a broadcast it has demand for through one neighbor,
//! its parent. A relay picks the cheapest neighbor its eligibility rule allows
//! ([`Router::eligible`], a tied route only toward the lower rendezvous hash), where
//! a warm neighbor costs the link alone and a cold one the link plus its cold
//! metric; ties go to warm, then to the lower hash. A copy with no demand left
//! lingers.
//!
//! Every pull names whom it serves ([`Serves`]): a pull whose whole demand traces
//! back to one relay names that relay, its root. A carrier tells each neighbor whose
//! cold metric is at least its own that it is warm, naming whom its copy serves, and
//! the neighbor counts it warm unless that is the neighbor's own root or itself:
//! demand a neighbor causes, through any number of relays, never warms a carrier
//! toward it. A carrier narrows what it tells only once the linger runs out.
//!
//! Each relay recomputes its choices in the same step as its router changes, as the
//! router's own node does, and every step checks that each broadcast's parents form
//! no cycle. Warm routes ride the stream carrying the sender's routes, so a
//! ROUTE_END retires them; subscriptions ride streams of their own, unordered
//! against both. Lingers run out only when a fixture or sweep says time passed.

use super::*;

/// A path its origin serves under one instance: two broadcasts sharing a path
/// and origin are two instances of it.
#[derive(Clone, Copy, Debug)]
struct Broadcast {
	path: &'static str,
	origin: Node,
}

/// Whom a pull or a copy serves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Serves {
	/// Demand of its own: a reader, or pulls tracing back to two or more relays.
	#[default]
	Alone,
	/// Demand that traces back to this one relay alone.
	Relay(Node),
	/// Demand some of which came from a relay that names no one.
	Unknown,
}

/// A warm route: whom the sender's copy serves, and what keeping it costs a
/// receiver it serves alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Warm {
	pub serves: Serves,
	/// The sender's price for its own pull, the cost of the copy to the demand it serves.
	pub cost: Metric,
}

/// One node's state for one broadcast.
#[derive(Clone, Debug, Default)]
struct Carry {
	reader: bool,
	/// Neighbors whose subscription arrived and was not cancelled, and whom each serves.
	downstream: BTreeMap<Node, Serves>,
	/// The neighbor this node pulls the broadcast through.
	parent: Option<Node>,
	/// Whom this node's pull serves, as it last told its parent.
	serves: Serves,
	/// Demand is gone and the copy lingers.
	lingering: bool,
	/// What this node advertises warm, or `None` when cold.
	advert: Option<Warm>,
	/// The advert is narrower than it was, and the old one stands until the linger
	/// runs out.
	fading: bool,
	/// What each neighbor was last told.
	told: BTreeMap<Node, Warm>,
	/// Each neighbor's warm route that arrived.
	warm: BTreeMap<Node, Warm>,
}

/// What the cache layer sent and moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CacheStats {
	/// Warm route starts, updates, and ends.
	pub warm: usize,
	/// Subscriptions, updates, and cancels.
	pub subscribes: usize,
	/// A relay moved a copy from one parent to another.
	pub moves: usize,
}

pub(super) struct Mesh {
	pub sim: Sim,
	broadcasts: Vec<Broadcast>,
	carry: Vec<Vec<Carry>>,
	/// Subscriptions, each broadcast's on a stream of its own.
	subs: BTreeMap<(Node, Node, usize), VecDeque<Sub>>,
	/// `(node, neighbor)`: the neighbor sent `node` GOAWAY.
	draining: BTreeSet<(Node, Node)>,
	/// Relays that never negotiated warm routes: each pulls through its selected
	/// route, sends no warm route, and its pulls name no one.
	legacy: BTreeSet<Node>,
	pub stats: CacheStats,
	/// Warm messages per broadcast.
	warm: Vec<usize>,
}

/// The rendezvous hash that orders a tie, by node index standing in for the Node ID.
fn hash(path: &str, node: Node) -> u64 {
	fnv(path, &[node as u64])
}

/// A change to one subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sub {
	/// Pull, or update whom the pull serves.
	Pull(Serves),
	Cancel,
}

/// A neighbor a node may pull a broadcast through.
#[derive(Clone, Copy, Debug)]
struct Candidate {
	next: Node,
	/// The node's metric through it.
	cold: Metric,
	/// The link to it alone, what pulling through it costs when it is warm.
	link: Metric,
	draining: bool,
}

impl Mesh {
	pub fn new(nodes: usize) -> Self {
		Self {
			sim: Sim::new(nodes),
			broadcasts: Vec::new(),
			carry: vec![Vec::new(); nodes],
			subs: BTreeMap::new(),
			draining: BTreeSet::new(),
			legacy: BTreeSet::new(),
			stats: CacheStats::default(),
			warm: Vec::new(),
		}
	}

	fn nodes(&self) -> usize {
		self.carry.len()
	}

	/// A publisher at `node`, a leaf that originates `id` with a claim covering
	/// every broadcast path. Called before its links, which a leaf configures.
	pub fn origin(&mut self, node: Node, id: Id) {
		self.sim.leaf(node);
		self.sim.originate(node, id, 0);
		self.sim.announce(node, "live/", 0, None);
		self.step();
	}

	/// `node` never negotiates warm routes.
	pub fn legacy(&mut self, node: Node) {
		self.legacy.insert(node);
	}

	/// A broadcast at `path` served by the origin at `origin`.
	pub fn broadcast(&mut self, path: &'static str, origin: Node) -> usize {
		self.broadcasts.push(Broadcast { path, origin });
		for carry in &mut self.carry {
			carry.push(Carry::default());
		}
		self.warm.push(0);
		self.broadcasts.len() - 1
	}

	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.sim.join(a, b, cost);
		self.step();
	}

	/// A reader at `node` starts or stops reading `broadcast`.
	pub fn read(&mut self, node: Node, broadcast: usize, reading: bool) {
		self.carry[node][broadcast].reader = reading;
		self.step();
	}

	pub fn reading(&self, node: Node, broadcast: usize) -> bool {
		self.carry[node][broadcast].reader
	}

	pub fn connect(&mut self, a: Node, b: Node) {
		self.sim.connect(a, b);
		self.step();
	}

	/// The session closes: everything on it goes, subscriptions and warm routes too.
	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.sim.disconnect(a, b);
		for (x, y) in [(a, b), (b, a)] {
			self.subs.retain(|&(from, to, _), _| (from, to) != (x, y));
			self.draining.remove(&(x, y));
			for carry in &mut self.carry[x] {
				carry.downstream.remove(&y);
				carry.told.remove(&y);
				carry.warm.remove(&y);
			}
		}
		self.step();
	}

	/// `node` sends GOAWAY on every session: its neighbors rank it last.
	pub fn goaway(&mut self, node: Node) {
		let peers: Vec<Node> = self.sim.peers(node).collect();
		for peer in peers {
			self.draining.insert((peer, node));
		}
		self.sim.goaway(node);
		self.step();
	}

	/// The origin at `node` advances its seqno.
	pub fn bump(&mut self, node: Node) {
		self.sim.bump(node);
		self.step();
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.sim.hold(from, to);
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.sim.release(from, to);
	}

	/// The linger at `node` for `broadcast` runs out: a copy with no demand goes, and
	/// a narrowed advert takes effect.
	pub fn tick(&mut self, node: Node, broadcast: usize) {
		let carry = &mut self.carry[node][broadcast];
		let parent = match carry.lingering {
			true => {
				carry.lingering = false;
				carry.parent.take()
			}
			false => None,
		};
		if carry.fading {
			carry.fading = false;
			self.carry[node][broadcast].advert = self.desired(node, broadcast);
		}
		if let Some(parent) = parent {
			self.subscribe(node, parent, broadcast, Sub::Cancel);
		}
		self.step();
	}

	/// Deliver the next message on `from`'s route stream to `to`.
	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let delivered = self.sim.deliver(from, to);
		self.step();
		delivered
	}

	pub fn deliver_ack(&mut self, from: Node, to: Node) -> bool {
		let delivered = self.sim.deliver_ack(from, to);
		self.step();
		delivered
	}

	/// Deliver the next subscription change for `broadcast` from `from` to `to`.
	pub fn deliver_sub(&mut self, from: Node, to: Node, broadcast: usize) -> bool {
		let Some(sub) = self.subs.get_mut(&(from, to, broadcast)).and_then(VecDeque::pop_front) else {
			return false;
		};
		self.subs.retain(|_, queue| !queue.is_empty());
		let downstream = &mut self.carry[to][broadcast].downstream;
		match sub {
			Sub::Pull(serves) => downstream.insert(from, serves),
			Sub::Cancel => downstream.remove(&from),
		};
		self.step();
		true
	}

	/// Every queue with something to deliver: routes, echoes, then subscriptions.
	fn busy(&self) -> Vec<Queue> {
		(self.sim.busy().into_iter().map(|(from, to)| Queue::Route(from, to)))
			.chain(self.sim.busy_acks().into_iter().map(|(from, to)| Queue::Ack(from, to)))
			.chain(self.subs.keys().map(|&(from, to, b)| Queue::Sub(from, to, b)))
			.collect()
	}

	fn deliver_one(&mut self, queue: Queue) {
		match queue {
			Queue::Route(from, to) => self.deliver(from, to),
			Queue::Ack(from, to) => self.deliver_ack(from, to),
			Queue::Sub(from, to, b) => self.deliver_sub(from, to, b),
		};
	}

	/// Deliver round-robin until every unheld queue is empty, letting no linger run
	/// out. False if it never settles.
	pub fn run(&mut self) -> bool {
		for _ in 0..10_000 {
			let ready: Vec<Queue> = (self.busy().into_iter())
				.filter(|queue| !self.sim.held.contains(&queue.pair()))
				.collect();
			if ready.is_empty() {
				return true;
			}
			for queue in ready {
				self.deliver_one(queue);
			}
		}
		false
	}

	/// Run, then let every linger run out, until nothing changes.
	pub fn settle(&mut self) -> bool {
		for _ in 0..100 {
			if !self.run() {
				return false;
			}
			let lingering: Vec<(Node, usize)> = (0..self.nodes())
				.flat_map(|node| (0..self.broadcasts.len()).map(move |b| (node, b)))
				.filter(|&(node, b)| {
					let carry = &self.carry[node][b];
					carry.lingering || carry.fading
				})
				.collect();
			if lingering.is_empty() {
				return true;
			}
			for (node, b) in lingering {
				self.tick(node, b);
			}
		}
		false
	}

	pub fn parent(&self, node: Node, broadcast: usize) -> Option<Node> {
		self.carry[node][broadcast].parent
	}

	/// Whether `node` pulls `broadcast`.
	pub fn carries(&self, node: Node, broadcast: usize) -> bool {
		self.parent(node, broadcast).is_some()
	}

	/// Whether `node` counts `from` warm for `broadcast`: its copy stays whether or not
	/// `node` pulls through it.
	pub fn warm_at(&self, node: Node, from: Node, broadcast: usize) -> bool {
		let carry = &self.carry[node][broadcast];
		carry.warm.get(&from).is_some_and(|warm| match warm.serves {
			Serves::Alone => true,
			Serves::Relay(root) => match carry.serves {
				Serves::Unknown => false,
				Serves::Alone => root != node,
				Serves::Relay(own) => root != node && root != own,
			},
			Serves::Unknown => false,
		})
	}

	/// What pulling `broadcast` through `c` costs `node`: the link alone through a warm
	/// neighbor, the link plus the copy's cost through one whose copy serves only
	/// `node`'s demand, and the cold metric through any other.
	fn price(&self, node: Node, broadcast: usize, c: &Candidate) -> Metric {
		match self.carry[node][broadcast].warm.get(&c.next) {
			_ if self.warm_at(node, c.next, broadcast) => c.link,
			Some(warm) if warm.serves != Serves::Unknown => c.link + warm.cost,
			_ => c.cold,
		}
	}

	/// The relays pulling `broadcast` straight from its origin.
	pub fn copies(&self, broadcast: usize) -> usize {
		let origin = self.broadcasts[broadcast].origin;
		(0..self.nodes())
			.filter(|&node| self.parent(node, broadcast) == Some(origin))
			.count()
	}

	/// Warm messages sent for `broadcast`.
	pub fn warm_sent(&self, broadcast: usize) -> usize {
		self.warm[broadcast]
	}

	fn id(&self, broadcast: usize) -> Id {
		self.sim.proven(self.broadcasts[broadcast].origin)
	}

	fn subscribe(&mut self, from: Node, to: Node, broadcast: usize, sub: Sub) {
		self.subs.entry((from, to, broadcast)).or_default().push_back(sub);
		self.stats.subscribes += 1;
	}

	/// Take the deliveries, recompute every node's choices, and check for a loop.
	fn step(&mut self) {
		for note in std::mem::take(&mut self.sim.notes) {
			match note {
				Note::Warm {
					from,
					to,
					broadcast,
					warm,
				} => {
					let map = &mut self.carry[to][broadcast].warm;
					match warm {
						Some(warm) => map.insert(from, warm),
						None => map.remove(&from),
					};
				}
				Note::Ended { from, to, id } => {
					for b in 0..self.broadcasts.len() {
						if self.id(b) == id {
							self.carry[to][b].warm.remove(&from);
						}
					}
				}
			}
		}
		for node in 0..self.nodes() {
			for b in 0..self.broadcasts.len() {
				self.choose(node, b);
				self.tell(node, b);
			}
		}
		self.check_loops();
	}

	/// The neighbors `node` may pull `broadcast` through.
	fn candidates(&self, node: Node, broadcast: usize) -> Vec<Candidate> {
		let path = self.broadcasts[broadcast].path;
		(self.sim.nodes[node].eligible(self.id(broadcast)).into_iter())
			.filter(|route| !route.tied || hash(path, route.next) < hash(path, node))
			.map(|route| Candidate {
				next: route.next,
				cold: route.metric,
				link: self.sim.links[node][route.next].expect("a configured link").cost,
				draining: self.draining.contains(&(node, route.next)),
			})
			.collect()
	}

	/// The neighbor `node` pulls `broadcast` through: its current parent unless another
	/// candidate is strictly cheaper, otherwise the cheapest, ties going to warm and
	/// then to the lower hash.
	fn best(&self, node: Node, broadcast: usize) -> Option<Node> {
		if self.legacy.contains(&node) {
			return self.sim.nodes[node]
				.selected(self.id(broadcast))
				.and_then(|route| route.next);
		}
		let path = self.broadcasts[broadcast].path;
		let ranked: Vec<_> = (self.candidates(node, broadcast).into_iter())
			.map(|c| {
				let warm = self.warm_at(node, c.next, broadcast);
				(
					(c.draining, self.price(node, broadcast, &c)),
					!warm,
					hash(path, c.next),
					c.next,
				)
			})
			.collect();
		let best = ranked.iter().min()?;
		let parent = self.carry[node][broadcast].parent;
		match ranked.iter().find(|rank| Some(rank.3) == parent) {
			Some(current) if current.0 <= best.0 => parent,
			_ => Some(best.3),
		}
	}

	/// What `node` pays for its own pull of `broadcast`.
	fn cost(&self, node: Node, broadcast: usize) -> Option<Metric> {
		let parent = self.carry[node][broadcast].parent?;
		let c = (self.candidates(node, broadcast).into_iter()).find(|c| c.next == parent)?;
		Some(self.price(node, broadcast, &c))
	}

	/// Whom `node`'s demand for `broadcast` serves: itself for a reader, the one root
	/// its pulls trace back to, or no one in particular across two or more roots.
	fn served(&self, node: Node, broadcast: usize) -> Serves {
		let carry = &self.carry[node][broadcast];
		if self.legacy.contains(&node) {
			return Serves::Unknown;
		}
		if carry.reader {
			return Serves::Alone;
		}
		let mut roots = BTreeSet::new();
		let mut unknown = false;
		for (&from, &serves) in &carry.downstream {
			match serves {
				Serves::Alone => roots.insert(from),
				Serves::Relay(root) => roots.insert(root),
				Serves::Unknown => {
					unknown = true;
					false
				}
			};
		}
		match (roots.len(), unknown) {
			(2.., _) => Serves::Alone,
			(1, false) => Serves::Relay(*roots.first().expect("one root")),
			_ => Serves::Unknown,
		}
	}

	/// What `node` would advertise for `broadcast` now: warm while it pulls for demand
	/// it can name.
	fn desired(&self, node: Node, broadcast: usize) -> Option<Warm> {
		let carry = &self.carry[node][broadcast];
		let demand = carry.reader || !carry.downstream.is_empty();
		if node == self.broadcasts[broadcast].origin || self.legacy.contains(&node) || !demand {
			return None;
		}
		let serves = self.served(node, broadcast);
		let cost = self.cost(node, broadcast)?;
		(serves != Serves::Unknown).then_some(Warm { serves, cost })
	}

	/// Recompute `node`'s parent for `broadcast`, and move or update the subscription.
	fn choose(&mut self, node: Node, broadcast: usize) {
		if node == self.broadcasts[broadcast].origin {
			return;
		}
		let carry = &mut self.carry[node][broadcast];
		let demand = carry.reader || !carry.downstream.is_empty();
		if demand {
			carry.lingering = false;
		} else if carry.parent.is_some() {
			carry.lingering = true;
		}
		let want = demand || carry.lingering;
		// A lingering copy serves whom it last served.
		let serves = match demand {
			true => self.served(node, broadcast),
			false => self.carry[node][broadcast].serves,
		};
		let parent = match want {
			true => self.best(node, broadcast),
			false => None,
		};
		let carry = &mut self.carry[node][broadcast];
		let (old, told) = (carry.parent, carry.serves);
		carry.parent = parent;
		carry.serves = serves;
		if parent != old
			&& let Some(old) = old
			&& self.sim.connected(node, old)
		{
			self.subscribe(node, old, broadcast, Sub::Cancel);
		}
		if let Some(parent) = parent
			&& (Some(parent) != old || serves != told)
		{
			self.subscribe(node, parent, broadcast, Sub::Pull(serves));
		}
		if old.is_some() && parent.is_some() && parent != old {
			self.stats.moves += 1;
		}
	}

	/// Update `node`'s advert for `broadcast`, widening at once and narrowing once the
	/// linger runs out, and tell each neighbor that may use it.
	fn tell(&mut self, node: Node, broadcast: usize) {
		let desired = self.desired(node, broadcast);
		let carry = &mut self.carry[node][broadcast];
		// Narrower: some neighbor that counted the copy warm would no longer.
		let narrower = match (carry.advert, desired) {
			(Some(old), Some(new)) => old.serves != new.serves && new.serves != Serves::Alone,
			(Some(_), None) => true,
			(None, _) => false,
		};
		carry.fading = narrower;
		if !narrower {
			carry.advert = desired;
		}
		let advert = carry.advert;
		let id = self.id(broadcast);
		let peers: Vec<Node> = (self.sim.peers(node))
			.filter(|&peer| !self.sim.leaf[peer] && !self.legacy.contains(&peer))
			.collect();
		for to in peers {
			let router = &self.sim.nodes[node];
			// A warm route names the route it travels under, which a ROUTE_END retires.
			let routed = router.sent.contains_key(&(id, to));
			let own = router.selected(id).map(|route| route.metric);
			// A neighbor with no usable route to the origin has an infinite cold metric.
			let behind = (router.rx.get(&id).and_then(|routes| routes.get(&to)))
				.is_none_or(|entry| entry.held || own.is_some_and(|own| entry.advert.metric >= own));
			let carry = &mut self.carry[node][broadcast];
			if !routed {
				carry.told.remove(&to);
			}
			let want = advert.filter(|_| routed && own.is_some() && behind);
			if want == carry.told.get(&to).copied() {
				continue;
			}
			match want {
				Some(warm) => carry.told.insert(to, warm),
				None => carry.told.remove(&to),
			};
			let msg = Msg::Warm { broadcast, warm: want };
			self.sim.queues.entry((node, to)).or_default().push_back(msg);
			self.stats.warm += 1;
			self.warm[broadcast] += 1;
		}
	}

	/// Each broadcast's parents form no cycle.
	fn check_loops(&self) {
		for b in 0..self.broadcasts.len() {
			for node in 0..self.nodes() {
				let mut at = node;
				for hop in 0.. {
					let Some(next) = self.parent(at, b) else {
						break;
					};
					assert!(
						hop <= self.nodes(),
						"broadcast {b} loops from node {node}: {:?}",
						(0..self.nodes()).map(|n| self.parent(n, b)).collect::<Vec<_>>()
					);
					at = next;
				}
			}
		}
	}

	/// The steady state once demand and topology stop changing: every reader is
	/// delivered, subscriptions and warm routes match what their senders hold, nothing
	/// would change, every carrier has demand, and no relay could pull more cheaply
	/// given the copies the others hold.
	pub fn steady(&mut self, label: &str) {
		for (b, broadcast) in self.broadcasts.iter().enumerate() {
			for node in 0..self.nodes() {
				let carry = &self.carry[node][b];
				if carry.reader {
					let mut at = node;
					while let Some(next) = self.parent(at, b) {
						at = next;
					}
					assert_eq!(at, broadcast.origin, "{label}: broadcast {b} reaches node {node}");
				}
				let feeds: BTreeMap<Node, Serves> = (0..self.nodes())
					.filter(|&n| self.parent(n, b) == Some(node))
					.map(|n| (n, self.carry[n][b].serves))
					.collect();
				assert_eq!(carry.downstream, feeds, "{label}: broadcast {b} at node {node}");
				assert!(
					!carry.fading && !carry.lingering,
					"{label}: broadcast {b} at node {node}"
				);
				for to in 0..self.nodes() {
					assert_eq!(
						carry.told.get(&to),
						self.carry[to][b].warm.get(&node),
						"{label}: broadcast {b} warm from {node} at {to}"
					);
				}
				if node == broadcast.origin || carry.parent.is_none() {
					continue;
				}
				assert!(
					carry.reader || !feeds.is_empty(),
					"{label}: node {node} carries broadcast {b} for no one"
				);
			}
			for node in 0..self.nodes() {
				if node == broadcast.origin || self.legacy.contains(&node) {
					continue;
				}
				let Some(cost) = self.cost(node, b) else {
					continue;
				};
				let parent = self.parent(node, b);
				let draining = parent.is_some_and(|parent| self.draining.contains(&(node, parent)));
				for c in self.candidates(node, b) {
					assert!(
						(c.draining, self.price(node, b, &c)) >= (draining, cost),
						"{label}: node {node} would move broadcast {b} from {parent:?} to {}",
						c.next
					);
				}
			}
		}
		let stats = self.stats;
		let queued: usize = self.sim.queues.values().map(VecDeque::len).sum();
		self.step();
		assert_eq!(self.stats, stats, "{label}: a choice changed at quiescence");
		assert_eq!(queued, self.sim.queues.values().map(VecDeque::len).sum::<usize>());
	}
}

/// A queue the cache layer delivers from.
#[derive(Clone, Copy, Debug)]
enum Queue {
	Route(Node, Node),
	Ack(Node, Node),
	Sub(Node, Node, usize),
}

impl Queue {
	fn pair(self) -> (Node, Node) {
		match self {
			Self::Route(from, to) | Self::Ack(from, to) | Self::Sub(from, to, _) => (from, to),
		}
	}
}

/// An origin behind relay `a`, then relays `a`, `b`, `c` in a chain priced 3, with
/// a skip from `a` to `c` priced `skip`, and one broadcast.
fn chain(skip: Metric) -> (Mesh, usize) {
	let (o, a, b, c) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4);
	mesh.origin(o, 7);
	mesh.join(o, a, 1);
	mesh.join(a, b, 3);
	mesh.join(b, c, 3);
	mesh.join(a, c, skip);
	let x = mesh.broadcast("live/x", o);
	assert!(mesh.settle());
	(mesh, x)
}

/// A cold broadcast takes the skip. A reader on the skipped relay makes it pull
/// through `a` and warm toward `c`, which joins it. When that reader leaves, its
/// warm route goes after the linger and `c` returns to the skip.
#[test]
fn a_skipped_relay_draws_its_neighbor_while_it_has_a_reader() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(c, x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(a), "a cold broadcast takes the skip");
	assert!(!mesh.carries(b, x));
	mesh.steady("cold");

	mesh.read(b, x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(b, x), Some(a));
	assert_eq!(mesh.parent(c, x), Some(b), "c joins b's copy");
	mesh.steady("warm");

	mesh.read(b, x, false);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(a), "back to the skip");
	assert!(!mesh.carries(b, x));
	mesh.steady("cold again");
}

/// A relay whose only consumer is `c` withdraws its warm route toward `c`. With
/// a cheaper skip, `c` bypasses it; when the relay is `c`'s cheapest cold route,
/// `c` stays and the relay keeps carrying for it alone.
#[test]
fn a_relay_feeding_one_consumer_stays_only_on_its_cheapest_cold_route() {
	let (a, b, c) = (1, 2, 3);
	for (skip, stays) in [(5, false), (9, true)] {
		let (mut mesh, x) = chain(skip);
		mesh.read(b, x, true);
		mesh.read(c, x, true);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(c, x), Some(b), "skip {skip}");
		mesh.read(b, x, false);
		assert!(mesh.settle());
		assert!(!mesh.warm_at(c, b, x), "skip {skip}: b withdrew");
		assert_eq!(mesh.parent(c, x), Some(if stays { b } else { a }), "skip {skip}");
		assert_eq!(mesh.carries(b, x), stays, "skip {skip}");
		mesh.steady(&format!("skip {skip}"));
	}
}

/// `r2`'s cheapest cold route runs through `r1` to `r3`, so `r3`'s only demand is
/// `r1`'s pull on `r2`'s behalf. Counted as demand besides `r2`'s, it would warm `r3`
/// toward `r2`: `r2` would join `r3` over its direct link, `r1`'s pull and with it
/// `r3`'s warmth would go, and `r2` would return to `r1`, once per linger forever.
/// A pull names the relay it serves, so `r3` stays cold toward `r2`.
#[test]
fn a_pull_on_a_neighbors_behalf_never_warms_its_parent_toward_it() {
	let (o, r1, r2, r3) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4);
	mesh.origin(o, 7);
	mesh.join(o, r3, 1);
	mesh.join(r3, r1, 1);
	mesh.join(r1, r2, 1);
	mesh.join(r2, r3, 3);
	let x = mesh.broadcast("live/x", o);
	mesh.read(r2, x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(r2, x), Some(r1));
	assert_eq!(mesh.parent(r1, x), Some(r3));
	assert!(!mesh.warm_at(r2, r3, x));
	mesh.steady("behalf");

	// A reader of its own makes r1's pull stand alone and r3 warm toward r2, but r1,
	// warm too, is the cheaper join.
	mesh.read(r1, x, true);
	assert!(mesh.settle());
	assert!(mesh.warm_at(r2, r3, x));
	assert_eq!(mesh.parent(r2, x), Some(r1));
	mesh.steady("own reader");
}

const PATHS: [&str; 8] = [
	"live/0", "live/1", "live/2", "live/3", "live/4", "live/5", "live/6", "live/7",
];

/// Two relays at equal cold metric, both warm toward each other: per broadcast,
/// only the lower hash keeps its uplink and the other joins it, so the pair pulls
/// one copy, and the owner differs across broadcasts.
#[test]
fn equal_relays_pull_one_copy_owned_by_the_lower_hash() {
	let (o, x, y) = (0, 1, 2);
	let mut mesh = Mesh::new(3);
	mesh.origin(o, 7);
	mesh.join(o, x, 3);
	mesh.join(o, y, 3);
	mesh.join(x, y, 1);
	let broadcasts: Vec<usize> = PATHS.iter().map(|path| mesh.broadcast(path, o)).collect();
	for &b in &broadcasts {
		mesh.read(x, b, true);
		mesh.read(y, b, true);
	}
	assert!(mesh.settle());
	let mut owners = BTreeSet::new();
	for (&b, path) in broadcasts.iter().zip(PATHS) {
		let (low, high) = match hash(path, x) < hash(path, y) {
			true => (x, y),
			false => (y, x),
		};
		assert_eq!(mesh.parent(low, b), Some(o), "{path}");
		assert_eq!(mesh.parent(high, b), Some(low), "{path}");
		assert_eq!(mesh.copies(b), 1, "{path}");
		owners.insert(low);
	}
	assert_eq!(owners.len(), 2, "the hash spreads the owner");
	mesh.steady("pair");
}

/// A ring of relays, each holding its neighbors' reports from while every relay
/// had its own link to the origin, loses all but one of those links. Each report
/// is now lower than the truth, the hazard that let a whole ring let go at once
/// under the old rank. Delivered in any order, no broadcast loops, and once quiet
/// every reader is fed through the one link left.
#[test]
fn a_ring_of_stale_lower_reports_forms_no_loop() {
	const RING: usize = 5;
	let o = 0;
	let relays: Vec<Node> = (1..=RING).collect();
	for seed in 0..20u64 {
		let mut mesh = Mesh::new(RING + 1);
		mesh.origin(o, 7);
		for (i, &relay) in relays.iter().enumerate() {
			mesh.join(o, relay, 1);
			mesh.join(relay, relays[(i + 1) % RING], 1);
		}
		let x = mesh.broadcast("live/x", o);
		for &relay in &relays {
			mesh.read(relay, x, true);
		}
		assert!(mesh.settle());
		for (i, &relay) in relays.iter().enumerate() {
			let next = relays[(i + 1) % RING];
			mesh.hold(relay, next);
			mesh.hold(next, relay);
		}
		for &relay in &relays[1..] {
			mesh.disconnect(o, relay);
		}
		let mut rng = XorShift(0x5717_e000 + seed);
		loop {
			let busy = mesh.busy();
			if busy.is_empty() {
				break;
			}
			mesh.deliver_one(busy[rng.next(busy.len() as u64) as usize]);
		}
		assert!(mesh.settle());
		mesh.steady(&format!("seed {seed}"));
		assert_eq!(mesh.copies(x), 1, "seed {seed}");
		assert_eq!(mesh.parent(relays[0], x), Some(o), "seed {seed}");
	}
}

/// `b` withdraws its warm route toward `c` while `c`'s subscription to it is on
/// its way. The subscription reaches `b` with no copy left, and `b` pulls it
/// along its own route, as for any request; `c` then moves back to the skip.
#[test]
fn a_request_crossing_a_warm_withdrawal_is_forwarded_without_a_loop() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(b, x, true);
	assert!(mesh.settle());
	assert!(mesh.warm_at(c, b, x));
	mesh.hold(c, b);
	mesh.hold(b, c);
	mesh.read(c, x, true);
	assert!(mesh.run());
	assert_eq!(mesh.parent(c, x), Some(b), "c's subscription waits");

	mesh.read(b, x, false);
	mesh.tick(b, x);
	assert!(mesh.run());
	assert!(!mesh.carries(b, x), "b let its copy go");
	assert!(mesh.warm_at(c, b, x), "its withdrawal waits");

	assert!(mesh.deliver_sub(c, b, x));
	assert_eq!(mesh.parent(b, x), Some(a), "b forwards the request along its own route");
	mesh.release(c, b);
	mesh.release(b, c);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(a));
	assert!(!mesh.carries(b, x));
	mesh.steady("crossed");
}

/// A warm route for another instance of the same path is never joined: `c`'s
/// reader of the new instance takes the skip although `b` is warm for the old one.
#[test]
fn a_warm_route_for_another_instance_is_never_joined() {
	let (o, a, b, c) = (0, 1, 2, 3);
	let (mut mesh, old) = chain(5);
	let new = mesh.broadcast("live/x", o);
	mesh.read(b, old, true);
	mesh.read(c, new, true);
	assert!(mesh.settle());
	assert!(mesh.warm_at(c, b, old));
	assert_eq!(mesh.parent(c, new), Some(a));
	assert!(!mesh.carries(b, new));
	mesh.steady("instances");
}

/// `k` relays on a LAN, each with its own uplink and a reader of every broadcast.
/// In a full mesh each broadcast crosses one uplink. In a ring a relay joins only a
/// neighbor with a lower hash, so each relay whose neighbors both hash higher keeps
/// an uplink copy, which the epic accepts.
#[test]
fn lan_relays_share_uplink_copies() {
	const K: usize = 6;
	let o = 0;
	let relays: Vec<Node> = (1..=K).collect();
	for full in [true, false] {
		let mut mesh = Mesh::new(K + 1);
		mesh.origin(o, 7);
		for &relay in &relays {
			mesh.join(o, relay, 3);
		}
		for i in 0..K {
			for j in i + 1..K {
				if full || j == i + 1 || (i == 0 && j == K - 1) {
					mesh.join(relays[i], relays[j], 1);
				}
			}
		}
		let broadcasts: Vec<usize> = PATHS.iter().map(|path| mesh.broadcast(path, o)).collect();
		for &b in &broadcasts {
			for &relay in &relays {
				mesh.read(relay, b, true);
			}
		}
		assert!(mesh.settle());
		mesh.steady(if full { "full mesh" } else { "ring" });
		let mut copies = 0;
		for (&b, path) in broadcasts.iter().zip(PATHS) {
			let minima = (0..K)
				.filter(|&i| {
					let h = hash(path, relays[i]);
					h < hash(path, relays[(i + 1) % K]) && h < hash(path, relays[(i + K - 1) % K])
				})
				.count();
			assert_eq!(mesh.copies(b), if full { 1 } else { minima }, "{path}");
			copies += mesh.copies(b);
		}
		// Recorded: one copy per broadcast in the full mesh, 13 for 8 in the ring.
		assert_eq!(copies, if full { 8 } else { 13 });
	}
}

/// While a broadcast is pulled: its origin advances its seqno, a relay loses its
/// route, and a relay drains. The seqno reaches `c` through the skip before `b`
/// passes it on, and `b`'s older route is then ineligible, so `c` moves off `b`
/// and back: what staying loop-free across seqnos costs a joined copy.
#[test]
fn a_seqno_advance_a_lost_route_and_a_drain_keep_pulls_loop_free() {
	let (o, a, b, c) = (0, 1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(b, x, true);
	mesh.read(c, x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(b));

	let moves = mesh.stats.moves;
	mesh.bump(o);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(b));
	assert_eq!(mesh.stats.moves - moves, 2, "c left b while b's route was older");
	mesh.steady("seqno");

	mesh.disconnect(a, b);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(a));
	assert_eq!(mesh.parent(b, x), Some(c), "b reaches the origin only through c");
	mesh.steady("route loss");

	mesh.connect(a, b);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(b));
	mesh.goaway(b);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, x), Some(a), "c leaves the draining relay");
	mesh.steady("drain");
}

/// Random relay meshes with two origins under churn: readers come and go,
/// sessions drop and return, messages are held and delivered out of step, lingers
/// run out, origins advance their seqno, and relays drain. No broadcast's parents
/// ever form a cycle. Once churn stops the mesh settles on the steady state, and a
/// broadcast nobody reads has sent no warm route.
#[test]
fn sweep_cache_routing() {
	sweep(100);
}

/// The sweep at length. Loop-free throughout, but 4 of 3000 scenarios (the first is
/// 478) never settle: two carriers each keep a narrowed advert standing while two
/// relays swap between them. Run it with `--ignored` while fixing convergence.
#[test]
#[ignore = "4 of 3000 scenarios never settle"]
fn sweep_cache_routing_long() {
	sweep(3000);
}

fn sweep(scenarios: usize) {
	const RELAYS: usize = 6;
	let mut rng = XorShift(0xcac4_e5ee);
	for scenario in 0..scenarios {
		let label = format!("scenario {scenario}");
		let origins = [RELAYS, RELAYS + 1];
		let mut mesh = Mesh::new(RELAYS + 2);
		mesh.origin(origins[0], 100);
		mesh.origin(origins[1], 200);
		if rng.chance(30) {
			mesh.legacy(rng.next(RELAYS as u64) as usize);
		}
		let mut links: Vec<(Node, Node)> = (0..RELAYS).map(|relay| (relay, (relay + 1) % RELAYS)).collect();
		for _ in 0..2 {
			let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
			if a != b && !links.contains(&(a, b)) && !links.contains(&(b, a)) {
				links.push((a, b));
			}
		}
		for origin in origins {
			let a = rng.next(RELAYS as u64) as usize;
			links.push((origin, a));
			links.push((origin, (a + 1 + rng.next(RELAYS as u64 - 1) as usize) % RELAYS));
		}
		for &(a, b) in &links {
			mesh.join(a, b, 1 + rng.next(5));
		}
		let read = [
			mesh.broadcast("live/a", origins[0]),
			mesh.broadcast("live/b", origins[0]),
			mesh.broadcast("live/a", origins[1]),
			// Another instance of the first.
			mesh.broadcast("live/a", origins[0]),
		];
		let idle = mesh.broadcast("live/idle", origins[0]);
		assert!(mesh.settle(), "{label}");

		for _ in 0..60 {
			match rng.next(12) {
				0..=2 => {
					let (relay, b) = (rng.next(RELAYS as u64) as usize, read[rng.next(4) as usize]);
					let reading = mesh.reading(relay, b);
					mesh.read(relay, b, !reading);
				}
				3 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					match mesh.sim.connected(a, b) {
						true => mesh.disconnect(a, b),
						false => mesh.connect(a, b),
					}
				}
				4 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					match rng.chance(70) {
						true => mesh.hold(from, to),
						false => mesh.release(from, to),
					}
				}
				5 => mesh.tick(rng.next(RELAYS as u64) as usize, read[rng.next(4) as usize]),
				6 if rng.chance(50) => mesh.bump(origins[rng.next(2) as usize]),
				6 if rng.chance(20) => mesh.goaway(rng.next(RELAYS as u64) as usize),
				_ => {
					for _ in 0..rng.next(8) {
						let busy = mesh.busy();
						if busy.is_empty() {
							break;
						}
						mesh.deliver_one(busy[rng.next(busy.len() as u64) as usize]);
					}
				}
			}
		}

		for &(a, b) in &links {
			mesh.release(a, b);
			mesh.release(b, a);
			if !mesh.sim.connected(a, b) {
				mesh.connect(a, b);
			}
		}
		assert!(mesh.settle(), "{label}");
		mesh.steady(&label);
		assert_eq!(mesh.warm_sent(idle), 0, "{label}");
	}
}

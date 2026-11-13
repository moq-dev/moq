//! Cache-aware routing on the path prototype in `paths.rs`, as
//! `quest/m3/cache-routing/README.md` decides it: a warm route is per broadcast,
//! propagates, and is kept free of loops by its path, with what
//! `quest/m2/cache-routing-proof.md` found a path needs besides.
//!
//! A relay with a reader whose pull follows the publisher's route offers its copy
//! at no cost, as its carrier, with the path the copy took. Each relay pulls a
//! broadcast it has demand for (a reader, or a neighbour's subscription) through
//! the cheapest of its next hop toward the publisher and each copy offered, and
//! offers a copy it joined on at what its pull costs, so a copy's cost rises with
//! every link from where it left the publisher's route. Pulling through a
//! neighbour goes wherever that neighbour pulls, so each relay offers every
//! neighbour its copy, even one whose path names that neighbour, which can then
//! use neither. A relay on one copy moves onto another only if that copy's carrier
//! ranks below by the Rendezvous Hash, so copies crossing on their way never trade
//! places; a relay pulling along its route is held to the carriers of the
//! neighbours behind it, which those neighbours' offers back name. A copy with no
//! demand left lingers.
//!
//! [`Mesh`] adds what a sweep checks to [`PathSim`]: after every step, whether a
//! broadcast's pulls form a cycle, which the path allows only while a withdrawal
//! crosses a link; once quiet, every reader is fed and nothing would change.
//! Subscriptions ride streams of their own, unordered against the routes and
//! copies.

use super::paths::{Cast, PathSim, Rules, rendezvous};
use super::*;

/// The rule item 27 of the routes epic picked: holds, causes, and a doubt that lifts
/// after 4 hops of delay.
const PICK: Rules = Rules::waits(4);

/// A queue the mesh delivers from.
#[derive(Clone, Debug)]
enum Queue {
	Route(Node, Node),
	Sub(Node, Node, Cast),
}

pub(super) struct Mesh {
	pub sim: PathSim,
	/// Each publisher's node.
	origins: BTreeMap<Id, Node>,
	casts: Vec<Cast>,
	/// Steps after which some broadcast's pulls formed a cycle.
	pub loops: usize,
	pub steps: usize,
	/// Of those, steps after which a reader's pulls led into the cycle.
	pub starved: usize,
	/// The most deliveries in a row after which a cycle remained, and the current run.
	pub longest: usize,
	run: usize,
	delivered: bool,
}

impl Mesh {
	pub fn new(nodes: usize, rules: Rules) -> Self {
		Self {
			sim: PathSim::new(nodes, rules),
			origins: BTreeMap::new(),
			casts: Vec::new(),
			loops: 0,
			steps: 0,
			starved: 0,
			longest: 0,
			run: 0,
			delivered: false,
		}
	}

	fn nodes(&self) -> usize {
		self.sim.nodes.len()
	}

	/// A publisher at `node`, a leaf that originates `id` and claims `live`. Called
	/// before its links.
	pub fn origin(&mut self, node: Node, id: Id) {
		self.sim.leaf(node);
		self.sim.originate(node, id);
		self.sim.announce(node, "live");
		self.origins.insert(id, node);
	}

	/// `node` never sends or reads a copy.
	pub fn legacy(&mut self, node: Node) {
		self.sim.nodes[node].legacy = true;
	}

	/// A broadcast at `path` of the publisher `id`, under `instance`.
	pub fn broadcast(&mut self, id: Id, path: &str, instance: u64) -> Cast {
		let cast = (id, p(path), instance);
		self.casts.push(cast.clone());
		cast
	}

	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.sim.join(a, b, cost);
		self.step();
	}

	pub fn connect(&mut self, a: Node, b: Node) {
		self.sim.connect(a, b);
		self.step();
	}

	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.sim.disconnect(a, b);
		self.step();
	}

	pub fn goaway(&mut self, node: Node) {
		self.sim.goaway(node);
		self.step();
	}

	/// A reader at `node` starts or stops reading `cast`.
	pub fn read(&mut self, node: Node, cast: &Cast, reading: bool) {
		match reading {
			true => self.sim.carry(node, cast),
			false => self.sim.uncarry(node, cast),
		}
		self.step();
	}

	pub fn reading(&self, node: Node, cast: &Cast) -> bool {
		self.sim.nodes[node].reading(cast)
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.sim.hold(from, to);
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.sim.release(from, to);
	}

	/// The linger for `cast` at `node` runs out.
	pub fn expire(&mut self, node: Node, cast: &Cast) {
		self.sim.expire(node, cast);
		self.step();
	}

	/// One hop of delay passes everywhere. Whether a doubt still waits to lift.
	pub fn tick(&mut self) -> bool {
		let waiting = self.sim.tick();
		self.step();
		waiting
	}

	pub fn deliver_sub(&mut self, from: Node, to: Node, cast: &Cast) -> bool {
		let delivered = self.sim.deliver_sub(from, to, cast);
		self.step();
		delivered
	}

	/// Every unheld queue with something to deliver.
	fn busy(&self) -> Vec<Queue> {
		(self.sim.busy().into_iter().map(|(from, to)| Queue::Route(from, to)))
			.chain((self.sim.busy_subs().into_iter()).map(|(from, to, cast)| Queue::Sub(from, to, cast)))
			.collect()
	}

	fn deliver_one(&mut self, queue: &Queue) {
		self.delivered = true;
		match queue {
			Queue::Route(from, to) => self.sim.deliver(*from, *to),
			Queue::Sub(from, to, cast) => self.sim.deliver_sub(*from, *to, cast),
		};
		self.step();
	}

	/// Deliver round-robin until every unheld queue is empty, letting no linger run
	/// out and no time pass. False if it never empties.
	pub fn run(&mut self) -> bool {
		self.rounds(10_000)
	}

	/// Deliver up to `rounds` rounds, one message from each unheld queue per round.
	/// Whether every unheld queue emptied.
	fn rounds(&mut self, rounds: usize) -> bool {
		for _ in 0..rounds {
			let ready = self.busy();
			if ready.is_empty() {
				return true;
			}
			for queue in &ready {
				self.deliver_one(queue);
			}
		}
		false
	}

	/// Run, let time pass until every doubt lifts, and let every linger run out,
	/// until nothing changes. A round of deliveries takes a hop of delay, so a long
	/// exchange lets time pass too, if more slowly. False if it never settles.
	pub fn settle(&mut self) -> bool {
		for _ in 0..1000 {
			if !self.rounds(64) {
				self.tick();
				continue;
			}
			if self.tick() {
				continue;
			}
			let lingering: Vec<(Node, Cast)> = (0..self.nodes())
				.flat_map(|node| self.sim.nodes[node].lingers().into_iter().map(move |cast| (node, cast)))
				.collect();
			if lingering.is_empty() && self.busy().is_empty() {
				return true;
			}
			for (node, cast) in lingering {
				self.expire(node, &cast);
			}
		}
		false
	}

	pub fn parent(&self, node: Node, cast: &Cast) -> Option<Node> {
		self.sim.nodes[node].parent(cast)
	}

	/// Whether `node` pulls `cast`.
	pub fn carries(&self, node: Node, cast: &Cast) -> bool {
		self.parent(node, cast).is_some()
	}

	/// The relays pulling `cast` straight from its publisher.
	pub fn copies(&self, cast: &Cast) -> usize {
		let origin = self.origins[&cast.0];
		(0..self.nodes())
			.filter(|&node| self.parent(node, cast) == Some(origin))
			.count()
	}

	/// Copy offers and their ends sent for `cast`.
	pub fn warm_sent(&self, cast: &Cast) -> usize {
		self.sim.warm.get(cast).copied().unwrap_or(0)
	}

	/// Whether `node` is offered a copy of `cast` by `from`.
	pub fn offered(&self, node: Node, from: Node, cast: &Cast) -> bool {
		self.sim.nodes[node].offers(from, cast).1.is_some()
	}

	/// Count a step after which some broadcast's pulls form a cycle.
	fn step(&mut self) {
		self.steps += 1;
		let cycle = self.cycle().is_some();
		self.loops += usize::from(cycle);
		self.run = match cycle {
			true => self.run + usize::from(std::mem::take(&mut self.delivered)),
			false => 0,
		};
		self.delivered = false;
		self.longest = self.longest.max(self.run);
		self.starved += usize::from(cycle && self.starving());
	}

	/// Whether some reader's pulls lead into a cycle.
	fn starving(&self) -> bool {
		self.casts.iter().any(|cast| {
			(0..self.nodes()).filter(|&node| self.reading(node, cast)).any(|node| {
				let mut at = node;
				for _ in 0..=self.nodes() {
					match self.parent(at, cast) {
						Some(next) => at = next,
						None => return false,
					}
				}
				true
			})
		})
	}

	/// A broadcast whose pulls form a cycle, and each relay's parent for it.
	fn cycle(&self) -> Option<(Cast, Vec<Option<Node>>)> {
		for cast in &self.casts {
			for node in 0..self.nodes() {
				let mut at = node;
				for hop in 0.. {
					let Some(next) = self.parent(at, cast) else {
						break;
					};
					if hop > self.nodes() {
						let parents = (0..self.nodes()).map(|n| self.parent(n, cast)).collect();
						return Some((cast.clone(), parents));
					}
					at = next;
				}
			}
		}
		None
	}

	/// The steady state once demand and topology stop changing: every reader is fed
	/// from its publisher, subscriptions and copies offered match what their senders
	/// hold, no copy lingers, and each relay pulls exactly what it has demand for,
	/// from its cheapest source. So a relay carries a broadcast only for a reader, or
	/// for neighbours whose cheapest source it is.
	pub fn steady(&mut self, label: &str) {
		assert!(self.busy().is_empty(), "{label}: still busy");
		assert_eq!(self.cycle(), None, "{label}");
		for cast in &self.casts {
			let origin = self.origins[&cast.0];
			for node in 0..self.nodes() {
				let relay = &self.sim.nodes[node];
				if relay.reading(cast) {
					let mut at = node;
					while let Some(next) = self.parent(at, cast) {
						at = next;
					}
					assert_eq!(at, origin, "{label}: {cast:?} reaches node {node}");
				}
				let feeds: BTreeSet<Node> = (0..self.nodes())
					.filter(|&n| self.parent(n, cast) == Some(node))
					.collect();
				assert_eq!(relay.downstream(cast), feeds, "{label}: {cast:?} at node {node}");
				assert!(!relay.lingering(cast), "{label}: {cast:?} at node {node}");
				if node != origin {
					let demand = relay.reading(cast) || !feeds.is_empty();
					let source = relay.source(cast).map(|source| source.next);
					assert_eq!(
						relay.parent(cast),
						source.filter(|_| demand),
						"{label}: {cast:?} at node {node}"
					);
				}
				for peer in 0..self.nodes() {
					if !self.sim.connected(node, peer) || self.sim.nodes[peer].legacy {
						continue;
					}
					let sent = relay.offers(peer, cast).0;
					let heard = self.sim.nodes[peer].offers(node, cast).1;
					assert_eq!(sent, heard, "{label}: {cast:?} offered from {node} to {peer}");
				}
			}
		}
		let stats = self.sim.stats;
		self.tick();
		assert_eq!(self.sim.stats, stats, "{label}: something changed at quiescence");
	}
}

/// An origin behind relay `a`, then relays `a`, `b`, `c` in a chain priced 3, with a
/// skip from `a` to `c` priced `skip`, and one broadcast.
fn chain(skip: Metric) -> (Mesh, Cast) {
	let (o, a, b, c) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4, PICK);
	mesh.origin(o, 7);
	mesh.join(o, a, 1);
	mesh.join(a, b, 3);
	mesh.join(b, c, 3);
	mesh.join(a, c, skip);
	let x = mesh.broadcast(7, "live/x", 0);
	assert!(mesh.settle());
	(mesh, x)
}

/// A cold broadcast takes the skip. A reader on the skipped relay `b` makes `c` join
/// its copy, and when that reader leaves, `c` returns to the skip. Readers arriving
/// the other way round join the other way: `b` joins `c`'s copy, which is farther
/// from the publisher than `b` is.
#[test]
fn a_skipped_relay_draws_its_neighbour_while_it_has_a_reader() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(c, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(a), "a cold broadcast takes the skip");
	assert!(!mesh.carries(b, &x));
	mesh.steady("cold");

	mesh.read(b, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(b, &x), Some(c), "b joins c's copy, though c is farther");
	assert_eq!(mesh.parent(c, &x), Some(a));
	mesh.steady("b joins c");

	let (mut mesh, x) = chain(5);
	mesh.read(b, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(b, &x), Some(a));
	mesh.read(c, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(b), "c joins b's copy");
	mesh.steady("c joins b");

	mesh.read(b, &x, false);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(a), "back to the skip");
	assert!(!mesh.carries(b, &x));
	mesh.steady("cold again");
	assert_eq!(mesh.loops, 0);
}

/// A relay whose only consumer is `c` offers `c` nothing once its own reader goes.
/// With a cheaper skip, `c` bypasses it; when the relay is `c`'s cheapest route to
/// the publisher, `c` stays and the relay keeps pulling for it alone.
#[test]
fn a_relay_feeding_one_consumer_stays_only_on_its_cheapest_route() {
	let (a, b, c) = (1, 2, 3);
	for (skip, stays) in [(5, false), (9, true)] {
		let (mut mesh, x) = chain(skip);
		mesh.read(b, &x, true);
		mesh.read(c, &x, true);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(c, &x), Some(b), "skip {skip}");
		mesh.read(b, &x, false);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(c, &x), Some(if stays { b } else { a }), "skip {skip}");
		assert_eq!(mesh.carries(b, &x), stays, "skip {skip}");
		mesh.steady(&format!("skip {skip}"));
	}
}

/// `r2`'s cheapest route runs through `r1` to `r3`, so `r3` pulls only on `r2`'s
/// behalf, and offers `r2` nothing: were it to, `r2` would join it over the direct
/// link, `r1`'s pull would go, and with it the reason for `r3`'s copy. A reader of
/// `r1`'s own makes it offer a copy, which `r2` already pulls.
#[test]
fn a_pull_on_a_neighbours_behalf_offers_nothing() {
	let (o, r1, r2, r3) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4, PICK);
	mesh.origin(o, 7);
	mesh.join(o, r3, 1);
	mesh.join(r3, r1, 1);
	mesh.join(r1, r2, 1);
	mesh.join(r2, r3, 3);
	let x = mesh.broadcast(7, "live/x", 0);
	assert!(mesh.settle());
	mesh.read(r2, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(r2, &x), Some(r1));
	assert_eq!(mesh.parent(r1, &x), Some(r3));
	assert!(!mesh.offered(r2, r3, &x) && !mesh.offered(r2, r1, &x));
	assert_eq!(mesh.warm_sent(&x), 2, "r2's own copy, offered to r1 and r3");
	mesh.steady("behalf");

	mesh.read(r1, &x, true);
	assert!(mesh.settle());
	assert!(mesh.offered(r2, r1, &x));
	assert_eq!(mesh.parent(r2, &x), Some(r1));
	mesh.steady("own reader");
}

const PATHS: [&str; 8] = [
	"live/0", "live/1", "live/2", "live/3", "live/4", "live/5", "live/6", "live/7",
];

/// Two relays at equal cost, each with a reader of every broadcast, whose readers
/// arrive in the same instant: per broadcast only the lower hash keeps its uplink and
/// the other joins it, so the pair pulls one copy, and the owner differs across
/// broadcasts. Unranked, the two trade copies for good.
#[test]
fn equal_relays_pull_one_copy_owned_by_the_lower_hash() {
	let (o, x, y) = (0, 1, 2);
	let build = |rules: Rules| {
		let mut mesh = Mesh::new(3, rules);
		mesh.origin(o, 7);
		mesh.join(o, x, 3);
		mesh.join(o, y, 3);
		mesh.join(x, y, 1);
		let casts: Vec<Cast> = PATHS.iter().map(|path| mesh.broadcast(7, path, 0)).collect();
		for cast in &casts {
			mesh.sim.carry(x, cast);
			mesh.sim.carry(y, cast);
		}
		(mesh, casts)
	};
	let (mut mesh, casts) = build(PICK);
	assert!(mesh.settle());
	let mut owners = BTreeSet::new();
	for cast in &casts {
		let rank = |node: Node| rendezvous(&cast.1, hop(node, 0));
		let (low, high) = match rank(x) < rank(y) {
			true => (x, y),
			false => (y, x),
		};
		assert_eq!(mesh.parent(low, cast), Some(o), "{cast:?}");
		assert_eq!(mesh.parent(high, cast), Some(low), "{cast:?}");
		assert_eq!(mesh.copies(cast), 1, "{cast:?}");
		owners.insert(low);
	}
	assert_eq!(owners.len(), 2, "the hash spreads the owner");
	mesh.steady("pair");
	assert_eq!(mesh.loops, 0);

	let (mut mesh, _) = build(Rules { ranked: false, ..PICK });
	assert!(!mesh.run(), "unranked, the two trade copies for good");
}

/// A ring of relays, each with its own link to the publisher and a reader, loses all
/// but one of those links while every link of the ring is held, so each relay acts on
/// what it last heard. Delivered in any order, once quiet every reader is fed through
/// the one link left, and the pulls formed a cycle only while a withdrawal was still
/// crossing a link.
#[test]
fn a_ring_of_stale_offers_settles_on_the_link_left() {
	const RING: usize = 5;
	let o = 0;
	let relays: Vec<Node> = (1..=RING).collect();
	let mut loops = 0;
	for seed in 0..20u64 {
		let mut mesh = Mesh::new(RING + 1, PICK);
		mesh.origin(o, 7);
		for (i, &relay) in relays.iter().enumerate() {
			mesh.join(o, relay, 1);
			mesh.join(relay, relays[(i + 1) % RING], 1);
		}
		let x = mesh.broadcast(7, "live/x", 0);
		for &relay in &relays {
			mesh.read(relay, &x, true);
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
		for (i, &relay) in relays.iter().enumerate() {
			let next = relays[(i + 1) % RING];
			mesh.release(relay, next);
			mesh.release(next, relay);
		}
		let mut rng = XorShift(0x5717_e000 + seed);
		loop {
			let busy = mesh.busy();
			if busy.is_empty() {
				break;
			}
			mesh.deliver_one(&busy[rng.next(busy.len() as u64) as usize]);
		}
		assert!(mesh.settle(), "seed {seed}");
		mesh.steady(&format!("seed {seed}"));
		assert_eq!(mesh.copies(&x), 1, "seed {seed}");
		assert_eq!(mesh.parent(relays[0], &x), Some(o), "seed {seed}");
		loops += mesh.loops;
	}
	assert!(loops <= 20, "{loops} steps with a cycle across 20 seeds");
}

/// `b` withdraws its copy while `c`'s subscription to it is on its way. The
/// subscription reaches `b` with no copy left, and `b` pulls along its own route, as
/// for any request; `c` then moves back to the skip.
#[test]
fn a_request_crossing_a_withdrawal_is_forwarded_without_a_loop() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(b, &x, true);
	assert!(mesh.settle());
	assert!(mesh.offered(c, b, &x));
	mesh.hold(c, b);
	mesh.hold(b, c);
	mesh.read(c, &x, true);
	assert!(mesh.run());
	assert_eq!(mesh.parent(c, &x), Some(b), "c's subscription waits");

	mesh.read(b, &x, false);
	mesh.expire(b, &x);
	assert!(mesh.run());
	assert!(!mesh.carries(b, &x), "b let its copy go");
	assert!(mesh.offered(c, b, &x), "its withdrawal waits");

	assert!(mesh.deliver_sub(c, b, &x));
	assert_eq!(
		mesh.parent(b, &x),
		Some(a),
		"b forwards the request along its own route"
	);
	mesh.release(c, b);
	mesh.release(b, c);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(a));
	assert!(!mesh.carries(b, &x));
	mesh.steady("crossed");
	assert_eq!(mesh.loops, 0);
}

/// A copy of another instance of the same path is never joined: `c`'s reader of the
/// new instance takes the skip although `b` offers the old one.
#[test]
fn a_copy_of_another_instance_is_never_joined() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, old) = chain(5);
	let new = mesh.broadcast(7, "live/x", 1);
	mesh.read(b, &old, true);
	mesh.read(c, &new, true);
	assert!(mesh.settle());
	assert!(mesh.offered(c, b, &old));
	assert_eq!(mesh.parent(c, &new), Some(a));
	assert!(!mesh.carries(b, &new));
	mesh.steady("instances");
}

/// `k` relays on a LAN, each with its own uplink and a reader of every broadcast. In
/// a full mesh each broadcast crosses one uplink. In a ring some relays keep an
/// uplink copy of their own, which the epic accepts.
#[test]
fn lan_relays_share_uplink_copies() {
	const K: usize = 6;
	let o = 0;
	let relays: Vec<Node> = (1..=K).collect();
	for full in [true, false] {
		let mut mesh = Mesh::new(K + 1, PICK);
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
		let casts: Vec<Cast> = PATHS.iter().map(|path| mesh.broadcast(7, path, 0)).collect();
		for cast in &casts {
			for &relay in &relays {
				mesh.read(relay, cast, true);
			}
		}
		assert!(mesh.settle());
		let label = if full { "full mesh" } else { "ring" };
		mesh.steady(label);
		let copies: Vec<usize> = casts.iter().map(|cast| mesh.copies(cast)).collect();
		// Recorded: one uplink copy per broadcast in the full mesh, and a few in the
		// ring, where a relay joins only a copy whose carrier ranks below its own.
		// Readers arriving everywhere at once cross their offers on the way, and the
		// pulls form a cycle until those cross back.
		let total: usize = copies.iter().sum();
		assert_eq!(
			(total, mesh.loops),
			if full { (8, 5) } else { (15, 0) },
			"{label}: {copies:?}"
		);
	}
}

/// While a broadcast is pulled, a relay loses its link toward the publisher, and
/// later a relay drains (GOAWAY keeps new pulls off its copy).
#[test]
fn a_lost_route_and_a_drain_move_pulls_without_a_loop() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5);
	mesh.read(b, &x, true);
	mesh.read(c, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(b));

	mesh.disconnect(a, b);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(a));
	assert_eq!(mesh.parent(b, &x), Some(c), "b reaches the publisher only through c");
	mesh.steady("route loss");

	mesh.connect(a, b);
	assert!(mesh.settle());
	mesh.steady("reconnected");
	mesh.goaway(b);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(c, &x), Some(a), "c leaves the draining relay");
	mesh.steady("drain");
	assert_eq!(mesh.loops, 0);
}

const HOME: Node = 0;
const NEAR: Node = 1;
const FAR: Node = 2;
const TAIL: Node = 3;
const ORIGIN: Node = 4;
const SIDE: Node = 5;

/// Two relays with an uplink each to the publisher's relay, `NEAR`'s the cheaper, and
/// a cheap link between them. `TAIL` hangs off `FAR`, and `SIDE` off `NEAR`.
fn uplinks(rules: Rules) -> (Mesh, Cast) {
	let mut mesh = Mesh::new(6, rules);
	mesh.origin(ORIGIN, 7);
	mesh.join(HOME, NEAR, 10);
	mesh.join(HOME, FAR, 12);
	mesh.join(NEAR, FAR, 3);
	mesh.join(FAR, TAIL, 1);
	mesh.join(NEAR, SIDE, 1);
	mesh.join(ORIGIN, HOME, 1);
	let x = mesh.broadcast(7, "live", 0);
	assert!(mesh.settle());
	(mesh, x)
}

/// `FAR` carries the broadcast and is farther from the publisher than `NEAR`. A reader
/// at `SIDE` pulls through `NEAR`, which joins `FAR`'s copy over the cheap link rather
/// than pull its own over its uplink. Babel's distance cannot allow this: `FAR`'s cost
/// to the publisher is not below `NEAR`'s.
#[test]
fn a_relay_joins_a_copy_farther_from_the_publisher() {
	for rules in [PICK, Rules::EPOCHS] {
		let (mut mesh, x) = uplinks(rules);
		mesh.read(FAR, &x, true);
		assert!(mesh.settle());
		let before = mesh.sim.stats;
		mesh.read(SIDE, &x, true);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(SIDE, &x), Some(NEAR), "{rules:?}");
		assert_eq!(
			mesh.parent(NEAR, &x),
			Some(FAR),
			"{rules:?}: NEAR joins the farther copy"
		);
		assert_eq!(mesh.parent(FAR, &x), Some(HOME), "{rules:?}");
		assert_eq!(mesh.sim.nodes[NEAR].cost(7), Some(11));
		assert_eq!(mesh.sim.nodes[FAR].cost(7), Some(13));
		mesh.steady(&format!("{rules:?}"));
		// SIDE's copy came through NEAR, its only neighbour, so it offers it to no one.
		assert_eq!(mesh.sim.stats.warm_msgs - before.warm_msgs, 0, "{rules:?}");

		mesh.read(FAR, &x, false);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(NEAR, &x), Some(HOME), "{rules:?}");
		assert!(!mesh.carries(FAR, &x), "{rules:?}");
		mesh.steady(&format!("{rules:?}: FAR's reader left"));
		assert_eq!(mesh.loops, 0, "{rules:?}");
	}
}

/// Both relays carry a copy, `NEAR`'s pulled from `FAR`'s whenever `FAR` ranks below
/// it. When `FAR`'s uplink closes the two swap if they must, each still fed from the
/// publisher, and the pulls loop only while the news crosses their link.
#[test]
fn two_copies_never_feed_each_other_once_quiet() {
	for rules in [PICK, Rules::EPOCHS] {
		let (mut mesh, x) = uplinks(rules);
		mesh.read(FAR, &x, true);
		assert!(mesh.settle());
		mesh.read(NEAR, &x, true);
		mesh.read(TAIL, &x, true);
		assert!(mesh.settle());
		mesh.steady(&format!("{rules:?}"));
		assert_eq!(mesh.loops, 0, "{rules:?}");

		mesh.disconnect(HOME, FAR);
		assert!(mesh.settle());
		assert_eq!(mesh.parent(NEAR, &x), Some(HOME), "{rules:?}");
		assert_eq!(mesh.parent(FAR, &x), Some(NEAR), "{rules:?}");
		assert_eq!(mesh.parent(TAIL, &x), Some(FAR), "{rules:?}");
		mesh.steady(&format!("{rules:?}: FAR's uplink closed"));
		assert!(mesh.loops <= 3, "{rules:?}: {} steps with a cycle", mesh.loops);
	}
}

/// `NEAR` pulls `FAR`'s copy, whose path crosses `HOME`, and then loses its own
/// session to `HOME`, which casts doubt on `HOME`. Doubt keeps a reader from moving
/// to a copy, not on the one it pulls, so `NEAR` stays on `FAR`'s throughout.
#[test]
fn a_doubt_leaves_the_copy_in_use_alone() {
	let (mut mesh, x) = uplinks(PICK);
	mesh.read(FAR, &x, true);
	mesh.read(SIDE, &x, true);
	assert!(mesh.settle());
	assert_eq!(mesh.parent(NEAR, &x), Some(FAR));

	mesh.disconnect(HOME, NEAR);
	assert_eq!(mesh.parent(NEAR, &x), Some(FAR));
	for _ in 0..64 {
		match mesh.busy().first().cloned() {
			Some(queue) => mesh.deliver_one(&queue),
			None => {
				mesh.tick();
			}
		};
		assert_eq!(mesh.parent(NEAR, &x), Some(FAR));
	}
	assert!(mesh.settle());
	mesh.steady("doubt");
	assert_eq!(mesh.loops, 0);
}

/// The publisher leaves while two relays carry a copy. Every copy goes with its
/// route, nothing is left pulling, and nothing remembers a dead hop.
#[test]
fn a_copy_that_goes_takes_its_offers_along() {
	let (mut mesh, x) = uplinks(PICK);
	mesh.read(FAR, &x, true);
	mesh.read(NEAR, &x, true);
	mesh.read(TAIL, &x, true);
	assert!(mesh.settle());

	mesh.disconnect(ORIGIN, HOME);
	assert!(mesh.settle());
	for relay in [HOME, NEAR, FAR, TAIL, SIDE] {
		assert!(!mesh.carries(relay, &x), "relay {relay}");
		assert_eq!(mesh.sim.nodes[relay].retained(), (0, 0), "relay {relay}");
	}
}

/// Random relay meshes with two publishers under churn: readers come and go,
/// sessions drop and return, messages are held and delivered out of step, lingers
/// run out, time passes, and relays drain, with links held or not. Once churn stops
/// every mesh settles on the steady state, and a broadcast nobody reads has sent no
/// copy offer. The pulls form a cycle only while a withdrawal crosses a link, which
/// the sweep counts.
#[test]
fn sweep_cache_routing() {
	for holds in [true, false] {
		sweep(SEED, 100, holds);
	}
}

/// The gate's seed, and the long sweep's unless `MOQ_ROUTE_SEED` names another.
const SEED: u64 = 0xcac4_e5ee;

/// The sweep at length from `MOQ_ROUTE_SEED` over `MOQ_ROUTE_CASES` scenarios each way,
/// which prints what it measured.
#[test]
#[ignore = "slow: nightly"]
fn sweep_cache_routing_long() {
	let setting = |name: &str, fallback: u64| std::env::var(name).map_or(fallback, |value| value.parse().expect(name));
	let seed = setting("MOQ_ROUTE_SEED", SEED);
	let cases = setting("MOQ_ROUTE_CASES", 3000) as usize;
	println!("MOQ_ROUTE_SEED={seed} MOQ_ROUTE_CASES={cases}");
	for holds in [true, false] {
		let out = sweep(seed, cases, holds);
		println!("holds {holds}: {out:?}");
	}
}

/// What a sweep measured.
#[derive(Debug, Default)]
struct Sweep {
	/// Steps after which some broadcast's pulls formed a cycle, of all steps.
	loops: usize,
	steps: usize,
	/// Scenarios with any such step.
	looped: usize,
	/// Steps after which a reader's pulls led into a cycle.
	starved: usize,
	/// The most deliveries in a row after which a cycle remained.
	longest: usize,
	/// Copy offers and their ends sent, and reader changes, during churn.
	warm: usize,
	reads: usize,
}

fn sweep(seed: u64, scenarios: usize, holds: bool) -> Sweep {
	const RELAYS: usize = 6;
	let mut rng = XorShift(seed);
	let mut out = Sweep::default();
	for scenario in 0..scenarios {
		let label = format!("seed {seed} holds {holds} scenario {scenario}");
		let origins = [RELAYS, RELAYS + 1];
		let mut mesh = Mesh::new(RELAYS + 2, PICK);
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
			mesh.broadcast(100, "live/a", 0),
			mesh.broadcast(100, "live/b", 0),
			mesh.broadcast(200, "live/a", 0),
			// Another instance of the first.
			mesh.broadcast(100, "live/a", 1),
		];
		let idle = mesh.broadcast(100, "live/idle", 0);
		assert!(mesh.settle(), "{label}");

		let warm = mesh.sim.stats.warm_msgs;
		for _ in 0..60 {
			match rng.next(12) {
				0..=2 => {
					let (relay, cast) = (rng.next(RELAYS as u64) as usize, &read[rng.next(4) as usize]);
					let reading = mesh.reading(relay, cast);
					mesh.read(relay, cast, !reading);
					out.reads += 1;
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
						true if holds => mesh.hold(from, to),
						true => {}
						false => mesh.release(from, to),
					}
				}
				5 => mesh.expire(rng.next(RELAYS as u64) as usize, &read[rng.next(4) as usize]),
				6 if rng.chance(50) => {
					mesh.tick();
				}
				6 if rng.chance(20) => mesh.goaway(rng.next(RELAYS as u64) as usize),
				_ => {
					for _ in 0..rng.next(8) {
						let busy = mesh.busy();
						if busy.is_empty() {
							break;
						}
						mesh.deliver_one(&busy[rng.next(busy.len() as u64) as usize]);
					}
				}
			}
		}
		out.warm += mesh.sim.stats.warm_msgs - warm;

		for &(a, b) in &links {
			mesh.release(a, b);
			mesh.release(b, a);
			if !mesh.sim.connected(a, b) {
				mesh.connect(a, b);
			}
		}
		assert!(mesh.settle(), "{label}");
		mesh.steady(&label);
		assert_eq!(mesh.warm_sent(&idle), 0, "{label}");
		out.loops += mesh.loops;
		out.steps += mesh.steps;
		out.looped += usize::from(mesh.loops > 0);
		out.starved += mesh.starved;
		out.longest = out.longest.max(mesh.longest);
	}
	out
}

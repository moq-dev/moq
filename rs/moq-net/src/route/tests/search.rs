//! Every order in which [`Sim`] can deliver, disconnect, connect, collect, fire a
//! timer, and change a claim on a small mesh, searched breadth-first on the real
//! routers, with the route rules checked in every state it reaches.
//!
//! This is `RouteHandoff.tla`'s check run on compiled code: there is no model to
//! drift from the router, and it reaches whatever the model leaves out. A state is
//! the whole [`Sim`], fingerprinted without what only records the past (its event
//! log, stats, and the work counters), with each timer by how far off it is. A
//! hold needs no step of its own: delivering the other queues first is the same
//! order. A broken rule prints the path to it as `Sim` calls.

use std::{
	collections::{BTreeSet, HashMap, hash_map::DefaultHasher},
	fmt,
	hash::Hasher,
	panic::{AssertUnwindSafe, catch_unwind},
};

use super::*;

/// How far the search may go, as `RouteHandoff.cfg` bounds the model.
#[derive(Clone, Debug)]
pub(super) struct Bounds {
	/// The claims the origin may change, and the rules check.
	pub claims: &'static [&'static str],
	/// The origin's claim changes (`MaxRev`).
	pub changes: u32,
	/// Session opens and closes (`MaxFlaps`).
	pub flaps: u32,
	/// A timer fires only once no news about its node is in flight (`Patient`).
	pub patient: bool,
	/// The longest path searched, in steps, if any.
	pub depth: Option<usize>,
}

/// One step of a path, printed as the `Sim` call that replays it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Step {
	Deliver(Node, Node),
	/// An echoed ROUTE_END, under the distance rule.
	Ack(Node, Node),
	Announce {
		origin: Node,
		claim: &'static str,
		seed: Metric,
	},
	Unannounce {
		origin: Node,
		claim: &'static str,
	},
	Connect(Node, Node),
	Disconnect(Node, Node),
	Fire(Node),
	Collect(Node, Id),
}

impl fmt::Display for Step {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Deliver(from, to) => write!(f, "sim.deliver({from}, {to});"),
			Self::Ack(from, to) => write!(f, "sim.deliver_ack({from}, {to});"),
			Self::Announce { origin, claim, seed } => write!(f, "sim.announce({origin}, {claim:?}, {seed}, None);"),
			Self::Unannounce { origin, claim } => write!(f, "sim.unannounce({origin}, {claim:?});"),
			Self::Connect(a, b) => write!(f, "sim.connect({a}, {b});"),
			Self::Disconnect(a, b) => write!(f, "sim.disconnect({a}, {b});"),
			Self::Fire(node) => write!(f, "sim.fire({node});"),
			Self::Collect(node, id) => write!(f, "sim.collect({node}, {id});"),
		}
	}
}

/// What the search covered.
#[derive(Clone, Copy, Debug)]
pub(super) struct Report {
	/// Distinct states.
	pub states: usize,
	/// Steps taken, including those that reached a state already seen.
	pub steps: usize,
	/// The longest path searched.
	pub depth: usize,
	/// Whether some state had a step left past [`Bounds::depth`].
	pub cut: bool,
}

/// A broken rule, and the path the search took to it.
#[derive(Debug)]
pub(super) struct Failure {
	pub rule: String,
	pub path: Vec<Step>,
}

impl fmt::Display for Failure {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "{} after {} steps:", self.rule, self.path.len())?;
		for step in &self.path {
			writeln!(f, "\t{step}")?;
		}
		Ok(())
	}
}

/// What one node's reader shows.
type Showing = BTreeMap<(Id, Path), Shown<Item>>;

/// A claim's life on one node's reader: the node, its restarts and forgets, the
/// identity, the claim, and the revision its life began at.
type Life = (Node, (u32, u32), Id, String, Rev);

/// A searched state: the [`Sim`], and what the rules and bounds remember.
#[derive(Clone)]
struct World {
	sim: Sim,
	changes: u32,
	flaps: u32,
	/// The lives each reader saw end, while it has not forgotten them.
	ended: BTreeSet<Life>,
	/// Messages sent since the last quiet point.
	burst: usize,
	/// External changes since the last quiet point.
	churn: usize,
}

impl World {
	fn fingerprint(&self) -> u64 {
		let mut hasher = DefaultHasher::new();
		let sim = &self.sim;
		for relay in &sim.nodes {
			match relay {
				Relay::Distance(router) => router.fingerprint(&mut hasher),
				Relay::Path(router) => router.fingerprint(sim.now, &mut hasher),
			}
		}
		digest(
			&mut hasher,
			&(&sim.watch, &sim.restarts, &sim.declared, &sim.up, &sim.held),
		);
		digest(&mut hasher, &(&sim.queues, &sim.acks, &sim.history));
		digest(
			&mut hasher,
			&(self.changes, self.flaps, &self.ended, self.burst, self.churn),
		);
		hasher.finish()
	}

	fn quiet(&self) -> bool {
		self.sim.busy().is_empty() && self.sim.busy_acks().is_empty()
	}

	/// Messages and echoes queued, by direction.
	fn queued(&self) -> BTreeMap<(Node, Node), usize> {
		let mut out = BTreeMap::new();
		for (key, queue) in &self.sim.queues {
			*out.entry(*key).or_default() += queue.len();
		}
		for (key, queue) in &self.sim.acks {
			*out.entry(*key).or_default() += queue.len();
		}
		out
	}
}

/// The mesh a search starts from, and what it checks there.
pub(super) struct Mesh {
	/// The state the search starts from: the mesh built, and whatever its fixture
	/// scripted queued but not yet delivered.
	pub sim: Sim,
	/// The node originating `id`.
	pub origin: Node,
	pub id: Id,
}

/// Search every order from `mesh` within `bounds`, stopping at the first broken
/// rule. Depth first, so memory holds one state per step of the current path and a
/// fingerprint per state seen, and a failure's path is the one the search took,
/// not the shortest.
pub(super) fn search(mesh: Mesh, bounds: &Bounds) -> Result<Report, Failure> {
	let Mesh { mut sim, origin, id } = mesh;
	sim.events.clear();
	let search = Search { bounds, origin, id };
	let root = World {
		sim,
		changes: 0,
		flaps: 0,
		ended: BTreeSet::new(),
		burst: 0,
		churn: 1,
	};
	search
		.check(&root, None, &root.sim.shown_all())
		.map_err(|rule| Failure { rule, path: Vec::new() })?;
	// The shallowest depth each state was reached at: under a depth bound, one
	// reached again sooner is searched again, so the search covers every state within it.
	let mut seen = HashMap::from([(root.fingerprint(), 0)]);
	let next = search.steps(&root);
	let mut stack = vec![Frame {
		world: root,
		by: None,
		next,
	}];
	let (mut steps, mut depth, mut cut) = (0, 0, false);
	while let Some(at) = stack.len().checked_sub(1) {
		let frame = &mut stack[at];
		depth = depth.max(at);
		let Some(step) = frame.next.pop() else {
			stack.pop();
			continue;
		};
		if bounds.depth == Some(at) {
			cut = true;
			frame.next.clear();
			continue;
		}
		steps += 1;
		let mut world = frame.world.clone();
		if let Err(rule) = search.apply(&mut world, &step) {
			let mut path: Vec<Step> = stack.iter().filter_map(|frame| frame.by.clone()).collect();
			path.push(step);
			return Err(Failure { rule, path });
		}
		let fp = world.fingerprint();
		if seen
			.get(&fp)
			.is_some_and(|&was| bounds.depth.is_none() || was <= at + 1)
		{
			continue;
		}
		seen.insert(fp, at + 1);
		let next = search.steps(&world);
		stack.push(Frame {
			world,
			by: Some(step),
			next,
		});
	}
	Ok(Report {
		states: seen.len(),
		steps,
		depth,
		cut,
	})
}

/// A state on the current path, the step that reached it, and the steps from it
/// left to take.
struct Frame {
	world: World,
	by: Option<Step>,
	next: Vec<Step>,
}

struct Search<'a> {
	bounds: &'a Bounds,
	origin: Node,
	id: Id,
}

impl Search<'_> {
	/// Every step enabled in `world`.
	fn steps(&self, world: &World) -> Vec<Step> {
		let sim = &world.sim;
		let bounds = self.bounds;
		let mut out: Vec<Step> = sim
			.busy()
			.into_iter()
			.map(|(from, to)| Step::Deliver(from, to))
			.collect();
		out.extend(sim.busy_acks().into_iter().map(|(from, to)| Step::Ack(from, to)));
		for node in 0..sim.nodes.len() {
			if sim.nodes[node].deadline().is_some() && self.patient(sim, node) {
				out.push(Step::Fire(node));
			}
		}
		if world.changes < bounds.changes {
			let seed = Metric::from(world.changes) + 1;
			for &claim in bounds.claims {
				out.push(Step::Announce {
					origin: self.origin,
					claim,
					seed,
				});
				if sim.truth(self.id, claim, sim.revision(self.id)).is_some() {
					out.push(Step::Unannounce {
						origin: self.origin,
						claim,
					});
				}
			}
		}
		if world.flaps < bounds.flaps {
			for a in 0..sim.nodes.len() {
				for b in a + 1..sim.nodes.len() {
					if sim.links[a][b].is_none() {
						continue;
					}
					out.push(match sim.connected(a, b) {
						true => Step::Disconnect(a, b),
						false => Step::Connect(a, b),
					});
				}
			}
		}
		for node in 0..sim.nodes.len() {
			if node != self.origin && !sim.holds(node, self.id) && sim.floors(node, self.id) > 0 {
				out.push(Step::Collect(node, self.id));
			}
		}
		out
	}

	/// Whether `node`'s next timer may fire: with [`Bounds::patient`], only once no
	/// DEAD or DRAIN about a node whose doubt or dead hops it lets go is in flight.
	fn patient(&self, sim: &Sim, node: Node) -> bool {
		if !self.bounds.patient {
			return true;
		}
		let Relay::Path(router) = &sim.nodes[node] else {
			return true;
		};
		let at = sim.now.max(router.deadline().expect("a timer"));
		let due = router.due(at);
		!sim.queues.values().flatten().any(|msg| match msg {
			Msg::Dead(path::Dead::Closed { far, .. }) => due.contains(far),
			Msg::Dead(path::Dead::Draining { node }) => due.contains(node),
			_ => false,
		})
	}

	/// Take `step` and check every rule, catching a router's or `Sim`'s own assertion.
	fn apply(&self, world: &mut World, step: &Step) -> Result<(), String> {
		let before = world.sim.shown_all();
		let quiet = world.quiet();
		let queued = world.queued();
		let stable = self.stable(&world.sim, step);
		let taken = catch_unwind(AssertUnwindSafe(|| self.take(world, step)));
		if let Err(panic) = taken {
			let message = (panic.downcast_ref::<String>().cloned())
				.or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
				.unwrap_or_default();
			return Err(format!("a panic: {message}"));
		}
		// What this step sent: every queue now, less what was there and stayed.
		let mut sent = 0;
		for (key, now) in world.queued() {
			let was = queued.get(&key).copied().unwrap_or(0);
			let consumed = match step {
				Step::Deliver(from, to) | Step::Ack(from, to) if (*from, *to) == key => 1,
				_ => 0,
			};
			sent += now - (was - consumed);
		}
		let external = matches!(
			step,
			Step::Announce { .. } | Step::Unannounce { .. } | Step::Connect(..) | Step::Disconnect(..)
		);
		world.burst = if external && quiet { 0 } else { world.burst } + sent;
		if external {
			world.churn = if quiet { 0 } else { world.churn } + 1;
		}
		let events = std::mem::take(&mut world.sim.events);
		for event in &events {
			let life = (event.node, event.forgot, event.id, event.claim.clone(), event.since);
			match event.kind {
				Kind::End if !event.withdrawn => {
					return Err(format!("NoFalseEnd: node {} saw {} end", event.node, event.claim));
				}
				Kind::End => {
					world.ended.insert(life);
				}
				Kind::Start if world.ended.contains(&life) => {
					return Err(format!(
						"NoRevival: node {} saw {} start again",
						event.node, event.claim
					));
				}
				_ => {}
			}
		}
		self.check(world, stable.then_some(before.as_slice()), &before)
	}

	fn take(&self, world: &mut World, step: &Step) {
		let sim = &mut world.sim;
		match *step {
			Step::Deliver(from, to) => assert!(sim.deliver(from, to)),
			Step::Ack(from, to) => assert!(sim.deliver_ack(from, to)),
			Step::Announce { origin, claim, seed } => {
				sim.announce(origin, claim, seed, None);
				world.changes += 1;
			}
			Step::Unannounce { origin, claim } => {
				sim.unannounce(origin, claim);
				world.changes += 1;
			}
			Step::Connect(a, b) => {
				sim.connect(a, b);
				world.flaps += 1;
			}
			Step::Disconnect(a, b) => {
				sim.disconnect(a, b);
				world.flaps += 1;
			}
			Step::Fire(node) => sim.fire(node),
			Step::Collect(node, id) => sim.collect(node, id),
		}
	}

	/// Whether `step` only changes a route's path or lifts a doubt: a timer, or a
	/// route its receiver already heard from that neighbour.
	fn stable(&self, sim: &Sim, step: &Step) -> bool {
		match *step {
			Step::Fire(_) => true,
			Step::Deliver(from, to) => match (&sim.nodes[to], sim.queues[&(from, to)].front()) {
				(
					Relay::Path(router),
					Some(Msg::Path {
						id,
						prefix,
						reach: Some(path::Reach::Finite(_)),
						..
					}),
				) => router.hears(*id, prefix, from),
				(_, Some(Msg::Dead(path::Dead::Draining { .. }))) => true,
				(
					Relay::Distance(router),
					Some(Msg::Route {
						id,
						reach: Some(Reach::Finite(_)),
					}),
				) => router.rx.get(id).is_some_and(|routes| routes.contains_key(&from)),
				_ => false,
			},
			_ => false,
		}
	}

	/// Check every rule in `world`. `stable` holds what the readers showed before a
	/// step that only changed a path or lifted a doubt; `before` what they showed
	/// before any step.
	fn check(&self, world: &World, stable: Option<&[Showing]>, before: &[Showing]) -> Result<(), String> {
		let sim = &world.sim;
		sim.check().map_err(|err| format!("Truthful: {err}"))?;
		let rev = sim.revision(self.id);
		let now = sim.shown_all();
		for (node, (was, now)) in before.iter().zip(&now).enumerate() {
			for (key, shown) in now {
				let Some(old) = was.get(key) else { continue };
				if shown.item.seed < old.item.seed {
					return Err(format!("NoRegression: node {node} shows {key:?} older"));
				}
			}
			let Some(stable) = stable else { continue };
			for (key, old) in &stable[node] {
				let gone = match now.get(key) {
					Some(shown) => shown.item != old.item && shown.item.seed <= old.item.seed,
					// Letting go of a claim held unavailable ends a hold, not a path,
					// and a newer floor may withdraw one.
					None => old.available && sim.covering(self.id, &key.1, rev) == Some(old.item),
				};
				if gone {
					return Err(format!("Stable: node {node}'s {key:?} changed with only a path"));
				}
			}
		}
		for node in 0..sim.nodes.len() {
			if node == self.origin {
				continue;
			}
			for &claim in self.bounds.claims {
				for key in self.keys(sim) {
					if let Ask::Loop(chain) = self.ask(sim, node, key, &p(claim)) {
						return Err(format!(
							"RequestsLoopFree: {claim} from node {node} loops round {chain:?}"
						));
					}
				}
			}
		}
		let dirs = sim.links.iter().flatten().filter(|side| side.is_some()).count();
		let budget = 4 * dirs * (self.prefixes(sim) + self.bounds.claims.len());
		if world.burst > budget * world.churn {
			return Err(format!(
				"Budget: {} messages for {} changes, over {budget} each",
				world.burst, world.churn
			));
		}
		if world.quiet() && sim.deadline().is_none() {
			self.settled(sim).map_err(|err| format!("QuietIsSettled: {err}"))?;
		}
		Ok(())
	}

	/// The prefixes the nodes ask for, the root included: the routes per publisher.
	fn prefixes(&self, sim: &Sim) -> usize {
		let root = Path::empty();
		sim.watch.iter().chain([&root]).collect::<BTreeSet<_>>().len()
	}

	/// The keys a node may file the origin's claims under: its own, and one per grant.
	fn keys(&self, sim: &Sim) -> Vec<Id> {
		let qualified = sim.grants.iter().map(|subject| qualified(subject, self.id));
		std::iter::once(self.id).chain(qualified).collect()
	}

	/// Where a request for `claim` of `key` from `node`'s reader goes.
	fn ask(&self, sim: &Sim, node: Node, key: Id, claim: &Path) -> Ask {
		let mut chain = vec![node];
		match &sim.nodes[node] {
			Relay::Distance(_) => {
				let mut at = node;
				while let Some(next) = sim.next_hop(at, key) {
					if chain.contains(&next) {
						chain.push(next);
						return Ask::Loop(chain);
					}
					chain.push(next);
					at = next;
				}
				match at == self.origin {
					true => Ask::Origin,
					false => Ask::Stuck,
				}
			}
			Relay::Path(_) => {
				let (mut at, mut key, mut from) = (node, key, None);
				loop {
					match sim.path(at).forward(key, claim, from.take()) {
						None => return Ask::Stuck,
						Some(path::Forward::Here) => return Ask::Origin,
						Some(path::Forward::Next { next, named, prefix }) => {
							let looped = chain.contains(&next);
							chain.push(next);
							if looped {
								return Ask::Loop(chain);
							}
							from = Some((at, prefix));
							(at, key) = (next, named);
						}
					}
				}
			}
		}
	}

	/// Nothing is in flight and no timer is left: each relay shows exactly the claims
	/// a path of open sessions carries to it, available. Under the path rule, a
	/// request for each reaches the origin, and no relay holds a route or remembers
	/// a dead hop.
	fn settled(&self, sim: &Sim) -> Result<(), String> {
		let rev = sim.revision(self.id);
		for &claim in self.bounds.claims {
			let claim = p(claim);
			let truth = sim.truth(self.id, claim.as_str(), rev);
			// The nodes a path of open sessions reaches through nodes asking for the
			// claim, each trusting the one before it with the claim.
			let mut carried = BTreeSet::from([self.origin]);
			let mut grow = vec![self.origin];
			while let Some(at) = grow.pop() {
				for next in sim.peers(at).collect::<Vec<_>>() {
					let granted = (sim.links[next][at].and_then(|side| side.grant))
						.is_none_or(|grant| within(&claim, &p(grant.scope)));
					if granted && within(&claim, &sim.watch[next]) && carried.insert(next) {
						grow.push(next);
					}
				}
			}
			for node in (0..sim.nodes.len()).filter(|&node| node != self.origin) {
				let want = truth.filter(|_| carried.contains(&node));
				let shown: Vec<(Id, &Shown<Item>)> = (self.keys(sim).into_iter())
					.filter_map(|key| Some((key, sim.shown(node).get(&(key, claim.clone()))?)))
					.collect();
				let right = match want {
					Some(item) => {
						!shown.is_empty() && shown.iter().all(|(_, shown)| shown.item == item && shown.available)
					}
					None => shown.is_empty(),
				};
				if !right {
					return Err(format!(
						"node {node} shows {claim} as {shown:?}, where a path carries {want:?}"
					));
				}
				for (key, _) in shown {
					if sim.paths() && self.ask(sim, node, key, &claim) != Ask::Origin {
						return Err(format!("a request for {claim} from node {node} stops short"));
					}
				}
			}
		}
		if !sim.paths() {
			return Ok(());
		}
		for node in (0..sim.nodes.len()).filter(|&node| node != self.origin) {
			let router = sim.path(node);
			for key in self.keys(sim) {
				if let Some((prefix, route)) = router.routes(key).find(|(_, route)| route.offer().is_none()) {
					return Err(format!("node {node} still holds {key}'s {prefix:?}: {route:?}"));
				}
			}
			let kept = router.retained();
			if kept.dead > 0 || kept.doubt > 0 {
				return Err(format!("node {node} still remembers {kept:?}"));
			}
		}
		Ok(())
	}
}

#[derive(Debug, PartialEq, Eq)]
enum Ask {
	Origin,
	Stuck,
	Loop(Vec<Node>),
}

impl Sim {
	/// What every node's reader shows.
	fn shown_all(&self) -> Vec<Showing> {
		(0..self.nodes.len()).map(|node| self.shown(node).clone()).collect()
	}
}

/// The origin and two relays in a triangle, every session open and the origin's
/// route sent, as `RouteHandoff.tla` starts. `dear` is the cost of the origin's
/// link to relay 2; `watch` what relay 2 asks for.
fn triangle(dear: Metric, watch: &str, grant: Option<Grant>) -> Mesh {
	let mut sim = Sim::with(Rule::PATH, 3);
	sim.watch(2, watch);
	sim.join(0, 1, 1);
	sim.join(0, 2, dear);
	let side = SimLink::cost(1);
	sim.link(1, 2, grant.map_or(side, |grant| side.grant(grant)), side);
	sim.originate(0, 7, 0);
	Mesh { sim, origin: 0, id: 7 }
}

/// `RouteHandoff.cfg`'s bounds: one claim, one claim change, one session change.
const ONE: Bounds = Bounds {
	claims: &["x"],
	changes: 1,
	flaps: 1,
	patient: true,
	depth: None,
};

fn run(mesh: Mesh, bounds: &Bounds) -> Report {
	let report = search(mesh, bounds).unwrap_or_else(|failure| panic!("{failure}"));
	eprintln!("{report:?}");
	assert!(!report.cut, "every state within the bounds");
	report
}

/// The model's triangle under `RouteHandoff.cfg`'s bounds, in the merge gate.
#[test]
#[cfg_attr(miri, ignore = "searches thousands of states")]
fn every_order_on_a_triangle_keeps_the_rules() {
	run(triangle(1, "", None), &ONE);
}

/// Two relays asking for `a/x` and `b/x`.
const TWO: Bounds = Bounds {
	claims: &["a/x", "b/x"],
	..ONE
};

// What the gate leaves to Nightly (`test/route-sweep.sh`), each a few seconds to a
// minute in release: `RouteHandoff.cfg`'s own mesh, and on the triangle what the
// model leaves out or bounds tighter.

/// `RouteHandoff.cfg`'s mesh: the origin behind relay 1, in a triangle with relays 2 and 3.
#[test]
#[ignore = "Nightly: about half a minute in release"]
fn every_order_behind_a_relay_keeps_the_rules() {
	let mut sim = Sim::with(Rule::PATH, 4);
	sim.join(0, 1, 1);
	sim.join(1, 2, 1);
	sim.join(1, 3, 1);
	sim.join(2, 3, 1);
	sim.originate(0, 7, 0);
	run(Mesh { sim, origin: 0, id: 7 }, &ONE);
}

/// The origin's link to relay 2 costs 2, so relay 2 prefers relay 1's path.
#[test]
#[ignore = "Nightly"]
fn every_order_past_a_dear_link_keeps_the_rules() {
	run(triangle(2, "", None), &ONE);
}

/// A claim at the root and one inside it, as a wildcard covers its paths.
#[test]
#[ignore = "Nightly"]
fn every_order_of_nested_claims_keeps_the_rules() {
	let nested = Bounds {
		claims: &["", "a"],
		..ONE
	};
	run(triangle(1, "", None), &nested);
}

/// Relay 2 asks for `a` alone.
#[test]
#[ignore = "Nightly"]
fn every_order_with_a_narrow_relay_keeps_the_rules() {
	run(triangle(1, "a", None), &TWO);
}

/// Relay 2 publishes to relay 1 under a grant scoped to `a`, so relay 1 files what
/// relay 2 sends under a qualified key.
#[test]
#[ignore = "Nightly"]
fn every_order_under_a_scoped_grant_keeps_the_rules() {
	let grant = Grant {
		subject: "g",
		scope: "a",
	};
	run(triangle(1, "", Some(grant)), &TWO);
}

/// The self-check's mesh on the path rule, which shows `b/x` in every order.
#[test]
#[ignore = "Nightly"]
fn every_order_past_a_narrowed_relay_shows_what_a_path_carries() {
	run(blind(Rule::PATH), &BLIND);
}

/// `a_relay_behind_a_scoped_neighbour_is_blind`'s mesh settled with every relay
/// asking for everything, and then the narrow relay asking for `a` alone, with
/// nothing delivered since.
fn blind(rule: Rule) -> Mesh {
	let (origin, narrow, wide, far, relay, tail) = (0, 1, 2, 3, 4, 5);
	let mut sim = Sim::with(rule, 6);
	sim.join(origin, narrow, 1);
	sim.join(narrow, relay, 1);
	sim.join(origin, far, 1);
	sim.join(far, wide, 1);
	sim.join(wide, relay, 1);
	sim.join(relay, tail, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	sim.announce(origin, "b/x", 0, None);
	assert!(sim.run());
	sim.watch(narrow, "a");
	Mesh { sim, origin, id: 7 }
}

/// What the blind mesh's fixture scripted: no change past the narrowed watch.
const BLIND: Bounds = Bounds {
	claims: &["a/x", "b/x"],
	changes: 0,
	flaps: 0,
	patient: true,
	depth: None,
};

/// The search's self-check: on the distance rule, which the path rule replaces,
/// it finds the blind relay `residual.rs` pins.
#[test]
#[cfg_attr(miri, ignore = "searches thousands of states")]
fn the_search_finds_the_blind_relay() {
	let failure = search(blind(Rule::Distance), &BLIND).expect_err("the distance rule is blind");
	assert!(failure.rule.contains("node 4 shows b/x as []"), "{failure}");
}

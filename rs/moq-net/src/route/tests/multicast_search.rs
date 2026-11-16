//! Every order in which [`RelaySim`] can deliver, flush a router, let a reader's debounce
//! run out, change a claim, and open or close a link on a small mesh, searched depth-first
//! on [`relay::Router`], with the multicast rule's properties checked in every state it
//! reaches: `search.rs` for the rule the switch puts lite-07 sessions on.
//!
//! A router reads any burst before it answers, so flushing one is a step of its own and
//! every batching is an order. The interest exchange that opens a session crosses as the
//! router flushes, as [`RelaySim`] carries it. A state is the whole [`RelaySim`],
//! fingerprinted without what only records its past, with each reader deadline by how far
//! off it is. A broken property prints the path to it as [`RelaySim`] calls.
//!
//! The properties: no reader sees an end its publisher never made (NoFalseEnd) or a life
//! it saw end start again (NoRevival); every copy of a forwarding node's claims at a floor
//! is that node's claims at that revision (Truthful); messages per change stay within a
//! budget per link and forwarding node (Budget); and once nothing is in flight, nothing
//! waits to flush, and no debounce runs, each relay shows exactly the claims a path of
//! open sessions carries, every request reaches its publisher, every route leads to its
//! forwarding node, and nothing about a closed session is left (QuietIsSettled). Requests
//! may loop while news is in flight, which the bake-off measures, so loops are checked
//! once quiet. Each property has a [`Mutant`] it is shown to catch.

use std::{
	collections::{BTreeSet, HashSet, VecDeque, hash_map::DefaultHasher},
	fmt,
	hash::Hasher,
	panic::{AssertUnwindSafe, catch_unwind},
};

use crate::route::relay::{self, Mutant};

use super::multi::Tie;
use super::multicast::RelaySim;
use super::*;

/// How far the search may go.
#[derive(Clone, Debug)]
struct Bounds {
	/// The paths the publisher may claim at, each life a new Instance.
	claims: &'static [&'static str],
	/// Claims announced or withdrawn.
	changes: u32,
	/// Links opened or closed.
	flaps: u32,
}

/// One step of a path, printed as the [`RelaySim`] call that replays it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
	Deliver(Node, Node),
	Flush(Node),
	Fire(Node),
	Announce(&'static str, Id),
	Withdraw(&'static str),
	Open(Node, Node),
	Close(Node, Node),
}

impl fmt::Display for Step {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Deliver(from, to) => write!(f, "sim.deliver({from}, {to});"),
			Self::Flush(node) => write!(f, "sim.flush_node({node});"),
			Self::Fire(node) => write!(f, "sim.fire({node});"),
			Self::Announce(claim, id) => write!(f, "sim.publish(P, {id}); sim.claim(P, {claim:?}, true);"),
			Self::Withdraw(claim) => write!(f, "sim.claim(P, {claim:?}, false);"),
			Self::Open(a, b) => write!(f, "sim.open({a}, {b});"),
			Self::Close(a, b) => write!(f, "sim.close({a}, {b});"),
		}
	}
}

/// A property the search checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
	NoFalseEnd,
	NoRevival,
	Truthful,
	Budget,
	QuietIsSettled,
}

/// What broke in a step: a property, or an assertion in a router or the sim.
#[derive(Debug)]
enum Broken {
	Rule(Rule, String),
	Panic(String),
}

impl fmt::Display for Broken {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Rule(rule, why) => write!(f, "{rule:?}: {why}"),
			Self::Panic(message) => write!(f, "a panic: {message}"),
		}
	}
}

/// What the search covered.
#[derive(Clone, Copy, Debug)]
struct Report {
	states: usize,
	steps: usize,
	depth: usize,
	/// The most messages one change cost.
	burst: usize,
}

/// A broken property, and the path the search took to it.
#[derive(Debug)]
struct Failure {
	broken: Broken,
	path: Vec<Step>,
}

impl fmt::Display for Failure {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "{} after {} steps:", self.broken, self.path.len())?;
		for step in &self.path {
			writeln!(f, "\t{step}")?;
		}
		Ok(())
	}
}

/// The mesh a search starts from: relays, one publisher, and the links that may flap.
struct Mesh {
	sim: RelaySim,
	relays: usize,
	publisher: Node,
	links: Vec<(Node, Node)>,
	/// The Instance the publisher's next claim serves.
	next: Id,
}

/// A searched state: the [`RelaySim`], and what the properties and bounds remember.
#[derive(Clone)]
struct World {
	sim: RelaySim,
	changes: u32,
	flaps: u32,
	next: Id,
	/// The broadcasts each reader saw end, and how many times it had forgotten each then.
	ended: BTreeSet<(Node, Path, Id, u32)>,
	/// Messages sent since the network was last quiet.
	burst: usize,
	/// External changes since the network was last quiet.
	churn: usize,
}

impl World {
	fn fingerprint(&self) -> u64 {
		let mut hasher = DefaultHasher::new();
		self.sim.fingerprint(&mut hasher);
		digest(
			&mut hasher,
			&(self.changes, self.flaps, self.next, &self.ended, self.burst, self.churn),
		);
		hasher.finish()
	}

	fn queued(&self) -> usize {
		self.sim.ready().iter().map(|(_, count)| count).sum()
	}
}

struct Search<'a> {
	bounds: &'a Bounds,
	relays: usize,
	publisher: Node,
	links: Vec<(Node, Node)>,
	/// Messages one change may cost: four per direction of each link for every
	/// forwarding node and every claim.
	budget: usize,
	/// The one property checked, if not every one: where a planted defect breaks another
	/// first, or an assertion, the search goes on past it, so it shows this one is checked.
	only: Option<Rule>,
}

fn search(mesh: Mesh, bounds: &Bounds) -> Result<Report, Failure> {
	search_for(mesh, bounds, None)
}

fn search_for(mesh: Mesh, bounds: &Bounds, only: Option<Rule>) -> Result<Report, Failure> {
	let Mesh {
		mut sim,
		relays,
		publisher,
		links,
		next,
	} = mesh;
	sim.events.clear();
	sim.auto = false;
	let budget = 4 * (2 * links.len()) * (relays + bounds.claims.len());
	let search = Search {
		bounds,
		relays,
		publisher,
		links,
		budget,
		only,
	};
	let root = World {
		sim,
		changes: 0,
		flaps: 0,
		next,
		ended: BTreeSet::new(),
		burst: 0,
		churn: 1,
	};
	search.check(&root).map_err(|broken| Failure {
		broken,
		path: Vec::new(),
	})?;
	let mut seen = HashSet::from([root.fingerprint()]);
	let next = search.steps(&root);
	let mut stack = vec![(root, None::<Step>, next)];
	let (mut steps, mut depth, mut burst) = (0, 0, 0);
	while let Some(at) = stack.len().checked_sub(1) {
		depth = depth.max(at);
		let Some(step) = stack[at].2.pop() else {
			stack.pop();
			continue;
		};
		steps += 1;
		let mut world = stack[at].0.clone();
		match search.apply(&mut world, &step) {
			Ok(()) => {}
			// What broke it is not what this search looks for, and a state an assertion
			// stopped halfway is no state to go on from.
			Err(Broken::Panic(_)) if only.is_some() => continue,
			Err(broken) => {
				let mut path: Vec<Step> = stack.iter().filter_map(|(_, by, _)| by.clone()).collect();
				path.push(step);
				return Err(Failure { broken, path });
			}
		}
		burst = burst.max(world.burst / world.churn.max(1));
		if !seen.insert(world.fingerprint()) {
			continue;
		}
		let next = search.steps(&world);
		stack.push((world, Some(step), next));
	}
	Ok(Report {
		states: seen.len(),
		steps,
		depth,
		burst,
	})
}

impl Search<'_> {
	fn checks(&self, rule: Rule) -> bool {
		self.only.is_none_or(|only| only == rule)
	}

	/// Nothing in flight, nothing waiting to flush, and no debounce running.
	fn quiet(&self, sim: &RelaySim) -> bool {
		sim.ready().is_empty() && (0..self.relays).all(|node| !sim.pending(node)) && sim.timers().is_empty()
	}

	/// Every step enabled in `world`.
	fn steps(&self, world: &World) -> Vec<Step> {
		let sim = &world.sim;
		let mut out: Vec<Step> = (sim.ready().into_iter())
			.map(|((from, to), _)| Step::Deliver(from, to))
			.collect();
		out.extend((0..self.relays).filter(|&node| sim.pending(node)).map(Step::Flush));
		out.extend(sim.timers().into_iter().map(Step::Fire));
		if world.changes < self.bounds.changes {
			for &claim in self.bounds.claims {
				match sim.claims(self.publisher).iter().any(|(path, _)| *path == p(claim)) {
					true => out.push(Step::Withdraw(claim)),
					false => out.push(Step::Announce(claim, world.next)),
				}
			}
		}
		if world.flaps < self.bounds.flaps {
			for &(a, b) in &self.links {
				match sim.up().contains(&(a.min(b), a.max(b))) {
					true => out.push(Step::Close(a, b)),
					false => out.push(Step::Open(a, b)),
				}
			}
		}
		out
	}

	/// Take `step` and check every property, catching a router's or the sim's assertion.
	fn apply(&self, world: &mut World, step: &Step) -> Result<(), Broken> {
		let quiet = self.quiet(&world.sim);
		let queued = world.queued();
		let taken = catch_unwind(AssertUnwindSafe(|| self.take(world, step)));
		if let Err(panic) = taken {
			let message = (panic.downcast_ref::<String>().cloned())
				.or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
				.unwrap_or_default();
			return Err(Broken::Panic(message));
		}
		// What this step sent: every queue now, less what was there and stayed.
		let consumed = usize::from(matches!(step, Step::Deliver(..)));
		let sent = (world.queued() + consumed).saturating_sub(queued);
		let external = matches!(
			step,
			Step::Announce(..) | Step::Withdraw(_) | Step::Open(..) | Step::Close(..)
		);
		world.burst = if external && quiet { 0 } else { world.burst } + sent;
		if external {
			world.churn = if quiet { 0 } else { world.churn } + 1;
		}
		for event in std::mem::take(&mut world.sim.events) {
			let (id, path) = event.broadcast;
			match event.kind {
				Kind::End if event.live && self.checks(Rule::NoFalseEnd) => {
					let why = format!("node {} saw {path} of {id} end", event.node);
					return Err(Broken::Rule(Rule::NoFalseEnd, why));
				}
				Kind::End => {
					world.ended.insert((event.node, path, id, event.forgot));
				}
				// Excused once the node forgot it, as `search.rs` excuses the path rule.
				Kind::Start
					if self.checks(Rule::NoRevival)
						&& world.ended.contains(&(event.node, path.clone(), id, event.forgot)) =>
				{
					let why = format!("node {} saw {path} of {id} start again", event.node);
					return Err(Broken::Rule(Rule::NoRevival, why));
				}
				_ => {}
			}
		}
		self.check(world)
	}

	fn take(&self, world: &mut World, step: &Step) {
		let sim = &mut world.sim;
		match *step {
			Step::Deliver(from, to) => assert!(sim.deliver(from, to)),
			Step::Flush(node) => sim.flush_node(node),
			Step::Fire(node) => sim.fire(node),
			Step::Announce(claim, id) => {
				sim.publish(self.publisher, id);
				sim.claim(self.publisher, claim, true);
				world.next += 1;
				world.changes += 1;
			}
			Step::Withdraw(claim) => {
				sim.claim(self.publisher, claim, false);
				world.changes += 1;
			}
			Step::Open(a, b) => {
				sim.open(a, b);
				world.flaps += 1;
			}
			Step::Close(a, b) => {
				sim.close(a, b);
				world.flaps += 1;
			}
		}
	}

	fn check(&self, world: &World) -> Result<(), Broken> {
		let sim = &world.sim;
		if self.checks(Rule::Truthful) {
			self.truthful(sim).map_err(|why| Broken::Rule(Rule::Truthful, why))?;
		}
		if self.checks(Rule::Budget) && world.burst > self.budget * world.churn {
			let why = format!(
				"{} messages for {} changes, over {} each",
				world.burst, world.churn, self.budget
			);
			return Err(Broken::Rule(Rule::Budget, why));
		}
		if self.checks(Rule::QuietIsSettled) && self.quiet(sim) {
			self.settled(sim)
				.map_err(|why| Broken::Rule(Rule::QuietIsSettled, why))?;
		}
		Ok(())
	}

	/// Every copy a relay holds of a forwarding node's claims is that node's claims at the
	/// copy's floor.
	fn truthful(&self, sim: &RelaySim) -> Result<(), String> {
		for node in 0..self.relays {
			let Some(router) = sim.router(node) else { continue };
			for (origin, floor, items) in router.copies() {
				let truth = sim.truth(origin, floor);
				if truth != Some(items) {
					return Err(format!(
						"node {node} holds {origin:?}'s claims at {floor} as {items:?}, where it announced {truth:?}"
					));
				}
			}
		}
		Ok(())
	}

	/// The relays a path of open sessions joins to `from`, `from` included.
	fn reach(&self, sim: &RelaySim, from: &[Node]) -> BTreeSet<Node> {
		let mut seen: BTreeSet<Node> = from.iter().copied().collect();
		let mut queue: VecDeque<Node> = from.iter().copied().collect();
		while let Some(at) = queue.pop_front() {
			for &(a, b) in sim.up() {
				let next = match at {
					_ if at == a => b,
					_ if at == b => a,
					_ => continue,
				};
				if next < self.relays && seen.insert(next) {
					queue.push_back(next);
				}
			}
		}
		seen
	}

	/// Each relay shows exactly the claims a path of open sessions carries to it, available,
	/// and a request for each reaches the publisher; every route a relay holds leads over
	/// open sessions to its forwarding node, and it holds one to every forwarding node
	/// such a path joins it to; and no relay remembers a closed session or a drain.
	fn settled(&self, sim: &RelaySim) -> Result<(), String> {
		let homes: Vec<Node> = (sim.up().iter())
			.filter_map(|&(a, b)| match self.publisher {
				_ if a == self.publisher => Some(b),
				_ if b == self.publisher => Some(a),
				_ => None,
			})
			.collect();
		let carried = match homes.is_empty() {
			true => BTreeSet::new(),
			false => self.reach(sim, &homes),
		};
		let claims = sim.claims(self.publisher);
		for node in 0..self.relays {
			let router = sim.router(node).expect("a relay");
			for (claim, id) in claims {
				let shown = router.shown().get(&(*id, claim.clone()));
				match carried.contains(&node) {
					true if shown.is_none_or(|shown| !shown.available) => {
						return Err(format!("node {node} shows {claim} of {id} as {shown:?}"));
					}
					true if sim.chase(node, *id, claim).is_none_or(|(_, reached)| !reached) => {
						return Err(format!("a request for {claim} of {id} from node {node} stops short"));
					}
					_ => {}
				}
			}
			for (id, claim) in router.shown().keys() {
				if !carried.contains(&node) || !claims.contains(&(claim.clone(), *id)) {
					return Err(format!("node {node} still shows {claim} of {id}"));
				}
			}
			// Routes: to every forwarding node an open path joins, and over open sessions.
			let joined = self.reach(sim, &[node]);
			for other in (0..self.relays).filter(|&other| other != node) {
				let origin = sim.router(other).expect("a relay").me();
				let announced = sim.router(other).expect("a relay").own().0 > 0;
				let want = announced && joined.contains(&other);
				if router.best(origin).is_some() != want {
					return Err(format!(
						"node {node} {} a route to {origin:?}",
						if want { "lacks" } else { "holds" }
					));
				}
				if want {
					let (mut at, mut hops) = (node, 0);
					while at != other {
						let Some((via, _)) = sim.router(at).and_then(|router| router.best(origin)) else {
							return Err(format!("node {node}'s route to {origin:?} stops at {at}"));
						};
						if !sim.up().contains(&(at.min(via), at.max(via))) || hops > self.relays {
							return Err(format!("node {node}'s route to {origin:?} breaks at {at}"));
						}
						(at, hops) = (via, hops + 1);
					}
				}
			}
			for origin in router.routes() {
				let current = (0..self.relays).any(|other| sim.router(other).expect("a relay").me() == origin);
				if !current {
					return Err(format!("node {node} holds a route to {origin:?}, which is gone"));
				}
			}
			let kept = router.kept();
			if kept.pending > 0 {
				return Err(format!("node {node} still remembers {kept:?}"));
			}
		}
		Ok(())
	}
}

/// Three relays in a triangle with a publisher on relay 0, every session open and its
/// interest exchanged, the publisher's first claim in flight. Every link may flap.
fn triangle() -> Mesh {
	let mut mesh = unanswered();
	settle(&mut mesh.sim);
	mesh.sim.claim(mesh.publisher, "x", true);
	mesh
}

/// The triangle with every session just opened and nothing flushed.
fn unanswered() -> Mesh {
	let mut sim = RelaySim::new(4, relay::debounce(HOP));
	sim.client(3);
	sim.publish(3, 100);
	sim.auto = false;
	for (a, b) in [(0, 1), (0, 2), (1, 2)] {
		sim.link(a, b, 1, Tie::Peer);
	}
	sim.link(3, 0, 1, Tie::Peer);
	Mesh {
		sim,
		relays: 3,
		publisher: 3,
		links: vec![(0, 1), (0, 2), (1, 2), (3, 0)],
		next: 101,
	}
}

/// Relay 1 behind relay 0 and its publisher, relays 2 and 3 behind relay 1, and relay 4
/// behind both, everything settled: relay 4 hears two routes through the link from 0 to 1,
/// so whichever neighbour names it closed first, the other's route still names it. That
/// link may close.
fn diamond() -> Mesh {
	let mut sim = RelaySim::new(6, relay::debounce(HOP));
	sim.client(5);
	sim.publish(5, 100);
	sim.auto = false;
	for (a, b) in [(0, 1), (1, 2), (1, 3), (2, 4), (3, 4)] {
		sim.link(a, b, 1, Tie::Peer);
	}
	sim.link(5, 0, 1, Tie::Peer);
	sim.claim(5, "x", true);
	settle(&mut sim);
	Mesh {
		sim,
		relays: 5,
		publisher: 5,
		links: vec![(0, 1)],
		next: 101,
	}
}

/// Flush and deliver until nothing is in flight.
fn settle(sim: &mut RelaySim) {
	loop {
		sim.flush();
		let ready = sim.ready();
		if ready.is_empty() {
			return;
		}
		for ((from, to), _) in ready {
			while sim.deliver(from, to) {}
		}
	}
}

/// One claim change and one flap.
const ONE: Bounds = Bounds {
	claims: &["x"],
	changes: 1,
	flaps: 1,
};

fn run(mesh: Mesh, bounds: &Bounds) -> Report {
	let report = search(mesh, bounds).unwrap_or_else(|failure| panic!("{failure}"));
	eprintln!("{report:?}");
	report
}

/// The triangle from its publisher's first claim, in the merge gate.
#[test]
#[cfg_attr(miri, ignore = "searches thousands of states")]
fn every_order_on_a_triangle_keeps_the_multicast_rules() {
	run(triangle(), &ONE);
}

/// The diamond's shared link closing, with one claim change: the dead hop a relay keeps
/// for a route that still names it, until that route goes.
#[test]
#[cfg_attr(miri, ignore = "searches thousands of states")]
fn every_order_past_a_shared_hop_keeps_the_multicast_rules() {
	run(diamond(), &ONE);
}

// What the gate leaves to Nightly (`test/route-sweep.sh`).

/// The triangle from its sessions' first flush: every order of the interest exchange,
/// the first claim, and the route starts it brings.
#[test]
#[ignore = "Nightly"]
fn every_order_from_the_first_flush_keeps_the_multicast_rules() {
	let mut mesh = unanswered();
	mesh.sim.claim(mesh.publisher, "x", true);
	run(mesh, &ONE);
}

/// Each property fails when a defect it guards against is planted, so the search checks
/// what it claims to.
#[test]
#[cfg_attr(miri, ignore = "searches thousands of states")]
fn every_property_catches_its_mutant() {
	for (mutant, rule) in [
		(Mutant::LostEnds, Rule::NoFalseEnd),
		(Mutant::OldFloor, Rule::NoRevival),
		(Mutant::Lagging, Rule::Truthful),
		(Mutant::KeepDead, Rule::QuietIsSettled),
		(Mutant::Chatter, Rule::Budget),
	] {
		// Only the diamond keeps a dead hop.
		let mut mesh = match mutant {
			Mutant::KeepDead => diamond(),
			_ => triangle(),
		};
		mesh.sim.mutate(mutant);
		let failure = search_for(mesh, &ONE, Some(rule)).expect_err(&format!("{mutant:?} went unnoticed"));
		eprintln!("{mutant:?}: {failure}");
		assert!(matches!(failure.broken, Broken::Rule(broke, _) if broke == rule));
	}
}

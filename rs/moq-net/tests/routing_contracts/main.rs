//! Deterministic counterexamples for the per-origin routing contract.
//!
//! Each fixture runs one scenario from `quest/m1/cluster-routing/routes.md`
//! against [`model`], under the rule that fails and the rule that holds. The
//! model is a reference for the contract, not the relay: when lite-07's route
//! layer lands, these topologies become its acceptance tests. Failover fixtures
//! here advance the origin's seqno by hand where the seqno request would;
//! [`handoff`] runs the request exchange and the announcement layer. It sits in
//! `tests/` rather than inline because it tests no moq-net module.

mod handoff;
mod model;

use model::{Aggregate, Config, Export, Forward, Grant, Link, Metric, Net, Outcome, Rule, TieBreak, qualified};

const PATH_ONLY: Forward = Forward::PathOnly {
	exclude_requester: true,
};

/// The CDN's grant to a local relay: its credential subject, publishing under `local/`.
const LOCAL: Grant = Grant {
	subject: "local",
	scope: "local/",
};

fn served_by(outcome: &Outcome) -> Option<(u64, usize)> {
	match outcome {
		Outcome::Served { by, path } => Some((*by, *path.last().unwrap())),
		_ => None,
	}
}

fn loops(outcomes: &[Outcome]) -> bool {
	outcomes.iter().any(|outcome| matches!(outcome, Outcome::Loop { .. }))
}

// Identity: a fresh routing identity per process start, seqno per identity.

/// A restart that reuses its routing identity restarts its seqno below the
/// feasibility distance every other node still holds, so its routes are refused
/// until the new process counts past the old one. This is why a pinned
/// `cluster.id` cannot stay the routing identity.
#[test]
fn restart_reusing_identity_is_refused() {
	let (origin, a, b) = (0, 1, 2);
	let mut net = Net::new(Config::default(), 3);
	net.join(origin, a, 1);
	net.join(a, b, 1);
	net.originate(origin, 7, 0);
	for _ in 0..3 {
		net.bump(origin);
	}
	assert!(net.run());
	assert!(net.route(b, 7).is_some());

	net.kill(origin);
	net.originate(origin, 7, 0);
	net.reconnect(origin);
	assert!(net.run());
	assert!(net.starved(a, 7), "seqno 0 is older than the 3 a remembers");
	assert!(net.route(b, 7).is_none());

	net.kill(origin);
	net.originate(origin, 8, 0);
	net.reconnect(origin);
	assert!(net.run());
	assert!(net.route(b, 8).is_some(), "a fresh identity has no history to lose to");
}

/// The same process moving to a farther attachment keeps its identity and
/// seqno, so the node next to its old attachment holds a feasibility distance
/// shorter than any route it now has and starves. The origin must advance its
/// seqno whenever its own sessions change.
#[test]
fn reconnect_without_restart_needs_a_seqno_advance() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(a, b, 1);
	net.join(b, c, 1);
	net.join(origin, a, 1);
	net.join(origin, c, 1);
	net.disconnect(origin, c);
	net.originate(origin, 7, 0);
	assert!(net.run());

	net.disconnect(origin, a);
	net.connect(origin, c);
	assert!(net.run());
	assert!(net.starved(a, 7), "a advertised one hop; every new route is longer");

	net.bump(origin);
	assert!(net.run());
	assert_eq!(net.route(a, 7).and_then(|route| route.next), Some(b));
}

/// A dead incarnation's route can return after a node collects it, but only
/// from a message already in flight on a live session, and that session then
/// delivers the retraction behind it. Ordered sessions, session-scoped state,
/// and never advertising an unreachable route are what bound the revival, not
/// the random identity.
#[test]
fn collected_incarnation_returns_only_while_its_retraction_is_in_flight() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(origin, a, 1);
	net.join(a, b, 1);
	net.join(a, c, 1);
	net.join(c, b, 1);
	// Everything a sends b waits: first the route, then its retraction.
	net.hold(a, b);
	net.originate(origin, 7, 0);
	assert!(net.run());
	assert_eq!(net.route(b, 7).and_then(|route| route.next), Some(c));

	net.kill(origin);
	assert!(net.run());
	assert!(net.route(b, 7).is_none());

	net.collect(b, 7);
	assert!(net.deliver(a, b), "the held update");
	assert!(net.route(b, 7).is_some(), "the dead incarnation is back at b");
	assert!(net.run());
	// c drained its distance, so b's copy spreads to it, but the path ends at a,
	// which let go: a stale dead end until the retraction lands, never a loop.
	assert_eq!(net.route(c, 7).and_then(|route| route.next), Some(b));
	assert!(net.route(a, 7).is_none());

	net.release(a, b);
	assert!(net.run());
	for node in 0..4 {
		assert!(
			net.route(node, 7).is_none(),
			"node {node} still routes to a dead incarnation"
		);
	}
}

/// No route entry is not proof that no advertisement derived from this node's
/// own is still in flight. b's echo from c waits on c's session while b loses
/// its only entry. b keeps its feasibility distance until every neighbor it
/// advertised to echoes the ROUTE_END back, so the queued echo is refused
/// rather than letting b route via c while c still routes via b. A leaf never
/// advertises the identity back, so only an explicit echo of the ROUTE_END,
/// which it sends too, ends the drain. Once the echoes land nothing is left.
#[test]
fn collection_keeps_the_feasibility_distance_against_a_queued_echo() {
	let (origin, b, c, leaf) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.leaf(leaf);
	net.join(origin, b, 1);
	net.join(b, c, 1);
	net.join(b, leaf, 1);
	net.hold(c, b);
	net.originate(origin, 7, 0);
	assert!(net.run());
	assert_eq!(net.route(c, 7).and_then(|route| route.next), Some(b));
	assert!(net.queued(c, b) > 0, "c's echo waits");

	net.hold(b, c);
	net.hold(leaf, b);
	net.disconnect(origin, b);
	assert!(!net.holds(b, 7));
	net.collect(b, 7);
	assert!(net.remembers(b, 7), "draining: the ROUTE_ENDs are not echoed yet");
	assert!(net.deliver(c, b), "the echo of b's own advertisement");
	assert!(net.route(b, 7).is_none(), "b refuses its own echo");
	assert!(net.starved(b, 7));

	net.release(b, c);
	net.release(c, b);
	assert!(net.run());
	assert!(net.queued_acks(leaf, b) > 0, "the leaf's echo waits");
	assert!(net.remembers(b, 7), "the leaf may still hold b's advertisement");
	net.release(leaf, b);
	assert!(net.run());
	for node in [b, c, leaf] {
		assert!(
			net.route(node, 7).is_none(),
			"node {node} routes to an unreachable origin"
		);
		assert!(!net.holds(node, 7), "node {node}");
		assert!(!net.remembers(node, 7), "node {node} keeps a drained distance");
	}
}

/// Every node's next-hop chain toward `id` ends without revisiting a node.
fn assert_loop_free(net: &Net, nodes: usize, id: u64) {
	for node in 0..nodes {
		let mut at = node;
		for hop in 0.. {
			let Some(next) = net.route(at, id).and_then(|route| route.next) else {
				break;
			};
			assert!(hop < nodes, "{id} loops from node {node}");
			at = next;
		}
	}
}

/// A restarted relay loses every feasibility distance, yet stays loop-free:
/// its sessions close with the process, and each neighbor drops what it
/// learned on them, so no neighbor still holds an advertisement the new
/// process never made. A stale echo still reaches it through a third node's
/// session that outlived it, and is safe, because every node on that echo's
/// path kept its own distance.
#[test]
fn restart_drops_the_feasibility_distance_with_every_session() {
	let (origin, b, c, d) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(origin, b, 1);
	net.join(b, c, 1);
	net.join(c, d, 1);
	net.join(d, b, 10);
	net.originate(origin, 7, 0);
	assert!(net.run());
	assert_eq!(net.route(d, 7).and_then(|route| route.next), Some(c));

	// The update c sends d after losing b waits on a session that outlives b.
	net.hold(c, d);
	net.kill(b);
	assert!(net.run());
	assert_loop_free(&net, 4, 7);
	assert!(net.route(c, 7).is_none());
	assert_eq!(net.route(d, 7).and_then(|route| route.next), Some(c), "stale");

	net.connect(b, d);
	assert!(net.run());
	assert_eq!(
		net.route(b, 7).and_then(|route| route.next),
		Some(d),
		"the echo of the old advertisement reaches the new process"
	);
	assert_loop_free(&net, 4, 7);
	net.connect(b, c);
	assert!(net.run());
	assert!(net.route(c, 7).is_none(), "the distance at c refuses the longer copy");
	assert_loop_free(&net, 4, 7);

	net.release(c, d);
	assert!(net.run());
	assert_loop_free(&net, 4, 7);
	net.connect(origin, b);
	assert!(net.run());
	assert_loop_free(&net, 4, 7);
	for node in [b, c, d] {
		assert!(net.route(node, 7).is_some(), "node {node} recovers");
	}
}

/// Random meshes churn links, hold and deliver messages out of step, and
/// collect route-less identities, then drop the feasibility distance. Dropped
/// once no route entry is left, it loops; dropped once every neighbor has
/// processed the retraction, as its echoed ROUTE_ENDs or a restart
/// prove, it never does. Only the edge to each neighbor matters, since every
/// node farther away keeps its own distance.
#[test]
fn sweep_forgetting_distances() {
	let mut loops = [0; 2];
	let mut forgotten = [0; 2];
	for (acked, (loops, forgotten)) in [false, true].into_iter().zip(loops.iter_mut().zip(&mut forgotten)) {
		let mut rng = XorShift(0x5eed_f0d5);
		for _ in 0..300 {
			let nodes = RELAYS + 2;
			let mut net = Net::new(Config::default(), nodes);
			let mut links = Vec::new();
			for a in 0..RELAYS {
				for b in a + 1..RELAYS {
					if b == a + 1 || rng.chance(40) {
						net.join(a, b, 1 + rng.next(3));
						links.push((a, b));
					}
				}
			}
			let ids = [RELAYS as u64, RELAYS as u64 + 1];
			for origin in RELAYS..nodes {
				let relay = rng.next(RELAYS as u64) as usize;
				net.join(origin, relay, 1);
				links.push((relay, origin));
				net.originate(origin, origin as u64, 0);
			}
			assert!(net.run());

			for _ in 0..60 {
				match rng.next(8) {
					0 => {
						let (a, b) = links[rng.next(links.len() as u64) as usize];
						if net.connected(a, b) {
							net.disconnect(a, b);
						} else {
							net.connect(a, b);
						}
					}
					1 | 2 => {
						let (a, b) = links[rng.next(links.len() as u64) as usize];
						let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
						if rng.chance(60) {
							net.hold(from, to);
						} else {
							net.release(from, to);
						}
					}
					3 => {
						let busy = net.busy();
						if !busy.is_empty() {
							let (from, to) = busy[rng.next(busy.len() as u64) as usize];
							net.deliver(from, to);
						}
					}
					4 => net.bump(RELAYS + rng.next(2) as usize),
					_ => {
						let node = rng.next(RELAYS as u64) as usize;
						let id = ids[rng.next(2) as usize];
						if !net.holds(node, id) {
							net.collect(node, id);
							if !acked || net.retracted(node, id) {
								net.forget(node, id);
								*forgotten += 1;
							}
						}
					}
				}
				net.settle(1);
				for &id in &ids {
					for node in 0..nodes {
						let mut at = node;
						let mut hops = 0;
						while let Some(next) = net.route(at, id).and_then(|route| route.next) {
							hops += 1;
							if hops > nodes {
								*loops += 1;
								break;
							}
							at = next;
						}
					}
				}
			}
		}
	}
	println!("distances dropped {forgotten:?}, loops {loops:?}: at no entry, then once retracted");
	assert!(loops[0] > 0, "the sweep must reach the unsafe drop");
	assert!(forgotten[1] > 0);
	assert_eq!(loops[1], 0);
}

// Metric: strictly positive link costs, checked, with no hop count.

/// An origin two links from node 2, then an unrelated session at node 2 that
/// makes it recompute its selection.
fn chain(rule: Rule, origin_cost: Metric, sibling_cost: Metric) -> Net {
	let (origin, a, b, late) = (0, 1, 2, 3);
	let mut net = Net::new(
		Config {
			rule,
			..Config::default()
		},
		4,
	);
	net.join(origin, a, origin_cost);
	net.join(a, b, sibling_cost);
	net.originate(origin, 7, 0);
	assert!(net.run());
	net.join(b, late, 1);
	assert!(net.run());
	net
}

/// Babel's feasibility needs every link to increase the metric (RFC 8966
/// s3.5.2). A zero-cost link, which `?cost=0` allows today, leaves a route
/// equal to its own feasibility distance, so the next recomputation refuses
/// it. Strictly positive costs increase on every link without a hop count, so
/// a zero-cost link is refused when it is configured.
#[test]
#[should_panic(expected = "zero-cost link 1-2 refused")]
fn zero_cost_links_are_refused() {
	assert!(chain(Rule::Saturating { ceiling: Metric::MAX }, 10, 0).starved(2, 7));
	assert_eq!(
		chain(Rule::CHECKED, 10, 1).route(2, 7).map(|route| route.metric),
		Some(11)
	);
	chain(Rule::CHECKED, 10, 0);
}

/// A total beyond the ceiling is unreachable under checked addition. A
/// saturating cost instead caps it at the ceiling, equal to the next hop's
/// metric, so the route is accepted and then starves on its next recomputation.
#[test]
fn overflow_is_unreachable() {
	let ceiling = 100;
	assert!(chain(Rule::Saturating { ceiling }, 100, 5).starved(2, 7));

	let net = chain(Rule::Checked { ceiling }, 100, 5);
	assert_eq!(net.route(1, 7).map(|route| route.metric), Some(100));
	assert!(net.route(2, 7).is_none());
	assert!(!net.starved(2, 7), "unreachable, not waiting on a seqno");
	assert!(net.route(3, 7).is_none());
}

// Forwarding progress: path-only requests reselect the origin at every hop.

/// Three relays in a ring, each next to its own importer of one epoch, and each
/// giving its clockwise link `priority`.
fn priority_ring(priority: i8) -> Net {
	let mut net = Net::new(Config::default(), 6);
	for relay in 0..3 {
		let next = (relay + 1) % 3;
		net.link(relay, next, Link::cost(1).priority(priority), Link::cost(1));
		let importer = relay + 3;
		net.leaf(importer);
		net.join(importer, relay, 1);
		net.originate(importer, importer as u64, 0);
		net.claim(importer as u64, "live/", 0, Some(1));
	}
	assert!(net.run());
	net
}

/// Per-origin routes are loop-free, yet a path-only request picks a different
/// origin at every hop and circles, past requester exclusion. Naming the
/// identity or bounding the total delivers.
#[test]
fn local_priorities_loop_path_only_requests() {
	let net = priority_ring(1);
	assert!(matches!(net.request(0, "live/x", PATH_ONLY), Outcome::Loop { .. }));
	assert_eq!(served_by(&net.request(0, "live/x", Forward::Pinned)), Some((4, 4)));
	assert_eq!(served_by(&net.request(0, "live/x", Forward::Bounded)), Some((4, 4)));
	// The downstream condition needs no wire field but overrides the priority.
	assert!(served_by(&net.request(0, "live/x", Forward::Downhill)).is_some());

	// Without priorities the total is a potential that strictly decreases.
	let net = priority_ring(0);
	assert_eq!(served_by(&net.request(0, "live/x", PATH_ONLY)), Some((3, 3)));
}

/// The same ring with every relay subscribing at once. Each forwarded request
/// joins the subscription its neighbor already opened, and no source feeds the
/// cycle. Joining by path or by epoch deadlocks; joining only a subscription to
/// the same identity, or one within the request's bound, does not.
#[test]
fn aggregation_cycles_starve_without_a_forwarding_constraint() {
	let net = priority_ring(1);
	let all = [0, 1, 2];
	assert!(loops(&net.subscribe_all("live/x", &all, PATH_ONLY, Aggregate::Path)));
	assert!(loops(&net.subscribe_all(
		"live/x",
		&all,
		Forward::Pinned,
		Aggregate::Epoch
	)));
	for (mode, aggregate) in [
		(Forward::Pinned, Aggregate::Identity),
		(Forward::Bounded, Aggregate::Bound),
	] {
		let outcomes = net.subscribe_all("live/x", &all, mode, aggregate);
		assert!(
			outcomes.iter().all(|outcome| served_by(outcome).is_some()),
			"{outcomes:?}"
		);
	}
}

/// Without an epoch, the serving identity is the instance. A relay that picks
/// another epochless origin within the requester's bound splices a different
/// publisher under the requester's announced one, with no Restart. Only a
/// request naming the identity keeps the instance the requester announced.
#[test]
fn epochless_reselection_splices_another_publisher() {
	let (edge, core, first, second) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(edge, core, 1);
	net.leaf(first);
	net.leaf(second);
	net.join(first, core, 2);
	net.join(second, core, 1);
	net.originate(first, 2, 0);
	net.originate(second, 3, 0);
	net.claim(2, "live/x", 0, None);
	let late = net.claim(3, "live/x", 0, None);
	// The edge has not heard the second claim yet.
	net.hide(edge, late);
	assert!(net.run());

	assert_eq!(net.resolve(edge, "live/x"), Some(2));
	for mode in [PATH_ONLY, Forward::Downhill, Forward::Bounded] {
		assert_eq!(served_by(&net.request(edge, "live/x", mode)), Some((3, 3)), "{mode:?}");
	}
	assert_eq!(served_by(&net.request(edge, "live/x", Forward::Pinned)), Some((2, 2)));
}

/// Nested claims `live/`, `live/a/`, and `live/a/x` in a ring with a chord,
/// with no priorities and symmetric costs. Each relay takes the longest claim
/// it has heard, so a relay missing the specific claims forwards toward the
/// wildcard while its neighbor forwards toward a more specific origin back
/// through it (RFC 8966 s2.8). The cycle has three hops, so requester
/// exclusion misses it. A request naming the identity follows one origin's
/// loop-free routes.
#[test]
fn overlapping_wildcards_loop_where_specific_claims_are_unheard() {
	let mut net = Net::new(Config::default(), 7);
	net.join(0, 1, 2);
	net.join(1, 2, 1);
	net.join(2, 3, 3);
	net.join(3, 0, 1);
	net.join(0, 2, 2);
	let mut claims = Vec::new();
	for (origin, relay, prefix, seed) in [(4, 2, "live/", 1), (5, 1, "live/a/", 1), (6, 3, "live/a/x", 0)] {
		net.leaf(origin);
		net.join(origin, relay, 1);
		net.originate(origin, origin as u64, 0);
		claims.push(net.claim(origin as u64, prefix, seed, None));
	}
	// Relay 0 has heard only the wildcard, relay 2 not the exact path.
	net.hide(0, claims[1]);
	net.hide(0, claims[2]);
	net.hide(2, claims[2]);
	assert!(net.run());

	assert_eq!(
		net.request(0, "live/a/x", PATH_ONLY),
		Outcome::Loop {
			path: vec![0, 2, 1, 0, 2]
		}
	);
	assert_eq!(served_by(&net.request(0, "live/a/x", Forward::Pinned)), Some((4, 4)));
	assert_eq!(served_by(&net.request(1, "live/a/x", Forward::Pinned)), Some((6, 6)));
}

/// A deterministic sweep over small random meshes: priorities, overlapping
/// wildcards, epochs, and claims some nodes have not heard. Pinned and bounded
/// requests never loop or deadlock; path-only requests and the wire-free
/// downstream condition do. A pinned request always arrives, while a bounded
/// one stalls where a transit hop has not heard the claim.
#[test]
fn sweep_forwarding_progress() {
	let mut rng = XorShift(0x5eed_c0de);
	let (mut path_only_loops, mut downhill_loops, mut path_deadlocks, mut bounded_stalls) = (0, 0, 0, 0);
	for scenario in 0..400 {
		let net = random_mesh(&mut rng);
		for at in 0..RELAYS {
			for path in ["live/a/x", "live/b"] {
				let label = format!("scenario {scenario} at {at} for {path}");
				for mode in [Forward::Pinned, Forward::Bounded] {
					assert!(
						!matches!(net.request(at, path, mode), Outcome::Loop { .. }),
						"{mode:?} {label}"
					);
				}
				if net.resolve(at, path).is_some() {
					// A pinned hop needs only a route; a bounded one needs the claim too.
					assert!(served_by(&net.request(at, path, Forward::Pinned)).is_some(), "{label}");
					bounded_stalls +=
						matches!(net.request(at, path, Forward::Bounded), Outcome::Stalled { .. }) as usize;
				}
				path_only_loops += matches!(net.request(at, path, PATH_ONLY), Outcome::Loop { .. }) as usize;
				downhill_loops += matches!(net.request(at, path, Forward::Downhill), Outcome::Loop { .. }) as usize;
			}
		}
		let all: Vec<usize> = (0..RELAYS).collect();
		for path in ["live/a/x", "live/b"] {
			// Joining by identity delivers exactly what each request would alone.
			let alone: Vec<Outcome> = all.iter().map(|&at| net.request(at, path, Forward::Pinned)).collect();
			assert_eq!(
				net.subscribe_all(path, &all, Forward::Pinned, Aggregate::Identity),
				alone,
				"scenario {scenario} for {path}"
			);
			assert!(!loops(&net.subscribe_all(
				path,
				&all,
				Forward::Bounded,
				Aggregate::Bound
			)));
			path_deadlocks += loops(&net.subscribe_all(path, &all, PATH_ONLY, Aggregate::Path)) as usize;
		}
	}
	assert!(path_only_loops > 0, "the sweep must reach a path-only loop");
	assert!(downhill_loops > 0, "the sweep must reach a downstream-condition loop");
	assert!(path_deadlocks > 0, "the sweep must reach a path aggregation deadlock");
	assert!(
		bounded_stalls > 0,
		"a bounded hop that has not heard the claim must stall"
	);
}

const RELAYS: usize = 5;

struct XorShift(u64);

impl XorShift {
	fn next(&mut self, below: u64) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0 % below
	}

	fn chance(&mut self, percent: u64) -> bool {
		self.next(100) < percent
	}
}

fn random_mesh(rng: &mut XorShift) -> Net {
	let origins = 3;
	let mut net = Net::new(Config::default(), RELAYS + origins);
	// A ring keeps it connected; chords add alternatives.
	for relay in 0..RELAYS {
		let next = (relay + 1) % RELAYS;
		let link = |rng: &mut XorShift| Link::cost(1 + rng.next(3)).priority(if rng.chance(25) { 1 } else { 0 });
		let (ab, ba) = (link(rng), link(rng));
		net.link(relay, next, ab, ba);
	}
	for _ in 0..2 {
		let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
		if a.abs_diff(b) > 1 && a.abs_diff(b) != RELAYS - 1 {
			net.join(a, b, 1 + rng.next(3));
		}
	}
	let prefixes = ["live/", "live/a/", "live/b"];
	for origin in RELAYS..RELAYS + origins {
		net.leaf(origin);
		net.join(origin, rng.next(RELAYS as u64) as usize, 1);
		net.originate(origin, origin as u64, 0);
		let prefix = prefixes[rng.next(3) as usize];
		let epoch = if rng.chance(50) { Some(1) } else { None };
		let claim = net.claim(origin as u64, prefix, rng.next(3), epoch);
		for relay in 0..RELAYS {
			if rng.chance(20) {
				net.hide(relay, claim);
			}
		}
	}
	assert!(net.run());
	net
}

// Failover: Babel feasibility against longer detours.

/// Redundant cores are feasible successors: losing one fails over at once.
#[test]
fn redundant_cores_fail_over_without_a_seqno_request() {
	let (ingest, core_a, core_b, edge) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(ingest, core_a, 1);
	net.join(ingest, core_b, 1);
	net.join(core_a, core_b, 1);
	net.link(edge, core_a, Link::cost(1).upstream(), Link::cost(1));
	net.link(edge, core_b, Link::cost(1).upstream(), Link::cost(1));
	net.originate(ingest, 7, 0);
	assert!(net.run());
	let primary = net.route(edge, 7).and_then(|route| route.next).unwrap();

	net.disconnect(edge, primary);
	assert!(net.run());
	assert!(
		net.route(edge, 7).is_some(),
		"the other core advertises no more than the lost one"
	);
}

/// A publisher reached directly over P2P and through the CDN. A browser selects
/// freely because nothing routes through it, so losing the P2P session falls
/// back at once. An opted-in native routing node advertised the one-hop route,
/// so the CDN detour is infeasible until the publisher advances its seqno, which
/// it can do because it saw its own P2P session close.
#[test]
fn p2p_fallback_is_immediate_for_leaves_and_needs_a_seqno_for_transit() {
	for transit in [false, true] {
		let (publisher, edge, subscriber) = (0, 1, 2);
		let mut net = Net::new(Config::default(), 3);
		net.leaf(publisher);
		if !transit {
			net.leaf(subscriber);
		}
		net.join(publisher, edge, 1);
		net.link(subscriber, edge, Link::cost(1).upstream(), Link::cost(1));
		net.link(subscriber, publisher, Link::cost(1).priority(1), Link::cost(1));
		net.originate(publisher, 9, 0);
		assert!(net.run());
		assert_eq!(net.route(subscriber, 9).and_then(|route| route.next), Some(publisher));

		net.disconnect(subscriber, publisher);
		assert!(net.run());
		assert_eq!(net.starved(subscriber, 9), transit);

		net.bump(publisher);
		assert!(net.run());
		assert_eq!(net.route(subscriber, 9).and_then(|route| route.next), Some(edge));
	}
}

/// A relay whose only peers are upstream sends no update for its down-only
/// route, so it holds no feasibility distance (RFC 8966 s3.7.3) and takes a
/// longer route at the same seqno once its best uplink goes, with no seqno
/// request.
#[test]
fn feasibility_tracks_only_updates_actually_sent() {
	let (relay, c1, c2, origin) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.link(relay, c1, Link::cost(1).upstream(), Link::cost(1));
	net.link(relay, c2, Link::cost(1).upstream(), Link::cost(1));
	net.leaf(origin);
	net.join(origin, c1, 1);
	net.join(origin, c2, 5);
	net.originate(origin, 7, 0);
	assert!(net.run());
	assert_eq!(net.route(relay, 7).and_then(|route| route.next), Some(c1));

	net.disconnect(relay, c1);
	assert!(net.run());
	assert_eq!(net.route(relay, 7).and_then(|route| route.next), Some(c2));
}

// GOAWAY: the lowest preference, never a saturated metric.

/// An edge with two upstream cores, preferring the first. A GOAWAY from that
/// core ranks it below every eligible peer, priority included, and the edge
/// fails over at once to the other core, which is feasible. The draining core
/// stays a last resort while its session is up.
#[test]
fn goaway_fails_over_and_keeps_the_draining_peer_last() {
	let (ingest, core_a, core_b, edge) = (0, 1, 2, 3);
	let mut net = Net::new(Config::default(), 4);
	net.join(ingest, core_a, 1);
	net.join(ingest, core_b, 1);
	net.link(edge, core_a, Link::cost(1).upstream().priority(1), Link::cost(1));
	net.link(edge, core_b, Link::cost(1).upstream(), Link::cost(1));
	net.originate(ingest, 7, 0);
	assert!(net.run());
	let next = |net: &Net| net.route(edge, 7).and_then(|route| route.next);
	assert_eq!(next(&net), Some(core_a));

	net.goaway(core_a);
	assert!(net.run());
	assert_eq!(next(&net), Some(core_b));

	net.disconnect(edge, core_b);
	assert!(net.run());
	assert_eq!(next(&net), Some(core_a), "the draining core is the last resort");
}

/// An edge serving a client through two uplinks, where the second uplink's
/// route is longer than the one the edge advertised. Retracting the first
/// route, as a saturated drain metric would, starves the edge and the client
/// until the origin refreshes its seqno. A GOAWAY instead keeps the draining
/// uplink serving while the alternative is infeasible.
///
/// The refresh reaches the edge through both uplinks. If the alternative's
/// arrives first, the edge moves. If the draining uplink's arrives first, the
/// edge selects it at the new seqno and lowers its feasibility distance, so the
/// alternative stays infeasible and the edge keeps the draining uplink until its
/// session closes.
#[test]
fn goaway_keeps_serving_until_a_refresh_makes_the_alternative_feasible() {
	let (origin, a, b, edge, client) = (0, 1, 2, 3, 4);
	let build = || {
		let mut net = Net::new(Config::default(), 5);
		net.leaf(origin);
		net.leaf(client);
		net.join(origin, a, 1);
		net.join(origin, b, 5);
		net.link(edge, a, Link::cost(1).upstream(), Link::cost(1));
		net.link(edge, b, Link::cost(1).upstream(), Link::cost(1));
		net.join(edge, client, 1);
		net.originate(origin, 7, 0);
		assert!(net.run());
		net
	};
	let next = |net: &Net| net.route(edge, 7).and_then(|route| route.next);

	let mut net = build();
	net.disconnect(edge, a);
	assert!(net.run());
	assert!(net.starved(edge, 7));
	assert!(net.route(client, 7).is_none());

	for draining_first in [false, true] {
		let mut net = build();
		net.goaway(a);
		assert!(net.run());
		assert_eq!(next(&net), Some(a), "b advertised 5, above the edge's distance of 2");
		assert!(net.route(client, 7).is_some());

		let late = if draining_first { b } else { a };
		net.hold(late, edge);
		net.bump(origin);
		assert!(net.run());
		net.release(late, edge);
		assert!(net.run());
		assert!(net.route(client, 7).is_some());
		if draining_first {
			assert_eq!(next(&net), Some(a), "the refresh through a lowered the distance first");
			net.disconnect(edge, a);
			assert!(net.run());
			assert!(net.starved(edge, 7), "only another refresh moves the edge to b");
		} else {
			assert_eq!(next(&net), Some(b));
		}
	}
}

// Export: one selection per identity, with a down-only bit.

/// Two CDN relays, each the uplink of one relay in a local mesh, reaching a CDN
/// origin claiming `cdn/`. The CDN admits each local relay under `grant`. The
/// caller runs it.
fn two_uplinks(cfg: Config, l2_marks_upstream: bool, grant: Option<Grant>) -> Net {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(cfg, 5);
	net.join(c1, c2, 10);
	net.join(l1, l2, 1);
	let admit = Link { grant, ..Link::cost(1) };
	let up = if l2_marks_upstream {
		Link::cost(1).upstream()
	} else {
		Link::cost(1)
	};
	net.link(l1, c1, Link::cost(1).upstream(), admit);
	net.link(l2, c2, up, admit);
	net.leaf(origin);
	net.join(origin, c1, 1);
	net.originate(origin, 7, 0);
	net.claim(7, "cdn/", 0, None);
	net
}

/// Without export policy the CDN routes between its own relays through the
/// LAN; the down-only bit keeps it on the CDN link while the local relays still
/// reach the CDN origin.
#[test]
fn two_uplinks_never_carry_cdn_transit() {
	let (c1, c2, l2) = (0, 1, 3);
	let next = |net: &Net| net.route(c2, 7).and_then(|route| route.next);
	let mut net = two_uplinks(
		Config {
			export: Export::Anything,
			..Config::default()
		},
		true,
		None,
	);
	assert!(net.run());
	assert_eq!(next(&net), Some(l2));
	let mut net = two_uplinks(Config::default(), true, None);
	assert!(net.run());
	assert_eq!(next(&net), Some(c1));
	assert!(net.route(l2, 7).is_some());
}

/// The topology that broke the two-view export model: C1-C2 at 100, a publisher
/// linked to C1 at 1 and to L1 at 10, the LAN L1-L2 at 1, and each local relay
/// marking its CDN link upstream. `uplink` decides whether L1's is up yet.
fn lan_beside_the_cdn(cfg: Config, uplink: bool) -> Net {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(cfg, 5);
	net.join(c1, c2, 100);
	net.leaf(origin);
	net.join(origin, c1, 1);
	net.join(origin, l1, 10);
	net.join(l1, l2, 1);
	net.link(l2, c2, Link::cost(1).upstream(), Link::cost(1));
	net.originate(origin, 7, 0);
	net.claim(7, "live/", 0, None);
	assert!(net.run());
	if uplink {
		net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
		assert!(net.run());
	}
	net
}

/// L1's best route runs through C1, so it is down-only and L2 exports nothing
/// to C2, which pays its internal link. With no down-only bit, as with the
/// export view that lost its restriction one LAN hop after the upstream
/// ingress, C2 reaches C1 through the LAN.
#[test]
fn cdn_never_reaches_its_own_relay_through_the_lan() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let served = |export| {
		let cfg = Config {
			export,
			..Config::default()
		};
		lan_beside_the_cdn(cfg, true).request(c2, "live/x", Forward::Pinned)
	};
	assert_eq!(
		served(Export::Anything),
		Outcome::Served {
			by: 7,
			path: vec![c2, l2, l1, c1, origin]
		}
	);
	assert_eq!(
		served(Export::DownOnly),
		Outcome::Served {
			by: 7,
			path: vec![c2, c1, origin]
		}
	);
}

/// Before L1's uplink comes up, L1 reaches the publisher directly and C2
/// subscribes through the LAN. The uplink then turns L1's route down-only
/// through C1. While L2's retraction is still in flight to C2, L2, which
/// accepted C2's request from an upstream link, must end it. L1 cannot tell the
/// request came from upstream, since no request bit goes on the wire, and
/// would forward it to C1. Once the retraction lands, C2 re-resolves.
#[test]
fn upstream_ingress_ends_a_request_whose_route_turns_down_only() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	for recheck in [false, true] {
		let cfg = Config {
			recheck,
			..Config::default()
		};
		let mut net = lan_beside_the_cdn(cfg, false);
		assert_eq!(
			net.request(c2, "live/x", Forward::Pinned),
			Outcome::Served {
				by: 7,
				path: vec![c2, l2, l1, origin]
			}
		);

		net.hold(l2, c2);
		net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
		assert!(net.run());
		assert_eq!(net.route(c2, 7).and_then(|route| route.next), Some(l2));
		let outcome = net.request(c2, "live/x", Forward::Pinned);
		if recheck {
			assert_eq!(outcome, Outcome::Stalled { path: vec![c2, l2] });
		} else {
			assert_eq!(
				outcome,
				Outcome::Served {
					by: 7,
					path: vec![c2, l2, l1, c1, origin]
				}
			);
		}
		// L2's own reader follows the down-only route, and C2's request must not
		// join that subscription either.
		assert_eq!(
			net.subscribe_all("live/x", &[c2, l2], Forward::Pinned, Aggregate::Identity),
			vec![
				outcome,
				Outcome::Served {
					by: 7,
					path: vec![l2, l1, c1, origin]
				}
			]
		);

		net.release(l2, c2);
		assert!(net.run());
		assert_eq!(
			net.request(c2, "live/x", Forward::Pinned),
			Outcome::Served {
				by: 7,
				path: vec![c2, c1, origin]
			}
		);
	}
}

/// A local relay whose own uplink is down still consumes CDN content and
/// publishes local content through its LAN peer: down-only routes flow down
/// the LAN, and exportable ones up the other uplink.
#[test]
fn dead_uplink_consumes_and_publishes_through_a_lan_peer() {
	let (c1, c2, l1, l2, cdn, local) = (0, 1, 2, 3, 4, 5);
	let mut net = Net::new(Config::default(), 6);
	net.join(c1, c2, 10);
	net.join(l1, l2, 1);
	net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
	net.link(l2, c2, Link::cost(1).upstream(), Link::cost(1));
	net.disconnect(l2, c2);
	for (origin, relay, id, prefix) in [(cdn, c1, 7, "cdn/"), (local, l2, 9, "local/")] {
		net.leaf(origin);
		net.join(origin, relay, 1);
		net.originate(origin, id, 0);
		net.claim(id, prefix, 0, None);
	}
	assert!(net.run());

	assert_eq!(
		net.request(l2, "cdn/x", Forward::Pinned),
		Outcome::Served {
			by: 7,
			path: vec![l2, l1, c1, cdn]
		}
	);
	assert_eq!(
		net.request(c2, "local/x", Forward::Pinned),
		Outcome::Served {
			by: 9,
			path: vec![c2, c1, l1, l2, local]
		}
	);
}

/// A partitioned local mesh reaches its other half through the CDN, and heals
/// back onto the LAN. The detour is longer than the route each island
/// advertised, so it waits on a seqno request answered by the origin.
#[test]
fn partitioned_local_mesh_reaches_itself_through_the_cdn() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(Config::default(), 5);
	net.join(c1, c2, 10);
	net.join(l1, l2, 1);
	net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
	net.link(l2, c2, Link::cost(1).upstream(), Link::cost(1));
	net.leaf(origin);
	net.join(origin, l2, 1);
	net.originate(origin, 7, 0);
	net.claim(7, "local/", 0, None);
	assert!(net.run());
	assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(l2));

	net.disconnect(l1, l2);
	assert!(net.run());
	assert!(net.starved(l1, 7), "the CDN detour is longer than the LAN route");

	net.bump(origin);
	assert!(net.run());
	assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(c1));
	assert_eq!(
		served_by(&net.request(l1, "local/x", Forward::Pinned)),
		Some((7, origin))
	);

	net.connect(l1, l2);
	assert!(net.run());
	assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(l2), "heals");
}

/// A local relay L2 whose LAN path to a local origin is cheap, behind a CDN
/// whose internal link is expensive. `lan_first` decides which session comes
/// up first, and `prefer_uplink` whether L2's application ranks its uplink
/// above the LAN or the LAN above it.
fn preferred_uplink(lan_first: bool, prefer_uplink: bool) -> Net {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(Config::default(), 5);
	net.join(c1, c2, 10);
	net.leaf(origin);
	net.join(origin, l1, 1);
	net.originate(origin, 7, 0);
	net.claim(7, "local/", 0, None);
	let (uplink, lan) = if prefer_uplink { (1, 0) } else { (0, 1) };
	let lan = |net: &mut Net| net.link(l2, l1, Link::cost(1).priority(lan), Link::cost(1));
	if lan_first {
		lan(&mut net);
	}
	net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
	net.link(l2, c2, Link::cost(1).upstream().priority(uplink), Link::cost(1));
	assert!(net.run());
	if !lan_first {
		lan(&mut net);
	}
	assert!(net.run());
	net
}

/// With one selection per identity, a relay whose preferred route is down-only
/// exports nothing, so the CDN hauls the origin across its expensive internal
/// link while a LAN path costing 3 sits unadvertised. This is accepted; ranking
/// local peers above a backup uplink avoids it.
#[test]
fn preferred_down_only_route_hides_the_exportable_alternative() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let next = |net: &Net, node| net.route(node, 7).and_then(|route| route.next);

	let net = preferred_uplink(false, true);
	assert_eq!(next(&net, l2), Some(c2));
	assert!(net.export_route(l2, 7).is_none());
	assert_eq!(next(&net, c2), Some(c1));
	assert_eq!(net.route(c2, 7).map(|route| route.metric), Some(12));

	let net = preferred_uplink(false, false);
	assert_eq!(next(&net, l2), Some(l1));
	assert_eq!(
		net.request(c2, "local/x", Forward::Pinned),
		Outcome::Served {
			by: 7,
			path: vec![c2, l2, l1, origin]
		}
	);
}

/// Babel selects only feasible routes, so a transit relay's priority for a
/// longer route holds only if that route arrived before the shorter one was
/// advertised. The same topology settles on different routes depending on
/// which session came up first.
#[test]
fn transit_priority_depends_on_arrival_order() {
	let (c2, l1, l2) = (1, 2, 3);
	let next = |lan_first| {
		preferred_uplink(lan_first, true)
			.route(l2, 7)
			.and_then(|route| route.next)
	};
	assert_eq!(next(false), Some(c2));
	assert_eq!(next(true), Some(l1));
}

// Subject-qualified identities: a forwarded identity cannot reach beyond its admission.

/// A scoped peer forwards a victim's identity with a newer seqno. Routes are
/// shared by every claim of an identity, so unscoped, the forgery captures
/// claims outside the forger's namespace, and its seqno outlives it: after it
/// leaves, the victim's real routes are refused until the victim counts past
/// the forged seqno. Filed under `hash(subject, id)`, the forgery serves only
/// claims inside the forger's scope. The forger's own proven identity stays
/// bare either way.
#[test]
fn forged_identity_hijacks_claims_outside_the_forgers_scope() {
	for scoped_admission in [false, true] {
		let (victim, core, edge, forger) = (0, 1, 2, 3);
		let mut net = Net::new(Config::default(), 4);
		net.leaf(victim);
		net.leaf(forger);
		net.join(victim, core, 5);
		net.join(core, edge, 1);
		let admission = if scoped_admission {
			Link::cost(1).grant(Grant {
				subject: "forger",
				scope: "forger/",
			})
		} else {
			Link::cost(1)
		};
		net.link(edge, forger, admission, Link::cost(1));
		net.originate(victim, 7, 0);
		net.originate(forger, 3, 0);
		net.claim(7, "other/", 0, None);
		net.claim(7, "forger/", 0, None);
		assert!(net.run());

		net.forge(forger, 7, 1000);
		assert!(net.run());
		assert!(net.route(core, 3).is_some(), "a connection's own identity is bare");
		let served = served_by(&net.request(core, "other/x", Forward::Pinned));
		if scoped_admission {
			assert_eq!(served, Some((7, victim)));
			assert_eq!(net.resolve(core, "forger/x"), Some(qualified("forger", 7)));
			assert_eq!(
				served_by(&net.request(core, "forger/x", Forward::Pinned)),
				Some((7, forger)),
				"the forgery is contained in its own scope"
			);
		} else {
			assert_eq!(served, Some((7, forger)), "the forger serves another namespace");
		}

		net.kill(forger);
		assert!(net.run());
		assert_eq!(
			net.starved(core, 7),
			!scoped_admission,
			"the forged seqno outlives the forger"
		);
	}
}

/// A local mesh publishing through two uplinks, each admitted under the same
/// subject. Both CDN relays file the local origin under one key, so it survives
/// losing an uplink: the first hop keeps its identity and the request follows
/// the other uplink, named as the local relay knows it. Claims outside the
/// scope are refused under that key, and the local relay's own identity stays
/// bare.
#[test]
fn scoped_delivery_through_two_uplinks_reaches_one_key() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(Config::default(), 5);
	net.join(c1, c2, 10);
	net.join(l1, l2, 1);
	let admit = Link::cost(1).grant(LOCAL);
	net.link(l1, c1, Link::cost(1).upstream(), admit);
	net.link(l2, c2, Link::cost(1).upstream(), admit);
	net.leaf(origin);
	net.join(origin, l1, 1);
	net.originate(origin, 9, 0);
	net.originate(l1, 20, 0);
	net.claim(9, "local/", 0, None);
	net.claim(9, "cdn/", 0, None);
	assert!(net.run());

	let key = qualified(LOCAL.subject, 9);
	assert!(net.route(c1, 9).is_none());
	assert_eq!(net.route(c1, key).and_then(|route| route.next), Some(l1));
	assert_eq!(net.route(c2, key).and_then(|route| route.next), Some(l2));
	assert_eq!(net.route(c1, 20).and_then(|route| route.next), Some(l1));
	assert_eq!(net.resolve(c2, "local/x"), Some(key));
	assert_eq!(
		net.request(c2, "local/x", Forward::Pinned),
		Outcome::Served {
			by: 9,
			path: vec![c2, l2, l1, origin]
		}
	);
	assert_eq!(net.resolve(c2, "cdn/x"), None, "outside the scope");
	assert_eq!(net.resolve(l1, "cdn/x"), Some(9));

	net.disconnect(l2, c2);
	assert!(net.run());
	assert_eq!(net.route(c2, key).and_then(|route| route.next), Some(c1));
	assert_eq!(
		net.request(c2, "local/x", Forward::Pinned),
		Outcome::Served {
			by: 9,
			path: vec![c2, c1, l1, origin]
		}
	);

	// A retracted identity leaves no alias behind.
	assert_eq!(net.aliases(c1), 1);
	net.kill(origin);
	assert!(net.run());
	assert_eq!(net.aliases(c1), 0);
}

/// A local relay missing its `upstream` mark leaks CDN routes back to the CDN,
/// which then reaches its own origin through the LAN. Admitting the local relay
/// under a scoped grant files the leaked identity apart by its subject, where the CDN origin's claims
/// do not count, so the CDN keeps its own route.
#[test]
fn misconfigured_uplink_is_contained_by_qualified_identities() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = two_uplinks(Config::default(), false, None);
	assert!(net.run());
	assert_eq!(net.route(c2, 7).and_then(|route| route.next), Some(l2));
	assert_eq!(
		net.request(c2, "cdn/x", Forward::Pinned),
		Outcome::Served {
			by: 7,
			path: vec![c2, l2, l1, c1, origin]
		}
	);

	let mut net = two_uplinks(Config::default(), false, Some(LOCAL));
	assert!(net.run());
	assert_eq!(net.route(c2, 7).and_then(|route| route.next), Some(c1));
	assert_eq!(
		net.route(c2, qualified(LOCAL.subject, 7)).and_then(|route| route.next),
		Some(l2),
		"the leak is filed apart"
	);
	assert_eq!(
		net.request(c2, "cdn/x", Forward::Pinned),
		Outcome::Served {
			by: 7,
			path: vec![c2, c1, origin]
		}
	);
}

/// A qualified key the CDN sent down comes back up through the misconfigured
/// uplink. Filed again as `hash(subject, hash(subject, id))`, it is a new
/// identity on every lap, so the CDN never settles. Qualified keys are
/// recognizable, and a scoped connection forwarding one has that route refused, so the lap ends and
/// the CDN holds what correctly marked uplinks give it. Nested scoped
/// federation is therefore out: the inner level must be unscoped.
#[test]
fn scoped_connections_cannot_requalify_a_qualified_identity() {
	let c2 = 1;
	let mut net = two_uplinks(Config::default(), true, Some(LOCAL));
	assert!(net.run());
	let marked = net.identities(c2);

	let mut net = two_uplinks(Config::default(), false, Some(LOCAL));
	assert!(net.run());
	assert_eq!(net.identities(c2), marked + 1, "the leak is filed once, never again");

	let cfg = Config {
		requalify: true,
		..Config::default()
	};
	let mut net = two_uplinks(cfg, false, Some(LOCAL));
	let mut known = Vec::new();
	for _ in 0..4 {
		assert!(!net.settle(10));
		known.push(net.identities(c2));
	}
	assert!(known.windows(2).all(|pair| pair[0] < pair[1]), "{known:?}");
	assert!(known[0] > marked, "{known:?} against {marked}");
}

// Selection: every node agrees on a pool member.

/// Cross-origin ties hash the path and identity, so every node agrees on a
/// pool member; hashing a node-local view does not.
#[test]
fn pool_ties_agree_only_on_a_path_and_identity_hash() {
	let disagreements = |tie| {
		let (edge_a, edge_b, hub_a, hub_b) = (0, 1, 2, 3);
		let mut net = Net::new(
			Config {
				tie,
				..Config::default()
			},
			7,
		);
		net.join(edge_a, hub_a, 1);
		net.join(edge_b, hub_b, 1);
		net.join(hub_a, hub_b, 1);
		for member in 4..7 {
			net.leaf(member);
			net.join(member, hub_a, 1);
			net.join(member, hub_b, 1);
			net.originate(member, member as u64, 0);
			net.claim(member as u64, "transcode/", 0, None);
		}
		assert!(net.run());
		(0..64)
			.filter(|i| {
				let path = format!("transcode/{i}");
				net.resolve(edge_a, &path) != net.resolve(edge_b, &path)
			})
			.count()
	};
	assert_eq!(disagreements(TieBreak::PathIdentity), 0);
	assert!(disagreements(TieBreak::PathHere) > 0);
}

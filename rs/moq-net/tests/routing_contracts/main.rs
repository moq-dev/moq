//! Deterministic counterexamples for the per-origin routing contracts.
//!
//! Each fixture runs one scenario from `quest/m1/cluster-routing/contracts.md`
//! against [`model`], under the rule that fails and the rule proposed to hold.
//! The model is a reference for the contracts, not the relay: when lite-07's
//! route layer lands, these topologies become its acceptance tests.

mod model;

use model::{Aggregate, Config, Export, Forward, Link, Net, Outcome, Rule, TieBreak, scoped_id};

const PATH_ONLY: Forward = Forward::PathOnly {
	exclude_requester: true,
};

fn served_by(outcome: &Outcome) -> Option<(u64, usize)> {
	match outcome {
		Outcome::Served { by, path } => Some((*by, *path.last().unwrap())),
		_ => None,
	}
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
	assert!(net.route(c, 7).is_none(), "c's feasibility distance refuses b's copy");

	net.release(a, b);
	assert!(net.run());
	for node in 0..4 {
		assert!(
			net.route(node, 7).is_none(),
			"node {node} still routes to a dead incarnation"
		);
	}
}

// Metric: strictly increasing, never saturating.

/// Babel's feasibility needs every link to increase the metric (RFC 8966
/// s3.5.2). A zero-cost link, which `?cost=0` allows today, or a cost saturated
/// at the ceiling, leaves a route equal to its own feasibility distance, so the
/// next recomputation refuses it. Ordering by cost then hops keeps the
/// configured prices and still increases on every link.
#[test]
fn zero_cost_and_saturated_links_need_hops_in_the_metric() {
	for (origin_cost, sibling_cost, ceiling) in [(10, 0, u64::MAX / 2), (100, 5, 100)] {
		let (origin, a, b, late) = (0, 1, 2, 3);
		let build = |rule| {
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
			// Any later event recomputes b's selection.
			net.join(b, late, 1);
			assert!(net.run());
			net
		};

		let net = build(Rule::CostOnly { ceiling });
		assert!(
			net.starved(b, 7),
			"cost {sibling_cost} under ceiling {ceiling} does not increase"
		);

		let net = build(Rule::Lexicographic { ceiling });
		let route = net.route(b, 7);
		if origin_cost + sibling_cost > ceiling {
			assert!(route.is_none(), "beyond the ceiling is unreachable, not repriced");
		} else {
			assert_eq!(route.map(|route| route.metric.cost), Some(origin_cost + sibling_cost));
		}
	}
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
	assert!(net.subscribe_all("live/x", &all, PATH_ONLY, Aggregate::Path).is_some());
	assert!(
		net.subscribe_all("live/x", &all, Forward::Pinned, Aggregate::Epoch)
			.is_some()
	);
	assert_eq!(
		net.subscribe_all("live/x", &all, Forward::Pinned, Aggregate::Identity),
		None
	);
	assert_eq!(
		net.subscribe_all("live/x", &all, Forward::Bounded, Aggregate::Bound),
		None
	);
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
	net.join(0, 2, 3);
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
			assert_eq!(
				net.subscribe_all(path, &all, Forward::Pinned, Aggregate::Identity),
				None
			);
			assert_eq!(net.subscribe_all(path, &all, Forward::Bounded, Aggregate::Bound), None);
			path_deadlocks += net.subscribe_all(path, &all, PATH_ONLY, Aggregate::Path).is_some() as usize;
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

// Export policy.

/// Two CDN relays, each the uplink of one relay in a local mesh, reaching a CDN
/// origin. Without export policy the CDN routes between its own relays through
/// the LAN; a down-only mark or an export view keeps it on the CDN link.
fn two_uplinks(export: Export, local_marks_upstream: bool, cdn_marks_downstream: bool) -> Net {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(
		Config {
			export,
			..Config::default()
		},
		5,
	);
	net.join(c1, c2, 10);
	net.join(l1, l2, 1);
	let up = |marked| {
		if marked {
			Link::cost(1).upstream()
		} else {
			Link::cost(1)
		}
	};
	let down = if cdn_marks_downstream {
		Link::cost(1).downstream()
	} else {
		Link::cost(1)
	};
	net.link(l1, c1, up(true), down);
	net.link(l2, c2, up(local_marks_upstream), down);
	net.leaf(origin);
	net.join(origin, c1, 1);
	net.originate(origin, 7, 0);
	assert!(net.run());
	net
}

#[test]
fn two_uplinks_never_carry_cdn_transit() {
	let (c1, c2, l2) = (0, 1, 3);
	let next = |net: &Net| net.route(c2, 7).and_then(|route| route.next);
	assert_eq!(next(&two_uplinks(Export::Anything, true, false)), Some(l2));
	assert_eq!(next(&two_uplinks(Export::DownOnly, true, false)), Some(c1));
	assert_eq!(next(&two_uplinks(Export::Views, true, false)), Some(c1));
	assert!(two_uplinks(Export::DownOnly, true, false).route(l2, 7).is_some());
}

/// The down-only mark is enforced only by the side that sets it. A local relay
/// missing its `upstream` mark leaks CDN routes back to the CDN. RFC 9234 makes
/// the role bilateral: the CDN marks the federation peer downstream, marks what
/// it sends down-only, and takes only the export view from it.
#[test]
fn misconfigured_uplink_leaks_unless_the_cdn_marks_its_downstream() {
	let (c1, c2, l2) = (0, 1, 3);
	let next = |net: &Net| net.route(c2, 7).and_then(|route| route.next);
	for export in [Export::DownOnly, Export::Views] {
		assert_eq!(next(&two_uplinks(export, false, false)), Some(l2), "{export:?} leaks");
		assert_eq!(
			next(&two_uplinks(export, false, true)),
			Some(c1),
			"{export:?} with roles"
		);
	}
}

/// A local mesh split in two reaches its other half through the CDN, and heals
/// back onto the LAN. The detour is longer than the route each island
/// advertised, so it waits on a seqno request answered by the origin.
#[test]
fn partitioned_local_mesh_reaches_itself_through_the_cdn() {
	for export in [Export::DownOnly, Export::Views] {
		let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
		let mut net = Net::new(
			Config {
				export,
				..Config::default()
			},
			5,
		);
		net.join(c1, c2, 10);
		net.join(l1, l2, 1);
		net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1).downstream());
		net.link(l2, c2, Link::cost(1).upstream(), Link::cost(1).downstream());
		net.leaf(origin);
		net.join(origin, l2, 1);
		net.originate(origin, 7, 0);
		net.claim(7, "local/", 0, None);
		assert!(net.run());
		assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(l2));

		net.disconnect(l1, l2);
		assert!(net.run());
		assert!(
			net.starved(l1, 7),
			"{export:?}: the CDN detour is longer than the LAN route"
		);

		net.bump(origin);
		assert!(net.run());
		assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(c1), "{export:?}");
		assert_eq!(
			served_by(&net.request(l1, "local/x", Forward::Pinned)),
			Some((7, origin))
		);

		net.connect(l1, l2);
		assert!(net.run());
		assert_eq!(
			net.route(l1, 7).and_then(|route| route.next),
			Some(l2),
			"{export:?} heals"
		);
	}
}

/// A local relay whose application prefers its uplink, next to a cheap LAN
/// path to a local origin, behind a CDN whose internal link is expensive.
/// `lan_first` decides which session comes up first.
fn preferred_uplink(export: Export, lan_first: bool) -> Net {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut net = Net::new(
		Config {
			export,
			..Config::default()
		},
		5,
	);
	net.join(c1, c2, 10);
	net.leaf(origin);
	net.join(origin, l1, 1);
	net.originate(origin, 7, 0);
	net.claim(7, "local/", 0, None);
	if lan_first {
		net.join(l1, l2, 1);
	}
	net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1).downstream());
	net.link(l2, c2, Link::cost(1).upstream().priority(1), Link::cost(1).downstream());
	assert!(net.run());
	if !lan_first {
		net.join(l1, l2, 1);
	}
	assert!(net.run());
	net
}

/// With one selection per identity, a relay whose preferred route is down-only
/// exports nothing, so the CDN hauls the origin across its expensive internal
/// link while a LAN path costing 3 sits unadvertised. An export view keeps
/// offering the LAN route upstream, and a request arriving from upstream
/// follows that view instead of looping back up.
#[test]
fn preferred_down_only_route_hides_the_exportable_alternative() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let next = |net: &Net, node| net.route(node, 7).and_then(|route| route.next);

	let net = preferred_uplink(Export::DownOnly, false);
	assert_eq!(next(&net, l2), Some(c2));
	assert!(net.export_route(l2, 7).is_none());
	assert_eq!(next(&net, c2), Some(c1));
	assert_eq!(net.route(c2, 7).map(|route| route.metric.cost), Some(12));

	let net = preferred_uplink(Export::Views, false);
	assert_eq!(next(&net, l2), Some(c2));
	assert_eq!(net.export_route(l2, 7).and_then(|route| route.next), Some(l1));
	assert_eq!(next(&net, c2), Some(l2));
	let served = net.request(c2, "local/x", Forward::Pinned);
	assert_eq!(
		served,
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
	for export in [Export::DownOnly, Export::Views] {
		let next = |lan_first| {
			preferred_uplink(export, lan_first)
				.route(l2, 7)
				.and_then(|route| route.next)
		};
		assert_eq!(next(false), Some(c2), "{export:?}");
		assert_eq!(next(true), Some(l1), "{export:?}");
	}
}

// Authentication: an asserted identity must not reach beyond its admission.

/// A scoped peer copies a victim's identity with a newer seqno. Per-origin
/// routes are shared by every claim, so the forgery captures claims outside the
/// forger's namespace, and its seqno outlives it: after it leaves, the victim's
/// real routes are refused until the victim counts past the forged seqno.
/// Keying an identity learned through a scoped admission to that admission
/// confines the forgery to the claims the forger was allowed to make.
#[test]
fn forged_identity_hijacks_claims_outside_the_forgers_scope() {
	for scoped in [false, true] {
		let (victim, core, edge, forger) = (0, 1, 2, 3);
		let mut net = Net::new(Config::default(), 4);
		net.leaf(victim);
		net.leaf(forger);
		net.join(victim, core, 5);
		net.join(core, edge, 1);
		let admission = if scoped { Link::cost(1).scoped() } else { Link::cost(1) };
		net.link(edge, forger, admission, Link::cost(1));
		net.originate(victim, 7, 0);
		net.claim(7, "other/", 0, None);
		assert!(net.run());

		net.originate(forger, 7, 1000);
		assert!(net.run());
		let served = served_by(&net.request(core, "other/x", Forward::Pinned));
		if scoped {
			assert_eq!(served, Some((7, victim)));
			assert!(
				net.route(core, scoped_id(7, forger)).is_some(),
				"the forgery is filed apart"
			);
		} else {
			assert_eq!(served, Some((7, forger)), "the forger serves another namespace");
		}

		net.kill(forger);
		assert!(net.run());
		assert_eq!(net.starved(core, 7), !scoped, "the forged seqno outlives the forger");
	}
}

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

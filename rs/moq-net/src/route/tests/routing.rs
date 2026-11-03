//! Route selection: identity, feasibility, seqno requests, GOAWAY, the
//! down-only bit, and qualified identities.

use super::*;

const LOCAL: Grant = Grant {
	subject: "local",
	scope: "local/",
};

fn next(sim: &Sim, node: Node, id: Id) -> Option<Node> {
	sim.route(node, id).and_then(|route| route.next)
}

/// A restart that reuses its routing identity restarts its seqno below the
/// feasibility distance every other node still holds, so its routes are
/// refused until a seqno request lifts it past the old process's seqno. A
/// fresh identity has no history to lose to.
#[test]
fn restart_reusing_identity_starves_until_requested() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, a, 1);
	sim.join(a, b, 1);
	sim.originate(origin, 7, 0);
	for _ in 0..3 {
		sim.bump(origin);
	}
	assert!(sim.run());
	assert!(sim.route(b, 7).is_some());

	sim.kill(origin);
	sim.originate(origin, 7, 0);
	sim.reconnect(origin);
	sim.hold(a, origin);
	assert!(sim.run());
	assert!(sim.starved(a, 7), "seqno 0 is older than the 3 a remembers");
	assert!(sim.route(b, 7).is_none());
	sim.release(a, origin);
	assert!(sim.run());
	assert_eq!(sim.seqno(origin), 4, "the request names the seqno after a's distance");
	assert!(sim.route(b, 7).is_some());

	sim.kill(origin);
	sim.originate(origin, 8, 0);
	sim.reconnect(origin);
	let requests = sim.stats.requests;
	assert!(sim.run());
	assert!(sim.route(b, 8).is_some());
	assert_eq!(sim.stats.requests, requests, "a fresh identity needs no request");
}

/// The same process moving to a farther attachment keeps its identity and
/// seqno, so the node next to its old attachment holds a feasibility distance
/// shorter than any route it now has and starves until its seqno request
/// reaches the origin.
#[test]
fn reconnect_without_restart_needs_a_seqno_advance() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(a, b, 1);
	sim.join(b, c, 1);
	sim.join(origin, a, 1);
	sim.join(origin, c, 1);
	sim.disconnect(origin, c);
	sim.originate(origin, 7, 0);
	assert!(sim.run());

	sim.hold(a, b);
	sim.disconnect(origin, a);
	sim.connect(origin, c);
	assert!(sim.run());
	assert!(sim.starved(a, 7), "a advertised one hop; every new route is longer");

	sim.release(a, b);
	assert!(sim.run());
	assert_eq!(sim.seqno(origin), 1);
	assert_eq!(next(&sim, a, 7), Some(b));
}

/// A dead incarnation's route can return after a node collects it, but only
/// from a message already in flight on a live session, and that session then
/// delivers the retraction behind it.
#[test]
fn collected_incarnation_returns_only_while_its_retraction_is_in_flight() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, a, 1);
	sim.join(a, b, 1);
	sim.join(a, c, 1);
	sim.join(c, b, 1);
	// Everything a sends b waits: first the route, then its retraction.
	sim.hold(a, b);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, b, 7), Some(c));

	sim.kill(origin);
	assert!(sim.run());
	assert!(sim.route(b, 7).is_none());

	sim.collect(b, 7);
	assert!(sim.deliver(a, b), "the held update");
	assert!(sim.route(b, 7).is_some(), "the dead incarnation is back at b");
	assert!(sim.run());
	assert!(sim.route(a, 7).is_none());
	assert_loop_free(&sim, 4, 7);

	sim.release(a, b);
	assert!(sim.run());
	for node in 0..4 {
		assert!(sim.route(node, 7).is_none(), "node {node} routes to a dead incarnation");
	}
}

/// No route entry is not proof that no advertisement derived from this node's
/// own is still in flight. b keeps its feasibility distance until every
/// neighbor it advertised to echoes the ROUTE_END back, so its own echo from c
/// is refused rather than looping. A leaf never advertises the identity back,
/// so only its explicit echo ends the drain.
#[test]
fn collection_keeps_the_feasibility_distance_against_a_queued_echo() {
	let (origin, b, c, leaf) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(leaf);
	sim.join(origin, b, 1);
	sim.join(b, c, 1);
	sim.join(b, leaf, 1);
	sim.hold(c, b);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, c, 7), Some(b));
	assert!(sim.queued(c, b) > 0, "c's echo waits");

	sim.hold(b, c);
	sim.hold(leaf, b);
	sim.disconnect(origin, b);
	assert!(!sim.holds(b, 7));
	sim.collect(b, 7);
	assert!(sim.remembers(b, 7), "draining: the ROUTE_ENDs are not echoed yet");
	assert!(sim.deliver(c, b), "the echo of b's own advertisement");
	assert!(sim.route(b, 7).is_none(), "b refuses its own echo");
	assert!(sim.starved(b, 7));

	sim.release(b, c);
	sim.release(c, b);
	assert!(sim.run());
	assert!(sim.queued_acks(leaf, b) > 0, "the leaf's echo waits");
	assert!(sim.remembers(b, 7), "the leaf may still hold b's advertisement");
	sim.release(leaf, b);
	assert!(sim.run());
	for node in [b, c, leaf] {
		assert!(
			sim.route(node, 7).is_none(),
			"node {node} routes to an unreachable origin"
		);
		assert!(!sim.holds(node, 7), "node {node}");
		assert!(!sim.remembers(node, 7), "node {node} keeps a drained distance");
	}
}

/// A restarted relay loses every feasibility distance, yet stays loop-free:
/// its sessions close with the process, and each neighbor drops what it
/// learned on them. A stale echo still reaches it through a third node's
/// session that outlived it, and is safe, because every node on that echo's
/// path kept its own distance.
#[test]
fn restart_drops_the_feasibility_distance_with_every_session() {
	let (origin, b, c, d) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, b, 1);
	sim.join(b, c, 1);
	sim.join(c, d, 1);
	sim.join(d, b, 10);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, d, 7), Some(c));

	// The update c sends d after losing b waits on a session that outlives b.
	sim.hold(c, d);
	sim.kill(b);
	assert!(sim.run());
	assert_loop_free(&sim, 4, 7);
	assert!(sim.route(c, 7).is_none());
	assert_eq!(next(&sim, d, 7), Some(c), "stale");

	sim.connect(b, d);
	assert!(sim.run());
	assert_eq!(
		next(&sim, b, 7),
		Some(d),
		"the old advertisement reaches the new process"
	);
	assert_loop_free(&sim, 4, 7);
	sim.connect(b, c);
	assert!(sim.run());
	assert!(sim.route(c, 7).is_none(), "the distance at c refuses the longer copy");
	assert_loop_free(&sim, 4, 7);

	sim.release(c, d);
	assert!(sim.run());
	assert_loop_free(&sim, 4, 7);
	sim.connect(origin, b);
	assert!(sim.run());
	assert_loop_free(&sim, 4, 7);
	for node in [b, c, d] {
		assert!(sim.route(node, 7).is_some(), "node {node} recovers");
	}
}

/// Random meshes churn links, hold and deliver messages out of step, and
/// collect route-less identities. Each node drops a feasibility distance only
/// once every neighbor echoed its ROUTE_END, so no selection ever loops.
#[test]
fn sweep_forgetting_distances() {
	const RELAYS: usize = 5;
	let mut rng = XorShift(0x5eed_f0d5);
	let mut collected = 0;
	for _ in 0..200 {
		let nodes = RELAYS + 2;
		let mut sim = Sim::new(nodes);
		let mut links = Vec::new();
		for a in 0..RELAYS {
			for b in a + 1..RELAYS {
				if b == a + 1 || rng.chance(40) {
					sim.join(a, b, 1 + rng.next(3));
					links.push((a, b));
				}
			}
		}
		let ids = [RELAYS as u64, RELAYS as u64 + 1];
		for origin in RELAYS..nodes {
			let relay = rng.next(RELAYS as u64) as usize;
			sim.join(origin, relay, 1);
			links.push((relay, origin));
			sim.originate(origin, origin as u64, 0);
		}
		assert!(sim.run());

		for _ in 0..60 {
			match rng.next(8) {
				0 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					if sim.connected(a, b) {
						sim.disconnect(a, b);
					} else {
						sim.connect(a, b);
					}
				}
				1 | 2 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					if rng.chance(60) {
						sim.hold(from, to);
					} else {
						sim.release(from, to);
					}
				}
				3 => {
					let busy = sim.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						sim.deliver(from, to);
					}
				}
				4 => sim.bump(RELAYS + rng.next(2) as usize),
				_ => {
					let node = rng.next(RELAYS as u64) as usize;
					let id = ids[rng.next(2) as usize];
					if !sim.holds(node, id) {
						sim.collect(node, id);
						collected += 1;
					}
				}
			}
			sim.settle(1);
			for &id in &ids {
				assert_loop_free(&sim, nodes, id);
			}
		}
	}
	assert!(collected > 0);
}

/// Babel's feasibility needs every link to increase the metric, so a
/// zero-cost link is refused when it is configured.
#[test]
fn zero_cost_links_are_refused() {
	let mut router: Router<Node, Item> = Router::new(1);
	let link = Link {
		cost: 0,
		..Link::default()
	};
	assert_eq!(router.connect(1, link), Err(Error::ZeroCost));
}

/// A total reaching the infinite metric is unreachable, never saturated, and
/// not waiting on a seqno.
#[test]
fn overflow_is_unreachable() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, a, MAX - 1);
	sim.join(a, b, 5);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(sim.route(a, 7).map(|route| route.metric), Some(MAX - 1));
	assert!(sim.route(b, 7).is_none());
	assert!(!sim.starved(b, 7), "unreachable, not waiting on a seqno");
}

/// Redundant cores are feasible successors: losing one fails over at once,
/// with no seqno request.
#[test]
fn redundant_cores_fail_over_without_a_seqno_request() {
	let (ingest, core_a, core_b, edge) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(ingest, core_a, 1);
	sim.join(ingest, core_b, 1);
	sim.join(core_a, core_b, 1);
	sim.link(edge, core_a, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(edge, core_b, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.originate(ingest, 7, 0);
	assert!(sim.run());
	let primary = next(&sim, edge, 7).unwrap();

	let requests = sim.stats.requests;
	sim.disconnect(edge, primary);
	assert!(
		sim.route(edge, 7).is_some(),
		"the other core advertises no more than the lost one"
	);
	assert!(sim.run());
	assert_eq!(sim.stats.requests, requests);
}

/// A publisher reached directly over P2P and through the CDN. A browser
/// advertises nothing it learned, so losing the P2P session falls back at
/// once. A routing node advertised the one-hop route, so the CDN detour is
/// infeasible until its seqno request reaches the publisher.
#[test]
fn p2p_fallback_is_immediate_for_leaves_and_needs_a_seqno_for_transit() {
	for transit in [false, true] {
		let (publisher, edge, subscriber) = (0, 1, 2);
		let mut sim = Sim::new(3);
		sim.leaf(publisher);
		if !transit {
			sim.leaf(subscriber);
		}
		sim.join(publisher, edge, 1);
		sim.link(subscriber, edge, SimLink::cost(1).upstream(), SimLink::cost(1));
		sim.link(subscriber, publisher, SimLink::cost(1).priority(1), SimLink::cost(1));
		sim.originate(publisher, 9, 0);
		assert!(sim.run());
		assert_eq!(next(&sim, subscriber, 9), Some(publisher));

		sim.disconnect(subscriber, publisher);
		assert_eq!(sim.starved(subscriber, 9), transit);
		assert!(sim.run());
		assert_eq!(next(&sim, subscriber, 9), Some(edge));
		assert_eq!(sim.seqno(publisher), u64::from(transit));
	}
}

/// A relay whose only peers are upstream sends no update for its down-only
/// route, so it holds no feasibility distance and takes a longer route at the
/// same seqno once its best uplink goes.
#[test]
fn feasibility_tracks_only_updates_actually_sent() {
	let (relay, c1, c2, origin) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.link(relay, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(relay, c2, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.leaf(origin);
	sim.join(origin, c1, 1);
	sim.join(origin, c2, 5);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, relay, 7), Some(c1));
	assert!(!sim.remembers(relay, 7));

	sim.disconnect(relay, c1);
	assert_eq!(next(&sim, relay, 7), Some(c2));
	assert!(sim.run());
	assert_eq!(sim.seqno(origin), 0);
}

/// A GOAWAY ranks the peer below every eligible peer, priority included, and
/// the edge fails over at once to the other core. The draining core stays a
/// last resort while its session is up.
#[test]
fn goaway_fails_over_and_keeps_the_draining_peer_last() {
	let (ingest, core_a, core_b, edge) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(ingest, core_a, 1);
	sim.join(ingest, core_b, 1);
	sim.link(edge, core_a, SimLink::cost(1).upstream().priority(1), SimLink::cost(1));
	sim.link(edge, core_b, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.originate(ingest, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, edge, 7), Some(core_a));

	sim.goaway(core_a);
	assert!(sim.run());
	assert_eq!(next(&sim, edge, 7), Some(core_b));

	sim.disconnect(edge, core_b);
	assert!(sim.run());
	assert_eq!(
		next(&sim, edge, 7),
		Some(core_a),
		"the draining core is the last resort"
	);
}

/// An edge serving a client through two uplinks, where the second uplink's
/// route is longer than the one the edge advertised. A GOAWAY keeps the
/// draining uplink serving while the alternative is infeasible. The refresh
/// reaches the edge through both uplinks: if the alternative's arrives first
/// the edge moves; if the draining uplink's does, the edge keeps it until its
/// session closes and then waits for one more refresh, which its request asks
/// for.
#[test]
fn goaway_keeps_serving_until_a_refresh_makes_the_alternative_feasible() {
	let (origin, a, b, edge, client) = (0, 1, 2, 3, 4);
	let build = || {
		let mut sim = Sim::new(5);
		sim.leaf(origin);
		sim.leaf(client);
		sim.join(origin, a, 1);
		sim.join(origin, b, 5);
		sim.link(edge, a, SimLink::cost(1).upstream(), SimLink::cost(1));
		sim.link(edge, b, SimLink::cost(1).upstream(), SimLink::cost(1));
		sim.join(edge, client, 1);
		sim.originate(origin, 7, 0);
		assert!(sim.run());
		sim
	};

	for draining_first in [false, true] {
		let mut sim = build();
		sim.goaway(a);
		assert!(sim.run());
		assert_eq!(
			next(&sim, edge, 7),
			Some(a),
			"b advertised 5, above the edge's distance of 2"
		);
		assert!(sim.route(client, 7).is_some());

		let late = if draining_first { b } else { a };
		sim.hold(late, edge);
		sim.bump(origin);
		assert!(sim.run());
		sim.release(late, edge);
		assert!(sim.run());
		assert!(sim.route(client, 7).is_some());
		if draining_first {
			assert_eq!(
				next(&sim, edge, 7),
				Some(a),
				"the refresh through a lowered the distance first"
			);
			sim.disconnect(edge, a);
			assert!(sim.starved(edge, 7), "only another refresh moves the edge to b");
			assert!(sim.run());
			assert_eq!(sim.seqno(origin), 2, "the edge's request asked for it");
		}
		assert_eq!(next(&sim, edge, 7), Some(b));
	}
}

/// Two CDN relays, each the uplink of one relay in a local mesh, reaching a
/// CDN origin. The CDN admits each local relay under `grant`.
fn two_uplinks(l2_marks_upstream: bool, grant: Option<Grant>) -> Sim {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	let admit = SimLink {
		grant,
		..SimLink::cost(1)
	};
	let up = if l2_marks_upstream {
		SimLink::cost(1).upstream()
	} else {
		SimLink::cost(1)
	};
	sim.link(l1, c1, SimLink::cost(1).upstream(), admit);
	sim.link(l2, c2, up, admit);
	sim.leaf(origin);
	sim.join(origin, c1, 1);
	sim.originate(origin, 7, 0);
	sim
}

/// The down-only bit keeps the CDN on its own link while the local relays
/// still reach the CDN origin.
#[test]
fn two_uplinks_never_carry_cdn_transit() {
	let (c1, c2, l2) = (0, 1, 3);
	let mut sim = two_uplinks(true, None);
	assert!(sim.run());
	assert_eq!(next(&sim, c2, 7), Some(c1));
	assert!(sim.route(l2, 7).is_some());
	assert!(sim.route(l2, 7).unwrap().down_only);
}

/// A local relay whose preferred route is down-only exports nothing, so the
/// CDN hauls the origin across its expensive internal link while a LAN path
/// sits unadvertised. Ranking local peers above a backup uplink avoids it.
/// Babel selects only feasible routes, so a transit relay's priority for a
/// longer route holds only if that route arrived before the shorter one was
/// advertised.
#[test]
fn preferred_down_only_route_hides_the_exportable_alternative() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let build = |lan_first: bool, prefer_uplink: bool| {
		let mut sim = Sim::new(5);
		sim.join(c1, c2, 10);
		sim.leaf(origin);
		sim.join(origin, l1, 1);
		sim.originate(origin, 7, 0);
		let (uplink, lan) = if prefer_uplink { (1, 0) } else { (0, 1) };
		let join_lan = |sim: &mut Sim| sim.link(l2, l1, SimLink::cost(1).priority(lan), SimLink::cost(1));
		if lan_first {
			join_lan(&mut sim);
		}
		sim.link(l1, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
		sim.link(l2, c2, SimLink::cost(1).upstream().priority(uplink), SimLink::cost(1));
		assert!(sim.run());
		if !lan_first {
			join_lan(&mut sim);
		}
		assert!(sim.run());
		sim
	};

	let sim = build(false, true);
	assert_eq!(next(&sim, l2, 7), Some(c2));
	assert!(sim.export_route(l2, 7).is_none());
	assert_eq!(next(&sim, c2, 7), Some(c1));
	assert_eq!(sim.route(c2, 7).map(|route| route.metric), Some(12));

	let sim = build(false, false);
	assert_eq!(next(&sim, l2, 7), Some(l1));
	assert_eq!(next(&sim, c2, 7), Some(l2));

	// Arrival order decides the transit priority's outcome.
	assert_eq!(next(&build(true, true), l2, 7), Some(l1));
}

/// A scoped peer forwards a victim's identity with a newer seqno. Unscoped,
/// the forgery captures the victim's route. Filed under a qualified key, it is
/// a separate identity, and the victim's route is untouched. A scoped peer's
/// own identity is qualified too, since nothing proves it.
#[test]
fn forged_identity_is_filed_apart_under_a_scoped_grant() {
	for scoped in [false, true] {
		let (victim, core, edge, forger) = (0, 1, 2, 3);
		let mut sim = Sim::new(4);
		sim.leaf(victim);
		sim.leaf(forger);
		sim.join(victim, core, 5);
		sim.join(core, edge, 1);
		let grant = Grant {
			subject: "forger",
			scope: "forger/",
		};
		let admission = if scoped {
			SimLink::cost(1).grant(grant)
		} else {
			SimLink::cost(1)
		};
		sim.link(edge, forger, admission, SimLink::cost(1));
		sim.originate(victim, 7, 0);
		sim.originate(forger, 3, 0);
		assert!(sim.run());

		sim.forge(forger, 7, 1000);
		assert!(sim.run());
		let own = if scoped { qualified("forger", 3) } else { 3 };
		assert!(sim.route(core, own).is_some());
		assert_eq!(
			sim.route(core, 3).is_some(),
			!scoped,
			"an unproven own identity is qualified"
		);
		if scoped {
			assert_eq!(next(&sim, core, 7), Some(victim));
			assert_eq!(next(&sim, core, qualified("forger", 7)), Some(edge));
		} else {
			assert_eq!(
				next(&sim, core, 7),
				Some(edge),
				"the forger captures the victim's route"
			);
		}

		sim.kill(forger);
		assert!(sim.run());
		assert_eq!(next(&sim, core, 7), Some(victim));
	}
}

/// A local mesh publishing through two uplinks, each admitted under the same
/// subject. Both CDN relays file the local origin under one key, so it
/// survives losing an uplink; a retracted identity leaves no alias behind.
#[test]
fn scoped_delivery_through_two_uplinks_reaches_one_key() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	let admit = SimLink::cost(1).grant(LOCAL);
	sim.link(l1, c1, SimLink::cost(1).upstream(), admit);
	sim.link(l2, c2, SimLink::cost(1).upstream(), admit);
	sim.leaf(origin);
	sim.join(origin, l1, 1);
	sim.originate(origin, 9, 0);
	sim.originate(l1, 20, 0);
	assert!(sim.run());

	let key = qualified(LOCAL.subject, 9);
	assert!(sim.route(c1, 9).is_none());
	assert_eq!(next(&sim, c1, key), Some(l1));
	assert_eq!(next(&sim, c2, key), Some(l2));
	assert_eq!(next(&sim, c1, qualified(LOCAL.subject, 20)), Some(l1));

	sim.disconnect(l2, c2);
	assert!(sim.run());
	assert_eq!(next(&sim, c2, key), Some(c1));

	assert_eq!(sim.aliases(c1), 2, "the origin's and the local relay's own");
	sim.kill(origin);
	assert!(sim.run());
	assert_eq!(sim.aliases(c1), 1);
}

/// A local relay missing its `upstream` mark leaks CDN routes back to the
/// CDN. Admitted under a scoped grant, the leaked identity is filed apart, so
/// the CDN keeps its own route.
#[test]
fn misconfigured_uplink_is_contained_by_qualified_identities() {
	let (c1, c2, l2) = (0, 1, 3);
	let mut sim = two_uplinks(false, None);
	assert!(sim.run());
	assert_eq!(next(&sim, c2, 7), Some(l2));

	let mut sim = two_uplinks(false, Some(LOCAL));
	assert!(sim.run());
	assert_eq!(next(&sim, c2, 7), Some(c1));
	assert_eq!(
		next(&sim, c2, qualified(LOCAL.subject, 7)),
		Some(l2),
		"the leak is filed apart"
	);
}

/// A qualified key the CDN sent down comes back up through the misconfigured
/// uplink. A scoped connection forwarding a qualified key has that route
/// refused, so the leak is filed once and never again.
#[test]
fn scoped_connections_cannot_requalify_a_qualified_identity() {
	let c2 = 1;
	let mut sim = two_uplinks(true, Some(LOCAL));
	assert!(sim.run());
	let marked = sim.identities(c2);

	let mut sim = two_uplinks(false, Some(LOCAL));
	assert!(sim.run());
	assert_eq!(sim.identities(c2), marked + 1, "the leak is filed once, never again");
}

/// A partitioned local mesh reaches its other half through the CDN once a
/// seqno request reaches the origin, and heals back onto the LAN.
#[test]
fn partitioned_local_mesh_reaches_itself_through_the_cdn() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	sim.link(l1, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(l2, c2, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.leaf(origin);
	sim.join(origin, l2, 1);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, l1, 7), Some(l2));

	sim.disconnect(l1, l2);
	assert!(sim.starved(l1, 7), "the CDN detour is longer than the LAN route");
	assert!(sim.run());
	assert_eq!(sim.seqno(origin), 1);
	assert_eq!(next(&sim, l1, 7), Some(c1));

	sim.connect(l1, l2);
	assert!(sim.run());
	assert_eq!(next(&sim, l1, 7), Some(l2), "heals");
}

/// A five-node ring loses its preferred origin link while a longer backup
/// remains. The backup's route is infeasible, so the starved node's request,
/// with duplicates suppressed at every hop, reaches the origin while the
/// backup never loses its session.
#[test]
fn a_ring_recovers_its_longer_backup_through_one_request() {
	let origin = 5;
	let mut sim = Sim::new(6);
	for relay in 0..5 {
		sim.join(relay, (relay + 1) % 5, 1);
	}
	sim.leaf(origin);
	sim.join(origin, 0, 1);
	sim.join(origin, 2, 10);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, 2, 7), Some(1));

	let requests = sim.stats.requests;
	sim.disconnect(origin, 0);
	assert!(sim.run());
	for relay in 0..5 {
		assert!(sim.route(relay, 7).is_some(), "relay {relay}");
	}
	assert_eq!(sim.seqno(origin), 1);
	assert!(sim.stats.requests > requests);
	assert_eq!(sim.stats.repeated_requests, 0, "duplicates suppressed");
	assert_eq!(sim.retained().pending, 0);
}

/// A request for an origin no usable route reaches stops once no usable
/// route is left, leaving nothing pending.
#[test]
fn an_unanswerable_request_stops() {
	let origin = 4;
	let mut sim = Sim::new(5);
	for relay in 0..4 {
		sim.join(relay, (relay + 1) % 4, 1);
	}
	sim.leaf(origin);
	sim.join(origin, 0, 1);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	sim.disconnect(origin, 0);
	assert!(sim.run());
	for relay in 0..4 {
		assert!(sim.route(relay, 7).is_none());
		assert!(!sim.holds(relay, 7));
	}
	assert_eq!(sim.retained().pending, 0);
}

/// Each message recomputes only the identity it names, so the work a message
/// costs does not grow with how many other origins a node routes to.
#[test]
fn a_message_recomputes_only_its_identity() {
	for origins in [1, 50] {
		let (a, b, client) = (0, 1, 2);
		let mut sim = Sim::new(3 + origins);
		sim.join(a, b, 1);
		sim.leaf(client);
		sim.join(client, b, 1);
		for origin in 3..3 + origins {
			sim.leaf(origin);
			sim.join(origin, a, 1);
			sim.originate(origin, origin as Id, 0);
			sim.announce(origin, &format!("live/{origin}"), 0, None);
		}
		assert!(sim.run());
		assert_eq!(sim.shown(client).len(), origins);

		let (touched, delivered) = (sim.touched(), sim.stats.delivered);
		sim.announce(3, "live/extra", 0, None);
		assert!(sim.run());
		assert_eq!(
			sim.touched() - touched,
			sim.stats.delivered - delivered + 1,
			"one identity per message, plus the origin's own change"
		);
	}
}

//! Route selection: identity, feasibility, seqno requests, GOAWAY, the
//! down-only bit, and qualified identities.

use super::*;

const LOCAL: Grant = Grant {
	subject: "local",
	scope: "local/",
};

/// Originate `id` at `node` with a root claim, which meets every interest: a
/// route goes to a stream only while a claim meets the stream's interest.
fn originate(sim: &mut Sim, node: Node, id: Id, seqno: Seqno) {
	sim.originate(node, id, seqno);
	sim.announce(node, "", 0, None);
}

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
	originate(&mut sim, origin, 7, 0);
	for _ in 0..3 {
		sim.bump(origin);
	}
	assert!(sim.run());
	assert!(sim.route(b, 7).is_some());

	sim.kill(origin);
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 8, 0);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
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
			originate(&mut sim, origin, origin as u64, 0);
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
	let mut router: Router<Node, Item> = Router::new(99, 1);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, ingest, 7, 0);
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
		originate(&mut sim, publisher, 9, 0);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, ingest, 7, 0);
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
		originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
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
		originate(&mut sim, origin, 7, 0);
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
		originate(&mut sim, victim, 7, 0);
		originate(&mut sim, forger, 3, 0);
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
/// survives losing an uplink, and so does the local relay's own identity; a
/// retracted identity leaves no alias behind.
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
	originate(&mut sim, origin, 9, 0);
	originate(&mut sim, l1, 20, 0);
	assert!(sim.run());

	let key = qualified(LOCAL.subject, 9);
	assert!(sim.route(c1, 9).is_none());
	assert_eq!(next(&sim, c1, key), Some(l1));
	assert_eq!(next(&sim, c2, key), Some(l2));
	assert_eq!(next(&sim, c1, qualified(LOCAL.subject, 20)), Some(l1));

	sim.disconnect(l1, c1);
	assert!(sim.run());
	assert_eq!(
		next(&sim, c1, qualified(LOCAL.subject, 20)),
		Some(c2),
		"the local relay's own fails over"
	);
	sim.connect(l1, c1);
	assert!(sim.run());

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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
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
	originate(&mut sim, origin, 7, 0);
	assert!(sim.run());
	sim.disconnect(origin, 0);
	assert!(sim.run());
	for relay in 0..4 {
		assert!(sim.route(relay, 7).is_none());
		assert!(!sim.holds(relay, 7));
	}
	assert_eq!(sim.retained().pending, 0);
}

/// A request newer than the one pending raises it and is sent on again, so the
/// newer requester is not left waiting on an answer that only meets the older
/// one; a repeat of the pending Seqno, or an older one, is a duplicate.
#[test]
fn a_newer_request_raises_the_pending_one() {
	let (up, a, b) = (0, 1, 2);
	let mut relay = Router::<Node, Item>::new(99, 9);
	for neighbor in [up, a, b] {
		relay.connect(neighbor, Link::default()).unwrap();
	}
	let advert = Advert {
		seqno: 0,
		metric: 1,
		down_only: false,
		rtt: None,
		hops: [].into(),
	};
	relay.route(up, 7, Some(Reach::Finite(advert)));
	relay.drain();

	let requests = |relay: &mut Router<Node, Item>| -> Vec<(Node, Seqno)> {
		relay
			.drain()
			.into_iter()
			.filter_map(|out| match out {
				Out::Request { to, id: 7, seqno } => Some((to, seqno)),
				_ => None,
			})
			.collect()
	};

	relay.request(a, 7, 1);
	assert_eq!(requests(&mut relay), [(up, 1)]);
	relay.request(b, 7, 1);
	assert_eq!(requests(&mut relay), [], "a duplicate");
	relay.request(b, 7, 2);
	assert_eq!(requests(&mut relay), [(up, 2)], "raised and sent again");
	relay.request(a, 7, 1);
	assert_eq!(requests(&mut relay), [], "older than the pending one");
}

/// A route starved at the last seqno asks for nothing newer, since its origin rotates
/// to a new identity rather than advance past it.
#[test]
fn a_route_starved_at_the_last_seqno_requests_nothing() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, a, 1);
	sim.join(a, b, 1);
	originate(&mut sim, origin, 7, MAX);
	assert!(sim.run());
	assert!(sim.route(b, 7).is_some());

	sim.kill(origin);
	originate(&mut sim, origin, 7, 0);
	sim.reconnect(origin);
	let requests = sim.stats.requests;
	assert!(sim.run());
	assert!(sim.starved(a, 7), "seqno 0 is older than the last one a remembers");
	assert_eq!(sim.stats.requests, requests, "nothing follows the last seqno");
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

/// A route whose metric plus the link cost reaches the infinite metric is
/// unreachable, so it neither certifies regions nor keeps an identity held:
/// only the reachable neighbor's region shows, and a held overflowing route
/// is let go.
#[test]
fn an_overflowing_route_neither_certifies_nor_holds() {
	let finite = |metric| {
		Some(Reach::Finite(Advert {
			seqno: 0,
			metric,
			down_only: false,
			rtt: None,
			hops: [].into(),
		}))
	};
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let (near, far) = (1, 2);
	let far_link = Link {
		cost: MAX,
		..Link::default()
	};

	let mut router: Router<Node, Item> = Router::new(99, 1);
	router.connect(near, Link::default()).unwrap();
	router.connect(far, far_link.clone()).unwrap();
	router.asks(near, p("live/a")).unwrap();
	router.route(near, 7, finite(1));
	router.batch(near, 7, vec![(p("live/a/x"), Some(item))], Some(1), 0);
	router.route(far, 7, finite(1));
	router.batch(far, 7, vec![(p("live/b/y"), Some(item))], Some(1), 0);
	assert_eq!(router.selected(7).and_then(|route| route.next), Some(near));
	assert_eq!(router.shows(7, &p("live/a/x")), Some(true));
	assert_eq!(
		router.shows(7, &p("live/b/y")),
		None,
		"only the unreachable route certifies it"
	);

	let mut router: Router<Node, Item> = Router::new(99, 1);
	router.connect(far, far_link).unwrap();
	router.route(far, 7, finite(1));
	router.batch(far, 7, vec![(p("live/x"), Some(item))], Some(1), 0);
	router.route(far, 7, Some(Reach::Held));
	assert!(router.selected(7).is_none());
	assert!(!router.grounded(7), "an unreachable held route recovers nothing");
	assert_eq!(router.shows(7, &p("live/x")), None);
}

/// A route grows to the hop limit with nothing else about it changed, so its sender
/// stops certifying. What only that neighbor certified is let go at once, though a
/// change of hops alone otherwise lets go of nothing.
#[test]
fn a_route_grown_to_the_hop_limit_certifies_nothing() {
	let finite = |hops: usize| {
		Some(Reach::Finite(Advert {
			seqno: 0,
			metric: 1,
			down_only: false,
			rtt: None,
			hops: (1..=hops as Id).collect(),
		}))
	};
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let (narrow, wide) = (1, 2);
	let mut router: Router<Node, Item> = Router::new(99, 1);
	router.connect(narrow, Link::default()).unwrap();
	router.connect(wide, Link::default()).unwrap();
	router.asks(narrow, p("live/a")).unwrap();
	router.route(narrow, 7, finite(2));
	router.batch(narrow, 7, vec![(p("live/a/x"), Some(item))], Some(1), 0);
	router.route(wide, 7, finite(MAX_HOPS - 2));
	router.batch(wide, 7, vec![(p("live/b/y"), Some(item))], Some(1), 0);
	assert_eq!(router.shows(7, &p("live/b/y")), Some(true));

	router.route(wide, 7, finite(MAX_HOPS - 1));
	assert_eq!(
		router.shows(7, &p("live/b/y")),
		Some(true),
		"one hop more moves nothing"
	);
	router.route(wide, 7, finite(MAX_HOPS));
	assert_eq!(
		router.shows(7, &p("live/b/y")),
		None,
		"the route crossed as many nodes as one may"
	);
	assert_eq!(router.shows(7, &p("live/a/x")), Some(true));
}

/// A fresh claim arrives only on a stream that certifies nothing, while an
/// older floor on another stream proves it absent. The merge rule trusts the
/// floor, so the claim stays hidden until the certifying neighbor refreshes
/// past it or lets go: that is the bound on how late an uncertified claim
/// shows.
#[test]
fn a_fresh_uncertified_claim_waits_on_an_older_floor() {
	let finite = |metric| {
		Some(Reach::Finite(Advert {
			seqno: 0,
			metric,
			down_only: false,
			rtt: None,
			hops: [].into(),
		}))
	};
	let item = Item {
		seed: 0,
		epoch: None,
		since: 2,
	};
	let (certifier, carrier) = (1, 2);
	for refresh in [true, false] {
		let mut router: Router<Node, Item> = Router::new(99, 1);
		router.connect(certifier, Link::default()).unwrap();
		router.connect(carrier, Link::default()).unwrap();
		router.route(certifier, 7, finite(1));
		router.batch(certifier, 7, Vec::new(), Some(1), 0);
		router.route(carrier, 7, finite(1));
		router.batch(carrier, 7, vec![(p("live/x"), Some(item))], None, 0);
		assert_eq!(router.shows(7, &p("live/x")), None, "the older floor hides it");

		match refresh {
			true => router.batch(certifier, 7, vec![(p("live/x"), Some(item))], Some(2), 0),
			false => router.disconnect([certifier]),
		}
		assert_eq!(router.shows(7, &p("live/x")), Some(true), "refresh {refresh}");
	}
}

/// Closing a session's streams together leaves the node as closing them one at a
/// time does: each stream asks its own prefix, and some share one, so a region one
/// closes stays certified by another while the rest let go. Once none is left, the
/// reader sees the same; the view stays, as a node with no route keeps it until a
/// route returns or it collects the identity.
#[test]
fn closing_streams_together_matches_closing_them_in_turn() {
	let finite = Some(Reach::Finite(Advert {
		seqno: 0,
		metric: 1,
		down_only: false,
		rtt: None,
		hops: [].into(),
	}));
	let item = |seed| Item {
		seed,
		epoch: None,
		since: 1,
	};
	let opened = || {
		let mut router: Router<Node, Item> = Router::new(99, 1);
		for stream in 0..6 {
			let prefix = format!("live/{}", stream % 4);
			router.connect(stream, Link::default()).unwrap();
			router.asks(stream, Path::new(&prefix).unwrap()).unwrap();
			router.route(stream, 7, finite.clone());
			let claims = (0..3)
				.map(|j| (Path::new(&format!("{prefix}/{j}")).unwrap(), Some(item(j))))
				.collect();
			router.batch(stream, 7, claims, Some(1), 0);
		}
		router
	};
	for closed in [vec![0, 1, 4], vec![0, 1, 2, 3, 4, 5]] {
		let mut together = opened();
		together.disconnect(closed.iter().copied());
		let mut in_turn = opened();
		for &stream in &closed {
			in_turn.disconnect([stream]);
		}
		assert_eq!(together.shown(), in_turn.shown(), "closed {closed:?}");
		if closed.len() < 6 {
			assert_eq!(together.view(7), in_turn.view(7), "closed {closed:?}");
			assert_eq!(together.retained(), in_turn.retained(), "closed {closed:?}");
		} else {
			assert!(together.shown().is_empty());
		}
	}
}

/// An echo travels apart from the echoing node's routes, so it can overtake a
/// route that node derived from this one's. b learned its route from a and sent it
/// back, then ended it on a's ROUTE_END. Its echo arrives first, so a drops its
/// feasibility distance, and b's old route arrives looking like a fresh, longer one
/// to an origin neither reaches. Its hops name a, so a refuses it, where two relays
/// once counted up without end.
#[test]
fn a_route_that_crossed_a_node_is_refused_once_its_distance_is_gone() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, a, 1);
	sim.join(origin, b, 1);
	sim.join(a, b, 1);
	// b hears the origin only through a, and b's route to a waits.
	sim.hold(origin, b);
	sim.hold(b, a);
	originate(&mut sim, origin, 7, 0);
	assert!(sim.run());
	assert_eq!(next(&sim, b, 7), Some(a));

	sim.disconnect(origin, a);
	assert!(sim.run());
	assert!(!sim.holds(b, 7), "b let go on a's ROUTE_END");
	assert!(sim.deliver_ack(b, a), "b's echo overtakes its own route");
	assert!(!sim.remembers(a, 7), "a dropped its distance");
	assert!(sim.deliver(b, a));
	assert!(sim.holds(a, 7), "b's route stays open until b ends it");
	assert!(sim.route(a, 7).is_none(), "a refuses its own route, come back");
	assert!(!sim.starved(a, 7), "no seqno would help");
	assert_eq!(sim.shows(a, 7, ""), None);

	// b still reaches the origin, and a takes the route b learns there.
	sim.release(b, a);
	sim.release(origin, b);
	assert!(sim.run());
	assert_eq!(
		sim.route(a, 7).map(|route| (route.next, route.metric)),
		Some((Some(b), 2))
	);
}

/// The origin sits behind relay a, in a triangle with b and c. An echo proves only
/// that nothing derived is in flight on its own link: here c's copy of a's route
/// waits on its way to b while both echo a's ROUTE_END, so a drops its distance, and
/// then b takes c's copy and offers it to a. It crossed a, so a refuses it and the
/// copies die out, where the three relays once handed it round at a growing metric.
#[test]
fn a_route_handed_round_a_triangle_is_refused_where_it_began() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, a, 1);
	sim.join(a, b, 2);
	sim.join(a, c, 1);
	sim.join(b, c, 2);
	for (from, to) in [(c, b), (b, c), (b, a), (c, a)] {
		sim.hold(from, to);
	}
	originate(&mut sim, origin, 7, 0);
	assert!(sim.run());
	assert_eq!(sim.route(b, 7).map(|route| route.metric), Some(3));

	sim.disconnect(origin, a);
	assert!(sim.run());
	assert!(!sim.holds(b, 7) && !sim.holds(c, 7), "both let go on a's ROUTE_END");
	assert!(sim.deliver_ack(b, a) && sim.deliver_ack(c, a));
	assert!(!sim.remembers(a, 7), "every neighbor echoed, so a dropped its distance");

	// c's copy, sent before the end, is shorter than what b advertised: feasible there.
	assert!(sim.deliver(c, b));
	assert_eq!(
		sim.route(b, 7).map(|route| (route.next, route.metric)),
		Some((Some(c), 4))
	);

	// b may offer it to a once a echoes the ROUTE_END b sent it.
	let mut peak = 0;
	for _ in 0..100 {
		for (from, to) in [(b, a), (c, a), (a, b), (a, c)] {
			sim.deliver(from, to);
			sim.deliver_ack(from, to);
			peak = peak.max(sim.route(a, 7).map_or(0, |route| route.metric));
		}
	}
	assert_eq!(peak, 0, "a never takes back a route that crossed it");

	for (from, to) in [(c, b), (b, c), (b, a), (c, a)] {
		sim.release(from, to);
	}
	assert!(sim.run());
	for node in [a, b, c] {
		assert!(!sim.holds(node, 7), "node {node}");
		assert!(!sim.remembers(node, 7), "node {node}");
	}
}

/// An origin names itself in its route's hops like any relay, so once it rotated
/// to a fresh identity and forgot the old one, a copy of the old route is still
/// refused where it began.
#[test]
fn a_retired_identitys_route_is_refused_by_the_node_that_originated_it() {
	let (origin, b) = (0, 1);
	let mut sim = Sim::new(2);
	sim.join(origin, b, 1);
	sim.hold(b, origin);
	originate(&mut sim, origin, 7, MAX - 1);
	assert!(sim.run());
	assert_eq!(next(&sim, b, 7), Some(origin));

	sim.bump(origin);
	assert_ne!(sim.proven(origin), 7, "the last seqno rotates the identity");
	assert!(sim.run());
	assert!(sim.deliver_ack(b, origin), "b's echo overtakes its own route");
	assert!(!sim.remembers(origin, 7));
	assert!(sim.deliver(b, origin));
	assert!(sim.holds(origin, 7), "b's copy of the old route arrived");
	assert!(sim.route(origin, 7).is_none(), "it crossed the node that began it");

	sim.release(b, origin);
	assert!(sim.run());
	assert!(!sim.holds(origin, 7) && !sim.remembers(origin, 7));
}

/// A scoped peer hands a relay's own route back under the key the relay files that
/// peer's identities by. The hops are never qualified, so the relay still finds
/// itself in them and refuses the copy.
#[test]
fn a_route_that_returns_through_a_scoped_peer_is_refused() {
	let (origin, relay, peer) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, relay, 1);
	sim.link(relay, peer, SimLink::cost(1).grant(LOCAL), SimLink::cost(1));
	sim.originate(origin, 7, 0);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(next(&sim, peer, 7), Some(relay));
	assert!(sim.holds(relay, key), "the peer sent the route back");
	assert!(sim.route(relay, key).is_none(), "it crossed the relay");
	assert_eq!(sim.shows(relay, key, "local/x"), None);

	sim.disconnect(origin, relay);
	assert!(sim.run());
	assert!(!sim.holds(relay, key) && !sim.holds(peer, 7));
}

/// A route that already crossed as many nodes as one may goes no further: the next
/// relay cannot add itself, so it is unreachable there, not waiting on a seqno.
#[test]
fn a_route_stops_at_the_hop_limit() {
	let last = MAX_HOPS + 1;
	let mut sim = Sim::new(last + 1);
	for node in 0..last {
		sim.join(node, node + 1, 1);
	}
	originate(&mut sim, 0, 7, 0);
	assert!(sim.run());
	let full = last - 2;
	assert_eq!(sim.route(full, 7).map(|route| route.metric), Some(full as Metric));
	assert!(
		sim.route(full + 1, 7).is_none(),
		"the route crossed as many nodes as one may"
	);
	assert!(sim.holds(full + 1, 7) && !sim.starved(full + 1, 7));
	assert!(!sim.holds(last, 7), "nothing is sent on");
}

/// A relay sends the origin's route to a peer that files it under a qualified key
/// and hands it back. While the peer got there by another relay the copy is a route
/// like any other; once the peer takes this relay's own, the copy has crossed it. No
/// seqno changes that, so the relay lets the key go without asking for one.
#[test]
fn a_scoped_copy_that_crossed_a_relay_asks_for_no_seqno() {
	let (origin, near, far, cdn) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, near, 1);
	sim.join(origin, far, 1);
	sim.link(near, cdn, SimLink::cost(1), SimLink::cost(1).grant(LOCAL));
	sim.link(far, cdn, SimLink::cost(1), SimLink::cost(2).grant(LOCAL));
	// The peer hears the far relay first.
	sim.hold(near, cdn);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(next(&sim, cdn, key), Some(far));
	assert_eq!(next(&sim, near, key), Some(cdn));
	assert_eq!(
		sim.shows(near, key, "local/x"),
		Some(true),
		"the far relay's copy is a route"
	);

	sim.release(near, cdn);
	assert!(sim.run());
	assert_eq!(next(&sim, cdn, key), Some(near), "the peer took the shorter route");
	assert!(sim.route(near, key).is_none(), "which crossed the near relay");
	assert!(!sim.starved(near, key));
	assert_eq!(sim.seqno(origin), 0, "nothing failed, so nothing was asked");
	assert_eq!(sim.retained().pending, 0);
	assert_eq!(sim.shows(near, 7, "local/x"), Some(true));
}

/// The same copy, arriving at a metric the relay's distance refuses: the peer had
/// the origin through the far relay, and falls back on the near one's route when
/// that goes. The copy is infeasible, but it extends nothing the near relay sent
/// under the key, so the peer will never move off it: the relay does not wait.
#[test]
fn a_scoped_copy_that_crossed_a_relay_is_not_waited_on() {
	let (origin, near, far, cdn) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, near, 5);
	sim.join(origin, far, 1);
	// The near relay is nearer the origin than the peer, so its route is feasible there.
	sim.link(near, cdn, SimLink::cost(1), SimLink::cost(3).grant(LOCAL));
	sim.link(far, cdn, SimLink::cost(1), SimLink::cost(5).grant(LOCAL));
	sim.originate(origin, 7, 0);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(next(&sim, cdn, key), Some(far));
	assert_eq!(sim.route(near, key).map(|route| route.metric), Some(7));
	assert_eq!(sim.shows(near, key, "local/x"), Some(true));

	sim.disconnect(far, cdn);
	assert!(sim.run());
	assert_eq!(
		sim.route(cdn, key).map(|route| (route.next, route.metric)),
		Some((Some(near), 8))
	);
	assert!(sim.holds(near, key) && sim.route(near, key).is_none());
	assert!(
		!sim.starved(near, key),
		"the peer's route stands whatever the relay does"
	);
	assert_eq!(sim.seqno(origin), 0);
	assert_eq!(sim.retained().pending, 0);
	assert_eq!(sim.shows(near, 7, "local/x"), Some(true));
}

/// A relay files a scoped origin under a qualified key and sends that key on to a
/// peer, which routes it through the relay and sends it back. The copy extends what
/// the relay sent under the key, so once the relay's own route goes, it waits for
/// the peer to hear and move off, as for any identity.
#[test]
fn a_scoped_key_sent_back_through_its_relay_is_waited_on() {
	let (origin, relay, peer) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.link(origin, relay, SimLink::cost(1), SimLink::cost(1).grant(LOCAL));
	sim.join(relay, peer, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(next(&sim, relay, key), Some(origin));
	assert_eq!(next(&sim, peer, key), Some(relay));
	assert!(sim.holds(relay, key) && sim.holds(peer, key));

	sim.disconnect(origin, relay);
	assert!(sim.starved(relay, key), "the peer still routes through the relay");
	assert!(sim.run());
	assert!(!sim.holds(relay, key) && !sim.holds(peer, key));
}

/// The cost of the longest loop-free path from `from` through nodes not yet `seen`,
/// each hop charged at the receiver's side of the link.
fn longest(sim: &Sim, from: Node, seen: &mut Vec<Node>) -> Metric {
	seen.push(from);
	let mut best = 0;
	for to in 0..sim.links.len() {
		if let Some(link) = sim.links[to][from]
			&& !seen.contains(&to)
		{
			best = best.max(link.cost + longest(sim, to, seen));
		}
	}
	seen.pop();
	best
}

/// Random meshes with the origin behind a relay deliver every message and echo in a
/// random order while links flap and relays restart and collect. A route names the
/// relays it crossed, so no relay ever selects one longer than a loop-free path: a
/// node that forgot the origin cannot take back a copy of its own old route. Without
/// the hops this finds a count to infinity within a dozen cases. Every fourth mesh
/// scopes some relays under a grant, which hands a relay its own route back under a
/// qualified key: nothing may wait on that copy once the mesh is quiet.
#[test]
fn sweep_no_route_outgrows_a_loop_free_path() {
	let mut rng = XorShift(0x0bad_c0de);
	let (mut flaps, mut collected, mut cut, mut restarts) = (0, 0, 0, 0);
	for case in 0..200 {
		let relays = 3 + rng.next(3) as usize;
		let origin = relays;
		let scoped = case % 4 == 1;
		let mut sim = Sim::new(relays + 1);
		let mut links = Vec::new();
		for a in 0..relays {
			for b in a + 1..relays {
				if b == a + 1 || rng.chance(50) {
					let side = SimLink::cost(1 + rng.next(3));
					let grant = Grant {
						subject: ["ga", "gb"][rng.next(2) as usize],
						scope: "",
					};
					match scoped && rng.chance(30) {
						true => sim.link(a, b, side.grant(grant), side),
						false => sim.link(a, b, side, side),
					}
					links.push((a, b));
				}
			}
		}
		// The origin's links come first, so they flap most.
		for _ in 0..1 + rng.next(2) {
			let relay = rng.next(relays as u64) as usize;
			if !sim.connected(origin, relay) {
				sim.join(origin, relay, 1 + rng.next(2));
				links.insert(0, (origin, relay));
			}
		}
		// A restart is a new relay as far as hops go, which a route may cross once more.
		let mut bound = longest(&sim, origin, &mut Vec::new());
		originate(&mut sim, origin, 7, 0);

		let check = |sim: &Sim, step: usize, bound: Metric| {
			for node in 0..relays {
				let metric = sim.route(node, 7).map_or(0, |route| route.metric);
				assert!(
					metric <= bound,
					"case {case} step {step}: node {node} selected a route of {metric}, past {bound}"
				);
			}
			assert_loop_free(sim, relays + 1, 7);
		};
		for step in 0..300 {
			match rng.next(40) {
				0 | 1 => {
					let pick = match rng.chance(60) {
						true => 0,
						false => rng.next(links.len() as u64) as usize,
					};
					let (a, b) = links[pick];
					match sim.connected(a, b) {
						true => sim.disconnect(a, b),
						false => sim.connect(a, b),
					}
					flaps += 1;
				}
				2 => {
					let node = rng.next(relays as u64) as usize;
					if !sim.holds(node, 7) {
						sim.collect(node, 7);
						collected += 1;
					}
				}
				3 => sim.bump(origin),
				4 if case % 4 == 0 && rng.chance(25) => {
					let node = rng.next(relays as u64) as usize;
					sim.kill(node);
					sim.reconnect(node);
					bound += 3 * relays as Metric;
					restarts += 1;
				}
				_ => {
					let (busy, acks) = (sim.busy(), sim.busy_acks());
					let pick = rng.next((busy.len() + acks.len()).max(1) as u64) as usize;
					if let Some(&(from, to)) = busy.get(pick) {
						sim.deliver(from, to);
					} else if let Some(&(from, to)) = acks.get(pick - busy.len()) {
						sim.deliver_ack(from, to);
					}
				}
			}
			check(&sim, step, bound);
		}
		assert!(sim.run(), "case {case} never settles");
		check(&sim, usize::MAX, bound);

		// Quiet: a relay the origin still reaches routes to it and shows its claim, and
		// every other one holds nothing, its distance included. No request is left.
		let mut reached = vec![origin];
		let mut next = 0;
		while let Some(&at) = reached.get(next) {
			next += 1;
			for peer in 0..=relays {
				if sim.links[at][peer].is_some() && sim.connected(at, peer) && !reached.contains(&peer) {
					reached.push(peer);
				}
			}
		}
		cut += usize::from(reached.len() <= relays);
		for node in 0..relays {
			for &id in sim.nodes[node].rx.keys() {
				assert!(!sim.starved(node, id), "case {case}: node {node} waits on {id}");
			}
			// A scoped link files the origin under another key past it.
			if scoped {
				continue;
			}
			match reached.contains(&node) {
				true => {
					assert!(sim.route(node, 7).is_some(), "case {case}: node {node} lost its route");
					assert_eq!(sim.shows(node, 7, ""), Some(true), "case {case}: node {node}");
				}
				false => {
					assert!(!sim.holds(node, 7), "case {case}: node {node} holds a dead route");
					assert!(
						!sim.remembers(node, 7),
						"case {case}: node {node} keeps a dead distance"
					);
					assert_eq!(sim.shows(node, 7, ""), None, "case {case}: node {node}");
				}
			}
		}
		assert_eq!(sim.retained().pending, 0, "case {case}");
	}
	assert!(
		flaps > 0 && collected > 0 && cut > 0 && restarts > 0,
		"{flaps} flaps, {collected} collects, {cut} cut, {restarts} restarts"
	);
}

/// A relay restarts with no memory and hands node 4 a reflection of the route 4 sent
/// before its last one. It crossed 4 and is infeasible there, so 4 waits on it like
/// any infeasible route, and its readers see the claim unavailable, never gone.
#[test]
fn a_relay_restart_keeps_the_claim_unavailable() {
	let origin = 6;
	let mut sim = Sim::new(7);
	sim.join(origin, 1, 1);
	sim.join(1, 3, 1);
	sim.link(1, 4, SimLink::cost(1), SimLink::cost(2));
	sim.link(1, 5, SimLink::cost(1).priority(1), SimLink::cost(3).priority(-1));
	sim.join(3, 4, 1);
	sim.join(4, 5, 1);
	originate(&mut sim, origin, 7, 0);
	assert!(sim.run());

	let before = sim.events.len();
	sim.kill(1);
	sim.reconnect(1);
	assert!(sim.run());
	for node in [4, 5] {
		let seen: Vec<Kind> = (sim.events[before..].iter())
			.filter(|event| event.node == node)
			.map(|event| event.kind)
			.collect();
		assert_eq!(seen, [Kind::Unavailable, Kind::Available], "node {node}");
	}
}

/// Every identity a rotation draws is fresh: never zero, never one still
/// declared or just retired, and within the 53 bits a lite-06 hop carries as
/// itself. The seed makes the first draw zero, and the high bits of every draw
/// set, which a small seed never reaches.
#[test]
fn rotated_identities_are_fresh_and_in_range() {
	// One xorshift64 draw, and its inverse to pick the seed before a chosen draw.
	fn draw(mut x: u64) -> u64 {
		x ^= x << 13;
		x ^= x >> 7;
		x ^ x << 17
	}
	fn undraw(y: u64) -> u64 {
		let unshift = |y: u64, shift: &dyn Fn(u64) -> u64| (0..64).fold(y, |x, _| y ^ shift(x));
		let x = unshift(y, &|x| x << 17);
		let x = unshift(x, &|x| x >> 7);
		unshift(x, &|x| x << 13)
	}
	// The router forces its seed odd, so take the first zero draw an odd seed reaches.
	let seed = (1..)
		.map(|high: u64| undraw(high << 53))
		.find(|seed| seed & 1 == 1)
		.expect("an odd seed");
	assert_eq!(draw(seed) & GENERATED, 0);

	let item = Item {
		seed: 0,
		epoch: None,
		since: 0,
	};
	let mut router: Router<Node, Item> = Router::new(hop(0, 0), seed);
	router.originate(7, 0);
	router.announce(p("live/x"), item).unwrap();
	let mut seen = BTreeSet::from([7]);
	for _ in 0..8 {
		router.age(MAX - 1);
		router.announce(p("live/x"), item).unwrap();
		let fresh = router.declared().expect("declared");
		assert_ne!(fresh, 0);
		assert!(fresh <= GENERATED, "{fresh:#x} is beyond a generated hop id");
		assert!(seen.insert(fresh), "{fresh:#x} was drawn before");
	}
}

/// One batch may withdraw a stream's only claim and start another: the stream
/// moves to the new claim, and the route it rides on stays up throughout.
#[test]
fn a_batch_replacing_a_streams_only_claim_keeps_its_route() {
	let (up, down) = (0, 1);
	let item = |seed| Item {
		seed,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(up, Link::default()).unwrap();
	router.connect(down, Link::default()).unwrap();
	router.peer_interest(down, Path::empty()).unwrap();
	let interest = router.interest();
	let advert = Advert {
		seqno: 0,
		metric: 1,
		down_only: false,
		rtt: None,
		hops: Arc::from([hop(0, 0)]),
	};
	router.route(up, 7, Some(Reach::Finite(advert)));
	router.batch(up, 7, vec![(p("live/a"), Some(item(1)))], Some(1), interest);
	assert!(
		router
			.drain()
			.iter()
			.any(|out| matches!(out, Out::Route { to, reach: Some(_), .. } if *to == down))
	);

	router.batch(
		up,
		7,
		vec![(p("live/a"), None), (p("live/b"), Some(item(2)))],
		Some(2),
		interest,
	);
	let out = router.drain();
	assert!(
		!out.iter()
			.any(|out| matches!(out, Out::Route { to, reach: None, .. } if *to == down)),
		"{out:?}"
	);
	assert!(out.contains(&Out::Batch {
		to: down,
		id: 7,
		changes: vec![(p("live/b"), Some(item(2))), (p("live/a"), None)],
		floor: Some(2),
	}));
}

/// A relay sends a route only to a stream whose interest one of its claims meets,
/// and records a feasibility distance only for a route it sends. Here no stream's
/// interest meets the claim, so the relay commits to nothing it never advertised.
#[test]
fn a_route_no_interest_meets_records_no_distance() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.leaf(origin);
	sim.watch(origin, "other/");
	sim.watch(b, "other/");
	// The relay's own interest certifies nothing over theirs.
	sim.watch(a, "live/");
	sim.join(origin, a, 1);
	sim.join(a, b, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	assert_eq!(next(&sim, a, 7), Some(origin));
	assert_eq!(sim.sent(a, b, 7), None);
	assert!(!sim.remembers(a, 7));
}

/// Two neighbors with feasible routes carry different uncertified attributes for one
/// claim: the relay shows and forwards its selected next hop's, whichever neighbor
/// sorts first.
#[test]
fn an_uncertified_claim_comes_from_the_selected_next_hop() {
	let (near, far, down) = (5, 0, 1);
	let item = |seed| Item {
		seed,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	// Both routes stay feasible: each sender's metric is below any this relay advertises.
	router
		.connect(
			far,
			Link {
				cost: 5,
				..Link::default()
			},
		)
		.unwrap();
	router.connect(near, Link::default()).unwrap();
	router.connect(down, Link::default()).unwrap();
	router.peer_interest(down, Path::empty()).unwrap();
	let interest = router.interest();
	for (from, seed) in [(far, 1), (near, 2)] {
		let advert = Advert {
			seqno: 0,
			metric: 1,
			down_only: false,
			rtt: None,
			hops: Arc::from([hop(9, 0), hop(from, 0)]),
		};
		router.route(from, 7, Some(Reach::Finite(advert)));
		// No floor: the claim lives only while the stream carries it.
		router.batch(from, 7, vec![(p("live/x"), Some(item(seed)))], None, interest);
	}
	assert_eq!(router.selected(7).and_then(|route| route.next), Some(near));
	assert_eq!(router.shown()[&(7, p("live/x"))].item, item(2));
	// Echo any end the relay sent downstream meanwhile, so it starts the route again.
	let mut out = router.drain();
	router.echo(down, 7);
	out.extend(router.drain());
	let last = (out.into_iter())
		.filter_map(|out| match out {
			Out::Batch { to, changes, .. } if to == down => Some(changes),
			_ => None,
		})
		.flatten()
		.rfind(|(claim, _)| *claim == p("live/x"));
	assert_eq!(last, Some((p("live/x"), Some(item(2)))));
}

/// A relay forwarding a neighbor's seqno request along its route. `close` names the
/// session that then closes, and whatever it was, nothing is left to ask for: the
/// relay keeps no request, though nothing else it knows names the identity.
fn forwarded_request_closes(close: Node) -> usize {
	let (up, requester) = (0, 1);
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(up, Link::default()).unwrap();
	router.connect(requester, Link::default()).unwrap();
	let advert = Advert {
		seqno: 0,
		metric: 1,
		down_only: false,
		rtt: None,
		hops: Arc::from([hop(9, 0), hop(up, 0)]),
	};
	router.route(up, 7, Some(Reach::Finite(advert)));
	router.request(requester, 7, 1);
	assert!(router.drain().contains(&Out::Request {
		to: up,
		id: 7,
		seqno: 1
	}));
	assert_eq!(router.retained().pending, 1);
	router.disconnect([close]);
	router.retained().pending
}

#[test]
fn a_forwarded_request_goes_with_its_requesters_session() {
	assert_eq!(forwarded_request_closes(1), 0);
}

#[test]
fn a_forwarded_request_goes_with_the_route_it_follows() {
	assert_eq!(forwarded_request_closes(0), 0);
}

/// A relay keeps nothing for an identity once the only session that routed it
/// closes: no state outlives what a peer told it.
#[test]
fn a_relay_keeps_nothing_once_its_only_route_closes() {
	let up = 0;
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(up, Link::default()).unwrap();
	let advert = Advert {
		seqno: 0,
		metric: 1,
		down_only: false,
		rtt: None,
		hops: Arc::from([hop(9, 0), hop(up, 0)]),
	};
	router.route(up, 7, Some(Reach::Finite(advert)));
	assert!(!router.keeps_nothing(7));
	router.disconnect([up]);
	assert!(router.keeps_nothing(7));
}

/// A claim only ever carried uncertified, as a stream's own, goes offline when the
/// stream drops it: that proves only the stream left it, not that the origin
/// withdrew it. That holds while another certifier has yet to answer, so which
/// regions are certified is unknown.
#[test]
fn an_uncertified_claim_dropped_goes_offline_while_a_certifier_is_unanswered() {
	let (answered, silent) = (0, 1);
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(answered, Link::default()).unwrap();
	router.connect(silent, Link::default()).unwrap();
	let interest = router.interest();
	for from in [answered, silent] {
		let advert = Advert {
			seqno: 0,
			metric: 1,
			down_only: false,
			rtt: None,
			hops: Arc::from([hop(9, 0), hop(from, 0)]),
		};
		router.route(from, 7, Some(Reach::Finite(advert)));
	}
	router.batch(answered, 7, vec![(p("live/x"), Some(item))], None, interest);
	assert_eq!(router.shows(7, &p("live/x")), Some(true));
	router.events();

	router.batch(answered, 7, vec![(p("live/x"), None)], None, interest);
	let kinds: Vec<Kind> = router.events().into_iter().map(|event| event.kind).collect();
	assert_eq!(kinds, [Kind::Offline]);
}

/// A finite route `from` sends, as its next hop toward an origin further on.
fn relayed(from: Node, seqno: Seqno, metric: Metric) -> Option<Reach> {
	Some(Reach::Finite(Advert {
		seqno,
		metric,
		down_only: false,
		rtt: None,
		hops: Arc::from([hop(9, 0), hop(from, 0)]),
	}))
}

/// A claim only a held neighbor certifies is shown unavailable: a held route cannot
/// serve it. Once the selected neighbor's floor covers the claim, it is available,
/// though that batch names no claim at all.
#[test]
fn a_floor_naming_no_claim_makes_a_held_neighbors_claim_available() {
	let (selected, held) = (0, 1);
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(selected, Link::default()).unwrap();
	router.connect(held, Link::default()).unwrap();
	let interest = router.interest();
	for from in [selected, held] {
		router.route(from, 7, relayed(from, 0, 1));
	}
	router.batch(held, 7, vec![(p("live/x"), Some(item))], Some(5), interest);
	// The selected neighbor carries the claim, but certifies nothing yet.
	router.batch(selected, 7, vec![(p("live/x"), Some(item))], None, interest);
	router.route(held, 7, Some(Reach::Held));
	assert_eq!(router.selected(7).and_then(|route| route.next), Some(selected));
	assert_eq!(router.shows(7, &p("live/x")), Some(false));
	router.events();

	router.batch(selected, 7, Vec::new(), Some(5), interest);
	assert_eq!(router.shows(7, &p("live/x")), Some(true));
	let kinds: Vec<Kind> = router.events().into_iter().map(|event| event.kind).collect();
	assert_eq!(kinds, [Kind::Available]);
}

/// A relay that narrows its interest keeps a floor outside it while that floor is
/// newer than every floor inside, then lets it go, with its claims, once a floor
/// inside catches up. A stream watching wider than the relay hears those claims
/// end. The relay asked one neighbor for a sibling region, and that neighbor has
/// yet to answer the narrowed interest, so no pruning lets the region go first.
#[test]
fn a_relay_lets_go_outside_its_interest_once_a_floor_inside_catches_up() {
	let (live, other, down) = (0, 1, 3);
	let item = |seed| Item {
		seed,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	for neighbor in [live, other, down] {
		router.connect(neighbor, Link::default()).unwrap();
	}
	router.asks(live, p("live")).unwrap();
	router.asks(other, p("other")).unwrap();
	router.peer_interest(down, Path::empty()).unwrap();
	let interest = router.interest();
	for from in [live, other] {
		router.route(from, 7, relayed(from, 0, 1));
	}
	router.batch(other, 7, vec![(p("other/x"), Some(item(1)))], Some(10), interest);
	router.batch(live, 7, vec![(p("live/x"), Some(item(2)))], Some(3), interest);

	let interest = router.watch(p("live"));
	router.batch(live, 7, vec![(p("live/x"), Some(item(2)))], Some(3), interest);
	let view = router.view(7).expect("a view");
	assert_eq!(view.floors, BTreeMap::from([(p("live"), 3), (p("other"), 10)]));
	assert!(view.items.contains_key(&p("other/x")));
	router.drain();

	router.batch(live, 7, vec![(p("live/y"), Some(item(3)))], Some(11), interest);
	let view = router.view(7).expect("a view");
	assert_eq!(view.floors, BTreeMap::from([(p("live"), 11)]));
	assert_eq!(
		view.items.keys().cloned().collect::<Vec<_>>(),
		[p("live/x"), p("live/y")]
	);
	let changes: Vec<(Path, Option<Item>)> = (router.drain().into_iter())
		.filter_map(|out| match out {
			Out::Batch { to, changes, .. } if to == down => Some(changes),
			_ => None,
		})
		.flatten()
		.collect();
	assert_eq!(changes, [(p("live/y"), Some(item(3))), (p("other/x"), None)]);
}

/// A relay forwarding a request for a newer seqno than its own starvation needs
/// keeps asking for the newer one: a route at the seqno it needs ends its
/// starvation but answers nothing its requester asked for.
#[test]
fn a_starving_relay_keeps_forwarding_the_newer_seqno_its_requester_asked_for() {
	let (up, requester) = (0, 1);
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let mut router: Router<Node, Item> = Router::new(hop(2, 0), 1);
	router.connect(up, Link::default()).unwrap();
	router.connect(requester, Link::default()).unwrap();
	router.peer_interest(requester, Path::empty()).unwrap();
	let interest = router.interest();
	router.route(up, 7, relayed(up, 0, 1));
	// A claim meets the requester's interest, so the relay sends it the route and
	// records a feasibility distance of seqno 0.
	router.batch(up, 7, vec![(p("live/x"), Some(item))], Some(1), interest);
	assert!(router.remembers(7));
	router.request(requester, 7, 5);
	assert!(router.drain().contains(&Out::Request {
		to: up,
		id: 7,
		seqno: 5
	}));

	// A longer route at the same seqno is infeasible: the relay starves, needing only seqno 1.
	router.route(up, 7, relayed(up, 0, 5));
	assert!(router.starved(7));
	let requests: Vec<_> = (router.drain().into_iter())
		.filter(|out| matches!(out, Out::Request { .. }))
		.collect();
	assert_eq!(requests, [], "the request for seqno 5 already covers it");

	router.route(up, 7, relayed(up, 1, 5));
	assert_eq!(router.selected(7).map(|route| route.seqno), Some(1));
	assert_eq!(router.retained().pending, 1, "seqno 5 is still owed to the requester");
	router.route(up, 7, relayed(up, 5, 5));
	assert_eq!(router.retained().pending, 0);
}

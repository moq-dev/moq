//! Where a request goes, on the path rule: the forwarding, export, and qualified
//! identity fixtures the reference model ran before the router existed.
//!
//! A lite-07 request names its publisher, and each relay follows the route it offered
//! the neighbour the request came from. No transit relay reselects, so the model's
//! counterexamples for path-only requests (local priorities circling, nested claims
//! looping where a relay has not heard the narrower ones, joins deadlocking, another
//! publisher spliced in) have nothing to run on here: each fixture checks that a named
//! request arrives instead. The model's Babel fixtures (feasibility, seqno requests, the
//! distance a node keeps) went with Babel.

use super::*;

const LOCAL: Grant = Grant {
	subject: "local",
	scope: "local/",
};

/// `node` publishes as `id`, claiming each of `claims`.
fn publish(sim: &mut Sim, node: Node, id: Id, claims: &[&str]) {
	sim.originate(node, id);
	for claim in claims {
		sim.announce(node, claim, 0, None);
	}
}

/// Where a reader's request at `node` ends when every relay on the way serves it from
/// its own subscription to the named publisher: each hop follows its own reader's
/// route, as a request joining that subscription does. `None` where one stops short.
/// A cycle would leave every subscription in it waiting on the next, so it panics.
fn joined(sim: &Sim, node: Node, id: Id, claim: &Path) -> Option<Node> {
	let (mut at, mut id) = (node, id);
	let mut seen = BTreeSet::new();
	loop {
		assert!(
			seen.insert((at, id)),
			"subscriptions to {id} {claim:?} join round a cycle from node {node}"
		);
		match sim.path(at).forward(id, claim, None)? {
			path::Forward::Here => return Some(at),
			path::Forward::Next { next, named, .. } => (at, id) = (next, named),
		}
	}
}

/// Three relays in a ring, each next to its own publisher of `live`, and each giving
/// its clockwise link `priority`.
fn priority_ring(priority: i8) -> Sim {
	let mut sim = Sim::new(6);
	for publisher in 3..6 {
		sim.leaf(publisher);
	}
	for relay in 0..3 {
		sim.link(
			relay,
			(relay + 1) % 3,
			SimLink::cost(1).priority(priority),
			SimLink::cost(1),
		);
		sim.join(relay + 3, relay, 1);
	}
	for publisher in 3..6 {
		publish(&mut sim, publisher, publisher as Id, &["live/"]);
	}
	assert!(sim.run());
	sim
}

/// Local priorities that each favour the clockwise link. In the model a request that
/// named only its path picked another publisher at every relay and circled the ring,
/// and three relays subscribing at once joined each other's subscriptions and starved.
/// A request naming its publisher reaches it from every relay, alone or joined.
#[test]
fn local_priorities_never_turn_a_named_request() {
	for priority in [0, 1] {
		let sim = priority_ring(priority);
		let claim = p("live/x");
		for relay in 0..3 {
			for publisher in 3..6 {
				let id = publisher as Id;
				let label = format!("priority {priority}, relay {relay}, publisher {publisher}");
				assert_eq!(sim.reaches(relay, id, &claim), Some(publisher), "{label}");
				assert_eq!(joined(&sim, relay, id, &claim), Some(publisher), "{label}");
			}
		}
	}
}

/// Two publishers of `live/x` behind one core, the second cheaper. A relay that picked
/// the first keeps it however the core would rank them: the core forwards the request
/// it was handed rather than splicing in the publisher it would pick itself.
#[test]
fn a_transit_relay_never_swaps_the_named_publisher() {
	let (edge, core, first, second) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(first);
	sim.leaf(second);
	sim.join(edge, core, 1);
	sim.join(first, core, 2);
	sim.join(second, core, 1);
	publish(&mut sim, first, 2, &["live/x"]);
	publish(&mut sim, second, 3, &["live/x"]);
	assert!(sim.run());
	let claim = p("live/x");
	assert_eq!(sim.reaches(edge, 2, &claim), Some(first));
	assert_eq!(sim.reaches(edge, 3, &claim), Some(second));
	assert_eq!(joined(&sim, edge, 2, &claim), Some(first));
}

/// Nested claims `live`, `live/a`, and `live/a/x` from three publishers on a ring with a
/// chord. In the model each relay took the longest claim it had heard, so relay 0,
/// missing the narrower claims, forwarded toward the wildcard while its neighbours
/// forwarded back through it toward a narrower one: a three-hop loop that requester
/// exclusion missed. Here relay 0 hears the narrower claims late, and a request for
/// any of the three publishers reaches it from every relay, before and after.
#[test]
fn nested_claims_never_loop_a_named_request() {
	let mut sim = Sim::new(7);
	for publisher in 4..7 {
		sim.leaf(publisher);
	}
	sim.join(0, 1, 2);
	sim.join(1, 2, 1);
	sim.join(2, 3, 3);
	sim.join(3, 0, 1);
	sim.join(0, 2, 2);
	sim.join(4, 2, 1);
	sim.join(5, 1, 1);
	sim.join(6, 3, 1);
	publish(&mut sim, 4, 4, &["live/"]);
	assert!(sim.run());
	// Relay 0 hears nothing more until the narrower claims are everywhere else.
	for peer in [1, 2, 3] {
		sim.hold(peer, 0);
	}
	publish(&mut sim, 5, 5, &["live/a/"]);
	publish(&mut sim, 6, 6, &["live/a/x"]);
	assert!(sim.run());
	let claim = p("live/a/x");
	let check = |sim: &Sim, label: &str| {
		for relay in 0..4 {
			for publisher in 4..7 {
				let id = publisher as Id;
				if relay == 0 && sim.queued(1, 0) + sim.queued(2, 0) + sim.queued(3, 0) > 0 && publisher > 4 {
					// Relay 0 has no route to the narrower publishers yet.
					continue;
				}
				assert_eq!(
					sim.reaches(relay, id, &claim),
					Some(publisher),
					"{label}: relay {relay}"
				);
				assert_eq!(
					joined(sim, relay, id, &claim),
					Some(publisher),
					"{label}: relay {relay}"
				);
			}
		}
	};
	check(&sim, "relay 0 behind");
	for peer in [1, 2, 3] {
		sim.release(peer, 0);
	}
	assert!(sim.run());
	check(&sim, "settled");
}

/// Five relays on a ring with chords, random costs and priorities, and three
/// publishers each claiming `live`, `live/a`, or `live/b`. Once settled, every relay's
/// request for every publisher reaches it, alone or joined. Then a ring link closes, and at
/// every round until it settles again no request loops except through a stale route,
/// and once settled every relay reaches every publisher again.
#[test]
fn sweep_named_requests_arrive() {
	const RELAYS: usize = 5;
	let mut rng = XorShift(0x5eed_c0de);
	let prefixes = ["live/", "live/a/", "live/b"];
	for scenario in 0..scenarios() {
		let mut sim = Sim::new(RELAYS + 3);
		for publisher in RELAYS..RELAYS + 3 {
			sim.leaf(publisher);
		}
		for relay in 0..RELAYS {
			let side = |rng: &mut XorShift| SimLink::cost(1 + rng.next(3)).priority(if rng.chance(25) { 1 } else { 0 });
			let (ab, ba) = (side(&mut rng), side(&mut rng));
			sim.link(relay, (relay + 1) % RELAYS, ab, ba);
		}
		for _ in 0..2 {
			let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
			if a.abs_diff(b) > 1 && a.abs_diff(b) != RELAYS - 1 && !sim.connected(a, b) {
				sim.join(a, b, 1 + rng.next(3));
			}
		}
		for publisher in RELAYS..RELAYS + 3 {
			sim.join(publisher, rng.next(RELAYS as u64) as usize, 1);
			let prefix = prefixes[rng.next(3) as usize];
			publish(&mut sim, publisher, publisher as Id, &[prefix]);
		}
		assert!(sim.run(), "scenario {scenario} settles");

		// Every relay watches the root, so its route covers any path: the request reaches
		// the publisher, which answers for what it does not claim.
		let arrive = |sim: &Sim, label: &str| {
			for at in 0..RELAYS {
				for publisher in RELAYS..RELAYS + 3 {
					let id = publisher as Id;
					for path in ["live/a/x", "live/b"] {
						let claim = p(path);
						let label = format!("scenario {scenario} {label}, relay {at}, publisher {publisher}, {path}");
						assert_eq!(sim.reaches(at, id, &claim), Some(publisher), "{label}");
						assert_eq!(joined(sim, at, id, &claim), Some(publisher), "{label}");
					}
				}
			}
		};
		arrive(&sim, "settled");

		let cut = rng.next(RELAYS as u64) as usize;
		sim.disconnect(cut, (cut + 1) % RELAYS);
		let mut quiet = false;
		for _ in 0..1_000 {
			for publisher in RELAYS..RELAYS + 3 {
				assert_loop_free(&sim, RELAYS + 3, publisher as Id);
			}
			if sim.busy().is_empty() && sim.deadline().is_none() {
				quiet = true;
				break;
			}
			sim.settle(1);
		}
		assert!(quiet, "scenario {scenario} settles after the cut");
		arrive(&sim, "after the cut");
	}
}

/// A scoped peer declares a victim's identity as its own and claims what the victim
/// claims, more cheaply. Admitted unscoped, its routes merge with the victim's and
/// requests for the victim's claims outside the forger's namespace go to the forger.
/// Admitted under a scoped grant, its identity is filed under a key derived from the
/// grant's subject, which carries only claims inside its scope, so the victim keeps
/// its own claims.
#[test]
fn a_forged_identity_is_contained_by_a_scoped_admission() {
	const FORGER: Grant = Grant {
		subject: "forger",
		scope: "forger/",
	};
	for scoped in [false, true] {
		let (victim, core, edge, forger) = (0, 1, 2, 3);
		let mut sim = Sim::new(4);
		sim.leaf(victim);
		sim.leaf(forger);
		sim.join(victim, core, 5);
		sim.join(core, edge, 1);
		let admission = match scoped {
			true => SimLink::cost(1).grant(FORGER),
			false => SimLink::cost(1),
		};
		sim.link(edge, forger, admission, SimLink::cost(1));
		publish(&mut sim, victim, 7, &["other/", "forger/"]);
		publish(&mut sim, forger, 7, &["other/", "forger/"]);
		assert!(sim.run());

		let other = p("other/x");
		match scoped {
			false => assert_eq!(
				sim.reaches(core, 7, &other),
				Some(forger),
				"the forger serves another namespace"
			),
			true => {
				assert_eq!(sim.reaches(core, 7, &other), Some(victim));
				let key = qualified(FORGER.subject, 7);
				assert_eq!(sim.shows(core, key, "other"), None, "outside the forger's scope");
				assert_eq!(
					sim.reaches(core, key, &p("forger/x")),
					Some(forger),
					"the forgery stays inside its own scope"
				);
			}
		}
	}
}

/// A local mesh publishing through two uplinks, each admitting its local relay under
/// the same subject, so both CDN relays file the local publisher under one key. It
/// survives losing either uplink: the CDN keeps the key and a request follows the
/// other uplink. The local relay's own identity is filed the same way and fails over
/// alike. Claims outside the scope are refused under that key.
#[test]
fn scoped_delivery_through_two_uplinks_reaches_one_key() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.leaf(origin);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	let admit = SimLink::cost(1).grant(LOCAL);
	sim.link(l1, c1, SimLink::cost(1).upstream(), admit);
	sim.link(l2, c2, SimLink::cost(1).upstream(), admit);
	sim.join(origin, l1, 1);
	publish(&mut sim, origin, 9, &["local/", "cdn/"]);
	publish(&mut sim, l1, 20, &["local/l1/"]);
	assert!(sim.run());

	let key = qualified(LOCAL.subject, 9);
	let own = qualified(LOCAL.subject, 20);
	assert!(!sim.routed(c1, 9), "a scoped peer's identities are qualified");
	assert!(!sim.routed(c1, 20), "its own included");
	assert_eq!(sim.next_hop(c1, key), Some(l1));
	assert_eq!(sim.next_hop(c2, key), Some(l2));
	assert_eq!(sim.next_hop(c1, own), Some(l1));
	assert_eq!(sim.shows(c2, key, "cdn"), None, "outside the scope");
	assert_eq!(sim.reaches(l1, 9, &p("cdn/x")), Some(origin));

	sim.disconnect(l1, c1);
	assert!(sim.run());
	assert_eq!(sim.next_hop(c1, own), Some(c2));
	assert_eq!(sim.reaches(c1, own, &p("local/l1/x")), Some(l1));
	assert_eq!(sim.reaches(c1, key, &p("local/x")), Some(origin));

	sim.connect(l1, c1);
	sim.disconnect(l2, c2);
	assert!(sim.run());
	assert_eq!(sim.next_hop(c2, key), Some(c1));
	assert_eq!(sim.reaches(c2, key, &p("local/x")), Some(origin));
}

/// Two CDN relays, each the uplink of one relay in a local mesh, and a CDN publisher
/// on the first. The CDN admits each local relay under `LOCAL`, and the second local
/// relay forgets to mark its uplink upstream.
fn leaky_uplinks() -> Sim {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.leaf(origin);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	let admit = SimLink::cost(1).grant(LOCAL);
	sim.link(l1, c1, SimLink::cost(1).upstream(), admit);
	sim.link(l2, c2, SimLink::cost(1), admit);
	sim.join(origin, c1, 1);
	publish(&mut sim, origin, 7, &[""]);
	sim
}

/// The leaked CDN route comes back to the CDN filed under the local subject's key, and
/// that key's route then leaks down and up again. A scoped connection forwarding an
/// already qualified key has that route refused, so the lap ends after one filing
/// instead of minting a new identity on every lap.
#[test]
fn a_qualified_identity_is_never_qualified_again() {
	let (c1, c2, l2) = (0, 1, 3);
	let mut sim = leaky_uplinks();
	assert!(sim.run(), "the leak settles");
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(sim.next_hop(c2, 7), Some(c1));
	assert_eq!(sim.next_hop(c2, key), Some(l2), "the leak is filed apart once");
	for node in [c1, c2] {
		assert!(
			!sim.routed(node, qualified(LOCAL.subject, key)),
			"node {node} never files the key again"
		);
	}
}

/// Redundant cores: an edge losing the core it routes through moves to the other at
/// once, since the other core's route avoids the closed session's far end.
#[test]
fn redundant_cores_fail_over_at_once() {
	let (ingest, core_a, core_b, edge) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(ingest);
	sim.join(ingest, core_a, 1);
	sim.join(ingest, core_b, 1);
	sim.join(core_a, core_b, 1);
	sim.link(edge, core_a, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(edge, core_b, SimLink::cost(1).upstream(), SimLink::cost(1));
	publish(&mut sim, ingest, 7, &[""]);
	assert!(sim.run());
	let primary = sim.next_hop(edge, 7).expect("a route");
	let other = if primary == core_a { core_b } else { core_a };

	sim.disconnect(edge, primary);
	assert_eq!(sim.next_hop(edge, 7), Some(other));
	assert_eq!(sim.shows(edge, 7, ""), Some(true), "never unavailable");
	assert_eq!(sim.reaches(edge, 7, &p("x")), Some(ingest));
}

/// A publisher reached directly over a preferred peer-to-peer session and through the
/// CDN, by a subscriber that is a browser or a routing node. Losing the direct session
/// keeps the claim shown, and the subscriber ends up on the CDN route either way.
#[test]
fn a_lost_p2p_session_falls_back_to_the_cdn() {
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
		publish(&mut sim, publisher, 9, &[""]);
		assert!(sim.run());
		assert_eq!(sim.next_hop(subscriber, 9), Some(publisher), "transit {transit}");

		sim.disconnect(subscriber, publisher);
		assert!(sim.shows(subscriber, 9, "").is_some(), "transit {transit}: still shown");
		assert!(sim.run());
		assert_eq!(sim.next_hop(subscriber, 9), Some(edge), "transit {transit}");
		assert_eq!(sim.shows(subscriber, 9, ""), Some(true), "transit {transit}");
	}
}

/// A local relay whose own uplink is down still consumes CDN content and publishes
/// local content through its LAN peer: the down-only route flows down the LAN, and
/// the exportable one up the other uplink.
#[test]
fn a_dead_uplink_consumes_and_publishes_through_a_lan_peer() {
	let (c1, c2, l1, l2, cdn, local) = (0, 1, 2, 3, 4, 5);
	let mut sim = Sim::new(6);
	sim.leaf(cdn);
	sim.leaf(local);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	sim.link(l1, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(l2, c2, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.disconnect(l2, c2);
	sim.join(cdn, c1, 1);
	sim.join(local, l2, 1);
	publish(&mut sim, cdn, 7, &["cdn/"]);
	publish(&mut sim, local, 9, &["local/"]);
	assert!(sim.run());

	assert_eq!(sim.next_hop(l2, 7), Some(l1));
	assert_eq!(sim.reaches(l2, 7, &p("cdn/x")), Some(cdn));
	assert_eq!(sim.next_hop(c2, 9), Some(c1));
	assert_eq!(sim.reaches(c2, 9, &p("local/x")), Some(local));
}

/// `L2` reaches a local publisher over the LAN through `L1`, and `C2`, which `L2` marks
/// upstream, routes through `L2` until the CDN hears the publisher itself. Then `L2`'s
/// cheapest route runs down from `C2`, so it is down-only and never offered back up:
/// `C2` moves onto the CDN, and a request `C2` forwarded to `L2` before stops at `L2`
/// rather than following a route back up through `C2`.
#[test]
fn an_upstream_request_stops_once_the_route_turns_down_only() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.leaf(origin);
	sim.join(c1, c2, 1);
	sim.join(origin, l1, 10);
	sim.join(l2, l1, 1);
	sim.link(l2, c2, SimLink::cost(1).upstream(), SimLink::cost(1));
	publish(&mut sim, origin, 7, &["local/"]);
	assert!(sim.run());
	assert_eq!(sim.next_hop(c2, 7), Some(l2));
	let claim = p("local/x");
	assert_eq!(sim.reaches(c2, 7, &claim), Some(origin));
	let Some(path::Forward::Next { prefix, .. }) = sim.path(c2).forward(7, &claim, None) else {
		panic!("C2 forwards to L2");
	};

	sim.join(origin, c1, 1);
	assert!(sim.run());
	assert_eq!(sim.next_hop(l2, 7), Some(c2));
	let route = sim.path(l2).route_for(7, &Path::empty()).and_then(path::Route::offer);
	assert!(route.expect("a route").down_only);
	assert_eq!(sim.next_hop(c2, 7), Some(c1), "C2 re-resolves off the LAN");
	assert_eq!(sim.reaches(c2, 7, &claim), Some(origin));
	assert!(
		sim.path(l2).forward(7, &claim, Some((c2, prefix))).is_none(),
		"L2 stops the request it accepted from upstream"
	);
}

/// A local relay `L2` reaching a local publisher over the LAN and through its uplink,
/// with the LAN or the uplink coming up first. The path rule ranks each route by the
/// lowest link priority on it and then by cost, with no feasibility condition, so the
/// same topology settles on the same route whichever session came up first. Under
/// Babel a transit relay kept a longer route it heard first.
#[test]
fn transit_selection_does_not_depend_on_arrival_order() {
	let (c1, c2, l1, l2, origin) = (0, 1, 2, 3, 4);
	for prefer_uplink in [false, true] {
		let settled = |lan_first: bool| {
			let mut sim = Sim::new(5);
			sim.leaf(origin);
			sim.join(c1, c2, 10);
			sim.join(origin, l1, 1);
			publish(&mut sim, origin, 7, &["local/"]);
			let (uplink, lan) = if prefer_uplink { (1, 0) } else { (0, 1) };
			let lan = |sim: &mut Sim| sim.link(l2, l1, SimLink::cost(1).priority(lan), SimLink::cost(1));
			if lan_first {
				lan(&mut sim);
			}
			sim.link(l1, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
			sim.link(l2, c2, SimLink::cost(1).upstream().priority(uplink), SimLink::cost(1));
			assert!(sim.run());
			if !lan_first {
				lan(&mut sim);
			}
			assert!(sim.run());
			sim.next_hop(l2, 7)
		};
		assert_eq!(settled(false), settled(true), "prefer uplink {prefer_uplink}");
	}
}

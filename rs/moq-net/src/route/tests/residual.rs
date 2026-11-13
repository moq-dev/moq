//! What the reviews of the hop list found on meshes where neighbours know different
//! parts of a publisher's claims, run on both rules.
//!
//! The first three were bugs of [`Router`], which the route per scope fixes: each
//! asserts the fix under the path rule and still pins the bug under the distance
//! rule, which sessions run until the switch (`quest/m2/cluster-routes/switch.md`)
//! deletes it. The others are rules either must keep.

use super::*;

both!(
	a_seqno_advance_storms_between_two_grants,
	a_relay_behind_a_scoped_neighbour_is_blind,
	a_relay_forgets_a_claim_a_fresh_relay_would_show,
	a_certifier_that_returns_brings_back_what_it_certifies,
	a_seqno_advance_leaves_a_relays_claims_alone,
	two_narrowed_interests_settle,
	two_relays_behind_a_scoped_relay_let_a_dead_claim_go,
);

/// The cost of `node`'s route for `id` under `prefix`.
fn metric(sim: &Sim, node: Node, id: Id, prefix: &str) -> Option<Metric> {
	match sim.rule() {
		Rule::Distance => sim.route(node, id).map(|route| route.metric),
		Rule::Path(_) => (sim.path(node).route_for(id, &p(prefix)))
			.and_then(|route| route.offer())
			.map(|offer| offer.metric),
	}
}

/// One advance of the origin's seqno and this mesh never goes quiet again under
/// round-robin delivery. Relay 5 files the origin under a qualified key for its
/// scoped peers 4 and 7, whose claims there come and go with whichever copy is
/// feasible, so 5 starts and ends the route to 4 on each advance and 4 asks for the
/// next seqno every time. The path rule has no seqno: the session the advance
/// stands for closes and opens again, and the mesh goes quiet.
fn a_seqno_advance_storms_between_two_grants(rule: Rule) {
	let ga = Grant {
		subject: "ga",
		scope: "",
	};
	let gb = Grant {
		subject: "gb",
		scope: "",
	};
	let origin = 9;
	let mut sim = Sim::with(rule, 10);
	sim.link(origin, 4, SimLink::cost(1), SimLink::cost(2));
	sim.join(origin, 7, 1);
	sim.join(1, 4, 1);
	sim.join(1, 5, 1);
	sim.join(2, 3, 1);
	sim.join(2, 5, 1);
	sim.join(3, 4, 1);
	sim.join(3, 6, 1);
	sim.link(4, 5, SimLink::cost(1).upstream(), SimLink::cost(1).grant(gb));
	sim.link(5, 6, SimLink::cost(1).grant(ga), SimLink::cost(1));
	sim.link(5, 7, SimLink::cost(1).grant(gb), SimLink::cost(1));
	sim.link(6, 7, SimLink::cost(3), SimLink::cost(1));
	sim.originate(origin, 8, 0);
	sim.announce(origin, "", 0, None);
	sim.watch(4, "a");
	sim.watch(7, "b");
	assert!(sim.settle(3000));
	if rule == Rule::Distance {
		assert_eq!(sim.seqno(origin), 0, "it converges with no request");
		sim.bump(origin);
		assert!(!sim.settle(500), "the storm is fixed: drop the residual");
		assert!(sim.seqno(origin) > 20, "each lap asks the origin for another seqno");
		return;
	}
	let shown = |sim: &Sim| [4, 7].map(|node| sim.shown(node).clone());
	let before = shown(&sim);
	sim.disconnect(origin, 4);
	sim.connect(origin, 4);
	assert!(sim.settle(500));
	assert_eq!(shown(&sim), before);
}

/// The relay's closest neighbour asked for `a` only. A neighbour as far from the
/// origin as the relay itself knows everything, and is not feasible, so the relay
/// neither shows `b/x` nor has a route to ask for it on. The path rule keeps the
/// wide neighbour's route beside the narrow one's, and a request for `b/x` takes it.
fn a_relay_behind_a_scoped_neighbour_is_blind(rule: Rule) {
	let (origin, narrow, wide, far, relay, tail) = (0, 1, 2, 3, 4, 5);
	let mut sim = Sim::with(rule, 6);
	sim.watch(narrow, "a");
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
	assert_eq!(sim.shows(wide, 7, "b/x"), Some(true));
	for node in [relay, tail] {
		assert_eq!(sim.shows(node, 7, "a/x"), Some(true), "node {node}");
		let blind = rule == Rule::Distance;
		assert_eq!(sim.shows(node, 7, "b/x"), (!blind).then_some(true), "node {node}");
	}
	if rule == Rule::Distance {
		let routes: Vec<Node> = (sim.babel(relay).eligible(7).into_iter())
			.filter(|route| !route.tied)
			.map(|route| route.next)
			.collect();
		assert_eq!(routes, [narrow], "the wide neighbour is no route either");
		return;
	}
	assert_eq!(sim.reaches(relay, 7, &p("b/x")), Some(origin));
	let first = sim.path(relay).forward(7, &p("b/x"), None);
	assert!(
		matches!(first, Some(crate::route::path::Forward::Next { next, .. }) if next == wide),
		"{first:?}"
	);
	let first = sim.path(relay).forward(7, &p("a/x"), None);
	assert!(
		matches!(first, Some(crate::route::path::Forward::Next { next, .. }) if next == narrow),
		"{first:?}"
	);
}

/// The mesh the reviews found the forgotten claim on: two origins, and six relays
/// with a link to relay 0 from each of the others. `cut` leaves out the link from
/// relay 0 to relay 3, and `narrow` has relay 2 ask for `b` alone from the start.
fn forgetful(rule: Rule, cut: bool, narrow: bool) -> Sim {
	let (origin, other) = (6, 7);
	let mut sim = Sim::with(rule, 8);
	if narrow {
		sim.watch(2, "b");
	}
	sim.join(other, 4, 1);
	sim.join(other, 5, 1);
	sim.join(origin, 3, 1);
	sim.join(origin, 2, 1);
	sim.join(0, 1, 1);
	sim.link(0, 2, SimLink::cost(3), SimLink::cost(1));
	if !cut {
		sim.join(0, 3, 1);
	}
	sim.join(0, 4, 1);
	sim.link(1, 2, SimLink::cost(2), SimLink::cost(1));
	sim.join(2, 3, 1);
	sim.join(2, 5, 1);
	sim.link(3, 4, SimLink::cost(1), SimLink::cost(2));
	sim.originate(origin, 7, 0);
	sim.announce(origin, "", 0, None);
	sim.originate(other, 8, 0);
	sim.announce(other, "", 0, None);
	assert!(sim.run());
	sim
}

/// What a relay shows depends on the order things happened in. Relay 0 loses its
/// link to relay 3, and relay 2 then asks for `b` alone. Relay 0 is left routing
/// through relay 2 and shows nothing of the origin, though its neighbour relay 4
/// shows the claim, and a relay that came up in the mesh as it now stands shows it
/// too. Nothing will bring the claim back. Under the path rule relay 0 shows what a
/// fresh relay would.
fn a_relay_forgets_a_claim_a_fresh_relay_would_show(rule: Rule) {
	let mut sim = forgetful(rule, false, false);
	assert_eq!(sim.shows(0, 7, ""), Some(true));
	sim.disconnect(0, 3);
	assert!(sim.run());
	sim.watch(2, "b");
	assert!(sim.run());
	let fresh = forgetful(rule, true, true);
	for sim in [&sim, &fresh] {
		assert_eq!(sim.shows(4, 7, ""), Some(true));
		assert_eq!(metric(sim, 0, 7, ""), Some(4));
	}
	assert_eq!(fresh.shows(0, 7, ""), Some(true));
	let forgot = rule == Rule::Distance;
	assert_eq!(sim.shows(0, 7, ""), (!forgot).then_some(true));
}

/// A neighbour that stops certifying and returns brings back what it certified.
/// The fix first tried for the forgotten claim broke the two tests below.
fn a_certifier_that_returns_brings_back_what_it_certifies(rule: Rule) {
	let mut sim = forgetful(rule, false, false);
	sim.watch(3, "b");
	assert!(sim.run());
	assert_eq!(sim.shows(4, 7, ""), Some(true));

	sim.disconnect(6, 3);
	assert!(sim.run());
	assert_eq!(sim.shows(4, 7, ""), Some(true));
	sim.disconnect(2, 3);
	assert!(sim.run());
	assert_eq!(sim.shows(4, 7, ""), Some(true));
}

/// A neighbour at the relay's own distance is feasible for one message during a
/// seqno advance. A relay that takes its view back then flaps its claims on every
/// advance. The path rule has no seqno and shows the whole claim through the wide
/// neighbour, and a flap of the narrow neighbour's uplink leaves it alone.
fn a_seqno_advance_leaves_a_relays_claims_alone(rule: Rule) {
	let (origin, wide, narrow, relay) = (0, 1, 2, 3);
	let mut sim = Sim::with(rule, 4);
	sim.watch(narrow, "a");
	sim.link(origin, wide, SimLink::cost(1), SimLink::cost(2));
	sim.join(origin, narrow, 1);
	sim.join(wide, relay, 1);
	sim.join(narrow, relay, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "", 0, None);
	assert!(sim.run());
	let claim = match rule {
		Rule::Distance => "a",
		Rule::Path(_) => "",
	};
	assert_eq!(sim.shows(relay, 7, claim), Some(true));

	let before = sim.events.len();
	match rule {
		Rule::Distance => sim.bump(origin),
		Rule::Path(_) => {
			sim.disconnect(origin, narrow);
			sim.connect(origin, narrow);
		}
	}
	assert!(sim.settle(500));
	assert!(sim.events[before..].iter().all(|event| event.node != relay));
	assert_eq!(sim.shows(relay, 7, claim), Some(true));
	assert_eq!(sim.shown(relay).len(), 1);
}

/// The same flap makes a relay start and end its route on a neighbour's stream on
/// every advance, and that neighbour asks for the next seqno each time.
fn two_narrowed_interests_settle(rule: Rule) {
	let (relay, wide, a, b, tail, origin) = (0, 1, 2, 3, 4, 5);
	let mut sim = Sim::with(rule, 6);
	sim.watch(a, "a");
	sim.watch(b, "b");
	sim.join(origin, a, 1);
	sim.link(origin, wide, SimLink::cost(1), SimLink::cost(2));
	sim.join(relay, wide, 1);
	sim.join(relay, a, 1);
	sim.join(relay, b, 1);
	sim.join(b, tail, 1);
	sim.link(tail, a, SimLink::cost(4), SimLink::cost(1));
	sim.originate(origin, 7, 0);
	sim.announce(origin, "", 0, None);
	assert!(sim.settle(3000), "never settles");
	assert!(sim.paths() || sim.seqno(origin) <= 1);
}

/// Two relays behind a relay whose only way to the origin asked for `a`. One of
/// them knew everything through a dear link of its own, which closes, and the
/// origin then withdraws `b/x`. Neither may keep it alive through the other: a rule
/// that believes any neighbour whose route did not cross the relay does.
fn two_relays_behind_a_scoped_relay_let_a_dead_claim_go(rule: Rule) {
	let (origin, narrow, relay, left, right) = (0, 1, 2, 3, 4);
	let mut sim = Sim::with(rule, 5);
	sim.watch(narrow, "a");
	sim.join(origin, narrow, 1);
	sim.join(narrow, relay, 1);
	sim.join(relay, left, 1);
	sim.join(relay, right, 1);
	sim.join(left, right, 1);
	sim.join(origin, right, 5);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	sim.announce(origin, "b/x", 0, None);
	assert!(sim.run());
	assert_eq!(sim.shows(right, 7, "b/x"), Some(true));

	sim.disconnect(origin, right);
	assert!(sim.run());
	sim.unannounce(origin, "b/x");
	assert!(sim.run());
	for node in [relay, left, right] {
		assert_eq!(sim.shows(node, 7, "a/x"), Some(true), "node {node}");
		assert_eq!(sim.shows(node, 7, "b/x"), None, "node {node}");
	}
}

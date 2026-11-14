//! Route selection: GOAWAY, the down-only bit, qualified identities, the hop limit,
//! and how a view merges what each neighbour certifies.

use super::*;

const LOCAL: Grant = Grant {
	subject: "local",
	scope: "local/",
};

/// Originate `id` at `node` with a root claim, which meets every interest: a
/// route goes to a stream only while a claim meets the stream's interest.
fn originate(sim: &mut Sim, node: Node, id: Id) {
	sim.originate(node, id);
	sim.announce(node, "", 0, None);
}

fn next(sim: &Sim, node: Node, id: Id) -> Option<Node> {
	sim.next_hop(node, id)
}

/// The cost of `node`'s root route for `id`.
fn metric(sim: &Sim, node: Node, id: Id) -> Option<Metric> {
	let route = sim.path(node).route_for(id, &Path::empty())?;
	Some(route.offer()?.metric)
}

/// A route along `hops`, the publisher first and the sender last, costing `metric`.
fn offer(hops: impl IntoIterator<Item = Id>, metric: Metric) -> Option<path::Reach> {
	let path = (hops.into_iter().enumerate())
		.map(|(session, node)| path::Hop {
			node,
			session: session as u64,
		})
		.collect();
	Some(path::Reach::Finite(path::Offer {
		path,
		draining: false,
		priority: 0,
		metric,
		down_only: false,
		rtt: None,
	}))
}

fn shows(router: &path::Router<Node, Item>, id: Id, claim: &str) -> Option<bool> {
	router.shown().get(&(id, p(claim))).map(|shown| shown.available)
}

/// A selection rule that never converges on a zero-cost link refuses one when it is
/// configured.
#[test]
fn zero_cost_links_are_refused() {
	let mut router: path::Router<Node, Item> = path::Router::new(99, 1, Doubt::default());
	let link = Link {
		cost: 0,
		..Link::default()
	};
	assert_eq!(router.connect(1, 101, link), Err(Error::ZeroCost));
}

/// A total reaching the infinite metric is unreachable, never saturated.
#[test]
fn overflow_is_unreachable() {
	let (origin, a, b) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, a, MAX - 1);
	sim.join(a, b, 5);
	originate(&mut sim, origin, 7);
	assert!(sim.run());
	assert_eq!(metric(&sim, a, 7), Some(MAX - 1));
	assert!(!sim.routed(b, 7));
	assert_eq!(sim.shows(b, 7, ""), None, "unreachable, not shown");
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
	originate(&mut sim, ingest, 7);
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
	originate(&mut sim, origin, 7);
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
	let route = sim.path(l2).route_for(7, &Path::empty()).and_then(|route| route.offer());
	assert!(route.expect("a route").down_only);
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

/// A partitioned local mesh reaches its other half through the CDN once the doubt
/// on the far end of the closed session lifts, and heals back onto the LAN.
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
	originate(&mut sim, origin, 7);
	assert!(sim.run());
	assert_eq!(next(&sim, l1, 7), Some(l2));

	sim.disconnect(l1, l2);
	assert!(sim.run());
	assert_eq!(next(&sim, l1, 7), Some(c1));

	sim.connect(l1, l2);
	assert!(sim.run());
	assert_eq!(next(&sim, l1, 7), Some(l2), "heals");
}

/// A message to one identity recomputes that identity alone, however many others
/// the node routes.
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
			sim.originate(origin, origin as Id);
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
/// unreachable, so it shows nothing: only the reachable neighbour's region shows,
/// and a held overflowing route keeps nothing.
#[test]
fn an_overflowing_route_neither_certifies_nor_holds() {
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

	let mut router: path::Router<Node, Item> = path::Router::new(99, 1, Doubt::default());
	router.connect(near, 101, Link::default()).unwrap();
	router.connect(far, 102, far_link.clone()).unwrap();
	router.route(near, 7, p("live/a"), offer([100, 101], 1), 0);
	router.batch(near, 7, p("live/a"), vec![(p("live/a/x"), Some(item))], Some(1), 0);
	router.route(far, 7, Path::empty(), offer([100, 102], 1), 0);
	router.batch(far, 7, Path::empty(), vec![(p("live/b/y"), Some(item))], Some(1), 0);
	assert_eq!(shows(&router, 7, "live/a/x"), Some(true));
	assert_eq!(
		shows(&router, 7, "live/b/y"),
		None,
		"only the unreachable route carries it"
	);

	let mut router: path::Router<Node, Item> = path::Router::new(99, 1, Doubt::default());
	router.connect(far, 102, far_link).unwrap();
	router.route(far, 7, Path::empty(), offer([100, 102], 1), 0);
	router.batch(far, 7, Path::empty(), vec![(p("live/x"), Some(item))], Some(1), 0);
	router.route(far, 7, Path::empty(), Some(path::Reach::Held), 0);
	assert_eq!(shows(&router, 7, "live/x"), None);
}

/// A path that crossed as many nodes as one may is refused like an ended route: what
/// only that neighbour carried goes at once, while one hop fewer moves nothing.
#[test]
fn a_route_grown_to_the_hop_limit_certifies_nothing() {
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let (narrow, wide) = (1, 2);
	let hops = |count: usize| (0..count as Id).map(|hop| 200 + hop).chain([102]);
	let mut router: path::Router<Node, Item> = path::Router::new(99, 1, Doubt::default());
	router.connect(narrow, 101, Link::default()).unwrap();
	router.connect(wide, 102, Link::default()).unwrap();
	router.route(narrow, 7, p("live/a"), offer([100, 101], 1), 0);
	router.batch(narrow, 7, p("live/a"), vec![(p("live/a/x"), Some(item))], Some(1), 0);
	router.route(wide, 7, Path::empty(), offer(hops(MAX_HOPS - 3), 1), 0);
	router.batch(wide, 7, Path::empty(), vec![(p("live/b/y"), Some(item))], Some(1), 0);
	assert_eq!(shows(&router, 7, "live/b/y"), Some(true));

	router.route(wide, 7, Path::empty(), offer(hops(MAX_HOPS - 2), 1), 0);
	assert_eq!(shows(&router, 7, "live/b/y"), Some(true), "one hop more moves nothing");
	router.route(wide, 7, Path::empty(), offer(hops(MAX_HOPS - 1), 1), 0);
	assert_eq!(
		shows(&router, 7, "live/b/y"),
		None,
		"the route crossed as many nodes as one may"
	);
	assert_eq!(shows(&router, 7, "live/a/x"), Some(true));
}

/// Closing a session's streams together leaves the node as closing them one at a
/// time does: each stream carries its own prefix, and some share one, so a region
/// one closes stays carried by another while the rest let go.
#[test]
fn closing_streams_together_matches_closing_them_in_turn() {
	let item = |seed| Item {
		seed,
		epoch: None,
		since: 1,
	};
	let now = Instant::ZERO;
	let opened = || {
		let mut router: path::Router<Node, Item> = path::Router::new(99, 1, Doubt::default());
		for stream in 0..6 {
			let prefix = p(&format!("live/{}", stream % 4));
			router.connect(stream, 101, Link::default()).unwrap();
			router.route(stream, 7, prefix.clone(), offer([100, 101], 1), 0);
			let claims = (0..3)
				.map(|j| (prefix.join(&p(&j.to_string())).unwrap(), Some(item(j))))
				.collect();
			router.batch(stream, 7, prefix, claims, Some(1), 0);
		}
		router
	};
	for closed in [vec![0, 1, 4], vec![0, 1, 2, 3, 4, 5]] {
		let mut together = opened();
		together.disconnect(closed.iter().copied(), now);
		let mut in_turn = opened();
		for &stream in &closed {
			in_turn.disconnect([stream], now);
		}
		assert_eq!(together.shown(), in_turn.shown(), "closed {closed:?}");
		assert_eq!(together.retained(), in_turn.retained(), "closed {closed:?}");
		if closed.len() == 6 {
			assert!(together.shown().is_empty());
		}
	}
}

/// A relay hands a peer under a scoped grant the origin's route, which the peer
/// files under a qualified key. The copy it would hand back names the relay, so the
/// relay never takes it for a route of its own.
#[test]
fn a_route_that_returns_through_a_scoped_peer_is_refused() {
	let (origin, relay, peer) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.join(origin, relay, 1);
	sim.link(relay, peer, SimLink::cost(1).grant(LOCAL), SimLink::cost(1));
	sim.originate(origin, 7);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	let key = qualified(LOCAL.subject, 7);
	assert_eq!(next(&sim, peer, 7), Some(relay));
	assert!(!sim.routed(relay, key), "it crossed the relay");
	assert_eq!(sim.shows(relay, key, "local/x"), None);

	sim.disconnect(origin, relay);
	assert!(sim.run());
	assert!(!sim.holds(relay, key) && !sim.holds(peer, 7));
}

/// A route that already crossed as many nodes as one may, its origin included, goes
/// no further: the next relay cannot add itself, so it refuses the path, and nothing
/// is sent on.
#[test]
fn a_route_stops_at_the_hop_limit() {
	let last = MAX_HOPS + 1;
	let mut sim = Sim::new(last + 1);
	for node in 0..last {
		sim.join(node, node + 1, 1);
	}
	originate(&mut sim, 0, 7);
	assert!(sim.run());
	let full = MAX_HOPS - 1;
	assert_eq!(metric(&sim, full, 7), Some(full as Metric));
	assert!(
		!sim.routed(full + 1, 7),
		"the route crossed as many nodes as one may"
	);
	assert!(!sim.holds(last, 7), "nothing is sent on");
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

/// Random meshes with the origin behind a relay deliver every message in a random
/// order while links flap and relays restart and collect. A route names the relays
/// it crossed, so no relay ever selects one longer than a loop-free path: a node that
/// forgot the origin cannot take back a copy of its own old route. Every fourth mesh
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
		originate(&mut sim, origin, 7);

		let check = |sim: &Sim, step: usize, bound: Metric| {
			for node in 0..relays {
				let metric = metric(sim, node, 7).unwrap_or(0);
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
				3 if case % 4 == 0 && rng.chance(25) => {
					let node = rng.next(relays as u64) as usize;
					sim.kill(node);
					sim.reconnect(node);
					bound += 3 * relays as Metric;
					restarts += 1;
				}
				_ => {
					let busy = sim.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						sim.deliver(from, to);
					}
				}
			}
			check(&sim, step, bound);
		}
		assert!(sim.run(), "case {case} never settles");
		check(&sim, usize::MAX, bound);

		// Quiet: a relay the origin still reaches routes to it and shows its claim, and
		// every other one holds nothing. Nothing is left in doubt.
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
			// A scoped link files the origin under another key past it.
			if scoped {
				continue;
			}
			match reached.contains(&node) {
				true => {
					assert!(sim.routed(node, 7), "case {case}: node {node} lost its route");
					assert_eq!(sim.shows(node, 7, ""), Some(true), "case {case}: node {node}");
				}
				false => {
					assert!(!sim.holds(node, 7), "case {case}: node {node} holds a dead route");
					assert_eq!(sim.shows(node, 7, ""), None, "case {case}: node {node}");
				}
			}
		}
		assert_eq!(sim.pending(), 0, "case {case}");
	}
	assert!(
		flaps > 0 && collected > 0 && cut > 0 && restarts > 0,
		"{flaps} flaps, {collected} collects, {cut} cut, {restarts} restarts"
	);
}

/// A relay restarts with no memory. Its neighbours hold the claim, so their readers
/// see it unavailable first, never gone at once. A hold ends once the neighbours that
/// learned the route answered, which can come before the relay's new route does; that
/// route is taken once the doubt on the relay lifts.
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
	originate(&mut sim, origin, 7);
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
		assert_eq!(seen.first(), Some(&Kind::Unavailable), "node {node}: {seen:?}");
		assert_eq!(sim.shows(node, 7, ""), Some(true), "node {node}: {seen:?}");
	}
}

//! What a route per scope keeps: [`path::Router`] on meshes where neighbours ask for
//! different prefixes, and how it compares with [`Router`] on the same scripts.

use super::path::meet;
use super::*;

fn offered(via: Id, session: u64) -> path::Reach {
	path::Reach::Finite(path::Offer {
		path: [path::Hop { node: 7, session }, path::Hop { node: via, session: 1 }].into(),
		draining: false,
		priority: i8::MAX,
		metric: 1,
		down_only: false,
		rtt: None,
	})
}

/// An interest edit keeps the newer withdrawal a narrow view already learned,
/// even while a wider neighbour has not heard it and the new streams have not arrived.
#[test]
fn narrowing_keeps_the_newest_covering_withdrawal() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	for peer in [1, 2] {
		router.connect(peer, peer as Id, Link::default()).unwrap();
	}
	router.route(1, 7, p(""), Some(offered(1, 0)), 0);
	router.batch(1, 7, p(""), vec![(p("a/b/x"), Some(1))], Some(1), 0);
	router.route(2, 7, p("a"), Some(offered(2, 0)), 0);
	router.batch(2, 7, p("a"), Vec::new(), Some(3), 0);
	assert!(router.events().iter().any(|event| event.kind == Kind::End));
	assert!(!router.shown().contains_key(&(7, p("a/b/x"))));

	router.watch(p("a/b"));
	assert!(!router.shown().contains_key(&(7, p("a/b/x"))));
	assert!(router.events().iter().all(|event| event.kind != Kind::Start));
}

/// Another report for an expired hop retires its last heard copy without
/// restarting the hop's timer.
#[test]
fn repeating_an_expired_dead_hop_does_not_restart_its_timer() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	let hop = path::Hop { node: 4, session: 1 };
	for peer in [1, 2] {
		router.connect(peer, peer as Id, Link::default()).unwrap();
		let path::Reach::Finite(mut offer) = offered(peer as Id, 0) else {
			unreachable!()
		};
		offer.path = [
			path::Hop { node: 7, session: 0 },
			path::Hop { node: 6, session: 1 },
			hop,
			path::Hop {
				node: peer as Id,
				session: 1,
			},
		]
		.into();
		router.route(peer, 7, p(""), Some(path::Reach::Finite(offer)), 0);
	}
	let dead = path::Dead::Closed {
		hop,
		far: 6,
		moved: Vec::new(),
		gone: true,
	};
	router.dead(1, dead.clone(), Instant::ZERO);
	let expired = Instant::ZERO.saturating_add(Doubt::DEFAULT.timer);
	router.timeout(expired);
	assert_eq!(router.retained().dead, 1, "the second peer still names the hop");
	router.dead(2, dead, expired);
	assert_eq!(router.retained().dead, 0, "the expired hop is no longer named");
}

#[test]
fn losing_a_wide_route_does_not_forget_a_kept_narrow_view() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	for peer in [1, 2] {
		router.connect(peer, peer as Id, Link::default()).unwrap();
	}
	router.route(1, 7, p(""), Some(offered(1, 0)), 0);
	router.batch(1, 7, p(""), vec![(p("a/x"), Some(1))], Some(1), 0);
	router.route(2, 7, p("a"), Some(offered(2, 0)), 0);
	router.batch(2, 7, p("a"), Vec::new(), Some(3), 0);
	let ended = router
		.events()
		.into_iter()
		.find(|event| event.kind == Kind::End)
		.unwrap();
	router.route(1, 7, p(""), None, 0);
	router.batch(2, 7, p("a"), vec![(p("a/x"), Some(2))], Some(4), 0);
	let started = router
		.events()
		.into_iter()
		.find(|event| event.kind == Kind::Start)
		.unwrap();
	assert_eq!(started.forgot, ended.forgot, "the narrow view kept the withdrawal");
}

#[test]
fn flapping_a_far_node_does_not_extend_older_dead_hops() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	router.connect(1, 1, Link::default()).unwrap();
	let mut now = Instant::ZERO;
	let retained = (Doubt::DEFAULT.timer.as_nanos() / HOP.as_nanos()) as usize + 1;
	for session in 0..3 * retained {
		let path::Reach::Finite(mut offer) = offered(1, 0) else {
			unreachable!()
		};
		offer.path = [
			path::Hop { node: 7, session: 0 },
			path::Hop {
				node: 2,
				session: session as u64,
			},
			path::Hop { node: 1, session: 1 },
		]
		.into();
		router.route(1, 7, p(""), Some(path::Reach::Finite(offer)), 0);
		router.dead(
			1,
			path::Dead::Closed {
				hop: path::Hop {
					node: 2,
					session: session as u64,
				},
				far: 7,
				moved: Vec::new(),
				gone: true,
			},
			now,
		);
		router.timeout(now);
		assert!(
			router.retained().dead <= retained,
			"dead hop {session} extended older timers"
		);
		now = now.saturating_add(HOP);
	}
}

#[test]
fn a_dead_repath_withdraws_its_claims_before_ending_the_sent_route() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	for peer in [1, 2, 3] {
		router.connect(peer, peer as Id, Link::default()).unwrap();
	}
	let path::Reach::Finite(mut old) = offered(1, 0) else {
		unreachable!()
	};
	old.path = [
		path::Hop { node: 7, session: 0 },
		path::Hop { node: 6, session: 1 },
		path::Hop { node: 4, session: 1 },
		path::Hop { node: 1, session: 1 },
	]
	.into();
	for prefix in ["", "a"] {
		router.route(1, 7, p(prefix), Some(path::Reach::Finite(old.clone())), 0);
		router.batch(1, 7, p(prefix), vec![(p("a/x"), Some(1))], Some(1), 0);
	}
	router.peer_interest(3, Some(p("a"))).unwrap();
	// A child still names this relay, so its newer empty view is not believed yet.
	let mut child = old;
	child.path = [
		path::Hop { node: 7, session: 0 },
		path::Hop { node: 6, session: 1 },
		path::Hop { node: 4, session: 1 },
		path::Hop { node: 99, session: 1 },
		path::Hop { node: 2, session: 1 },
	]
	.into();
	router.route(2, 7, p("a"), Some(path::Reach::Finite(child)), 0);
	router.batch(2, 7, p("a"), Vec::new(), Some(3), 0);
	router.drain();
	let path::Reach::Finite(offer) = offered(2, 1) else {
		unreachable!()
	};
	router.dead(
		2,
		path::Dead::Closed {
			hop: path::Hop { node: 4, session: 1 },
			far: 6,
			moved: vec![path::Moved {
				id: 7,
				prefix: p("a"),
				offer,
			}],
			gone: false,
		},
		Instant::ZERO,
	);
	let messages = router.drain();
	let withdrawal = messages.iter().position(|message| {
		matches!(message,
			path::Out::Batch { to: 3, prefix, changes, floor: Some(3), .. }
			if *prefix == p("a") && changes.contains(&(p("a/x"), None))
		)
	});
	assert!(
		withdrawal.is_some(),
		"the withdrawing floor must precede END: {messages:?}"
	);
	assert!(messages[..withdrawal.unwrap()].iter().all(|message| !matches!(
		message,
		path::Out::Dead { to: 3, .. } | path::Out::Route { to: 3, reach: None, .. }
	)));
}

/// A follower holds a route per prefix for one key, each through its own relay,
/// as a viewer session holding many announce prefixes does: one origin's claims
/// reach `prefixes` relays, each admitted under a grant scoped to its own prefix.
fn follower(prefixes: usize) -> (Sim, Node) {
	let (origin, hub, follower) = (0, 1, 2 + prefixes);
	let mut sim = Sim::with(Rule::PATH, prefixes + 3);
	sim.leaf(origin);
	sim.leaf(follower);
	sim.join(origin, hub, 1);
	for spoke in 0..prefixes {
		let scope: &'static str = Box::leak(format!("live/{spoke}").into_boxed_str());
		let grant = Grant {
			subject: "spokes",
			scope,
		};
		sim.link(2 + spoke, hub, SimLink::cost(1).grant(grant), SimLink::cost(1));
		sim.join(2 + spoke, follower, 1);
	}
	sim.originate(origin, 7, 0);
	for spoke in 0..prefixes {
		sim.announce(origin, &format!("live/{spoke}/x"), 0, None);
	}
	assert!(sim.run());
	let id = qualified("spokes", 7);
	assert_eq!(sim.path(follower).routes(id).count(), prefixes, "a route per prefix");
	assert_eq!(sim.shown(follower).len(), prefixes);
	(sim, follower)
}

/// The routes bench's `teardown` and `refollow` cells on the path rule: a follower
/// closing every session at once, and closing one prefix's session and opening it
/// again, cost about what they held. Twice the prefixes visit about twice the routes,
/// prefixes, and streams.
#[test]
fn teardown_is_linear_in_prefixes_held() {
	let teardown = |prefixes: usize| {
		let (mut sim, follower) = follower(prefixes);
		let before = sim.work();
		sim.isolate(follower);
		assert!(sim.run());
		assert!(sim.shown(follower).is_empty());
		sim.work() - before
	};
	let refollow = |prefixes: usize| {
		let (mut sim, follower) = follower(prefixes);
		let before = sim.work();
		sim.disconnect(2, follower);
		sim.connect(2, follower);
		assert!(sim.run());
		assert_eq!(sim.shown(follower).len(), prefixes);
		sim.work() - before
	};
	for (name, cost) in [
		("teardown", &teardown as &dyn Fn(usize) -> usize),
		("refollow", &refollow),
	] {
		let costs: Vec<usize> = [8, 16, 32].into_iter().map(cost).collect();
		for pair in costs.windows(2) {
			// Quadratic work would quadruple.
			assert!(pair[1] <= 5 * pair[0] / 2, "{name}: {costs:?}");
		}
	}
}

/// Adding an unrelated scope visits the same number of routes and reader claims,
/// regardless of how many sibling scopes this publisher already has.
#[test]
fn following_a_prefix_does_not_revisit_its_siblings() {
	for claims in [1, 16] {
		let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
		let mut first = None;
		for scope in 0..64 {
			let before = router.work();
			let prefix = p(&format!("live/{scope}"));
			router.connect(scope, 1, Link::default()).unwrap();
			router.route(scope, 7, prefix.clone(), Some(offered(1, 0)), 0);
			let changes = (0..claims)
				.map(|claim| (p(&format!("live/{scope}/{claim}")), Some(1)))
				.collect();
			router.batch(scope, 7, prefix, changes, Some(1), 0);
			let work = router.work() - before;
			let expected = *first.get_or_insert(work);
			assert_eq!(
				work, expected,
				"scope {scope}, {claims} claims: sibling scopes were revisited"
			);
			router.events();
			router.changed();
			router.drain();
		}
	}
}

const CLAIMS: [&str; 6] = ["a/x", "a/y/", "a/", "b/x", "b/y/z", ""];
const WATCHES: [&str; 5] = ["", "", "a/", "b/", "a/x"];
const SCOPES: [&str; 2] = ["a/", "b/"];
const SUBJECTS: [&str; 2] = ["ga", "gb"];

/// Two grants qualify the same raw key. A raw route learned from us through
/// either grant is not a child of the qualified route we advertised separately.
#[test]
fn another_grant_does_not_turn_a_raw_route_into_a_qualified_child() {
	let (relay, first, second, origin) = (0, 1, 2, 3);
	let mut sim = Sim::with(Rule::PATH, 4);
	for peer in [first, second] {
		sim.link(
			relay,
			peer,
			SimLink::cost(2).grant(Grant {
				subject: "ga",
				scope: "b",
			}),
			SimLink::cost(2),
		);
	}
	sim.join(origin, relay, 2);
	sim.join(origin, second, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "b/x", 0, None);
	assert!(sim.run());
	let key = qualified("ga", 7);
	assert_eq!(sim.reaches(relay, key, &p("b/x")), Some(origin));
	sim.disconnect(origin, second);
	assert!(sim.run());
	assert_eq!(sim.pending(), 0, "{}", mesh(&sim));
	assert_eq!(sim.shows(relay, key, "b/x"), None);
	assert_eq!(sim.shows(first, key, "b/x"), None);
	assert_eq!(sim.reaches(relay, 7, &p("b/x")), Some(origin));
}

#[test]
fn widening_past_a_childs_narrower_interest_releases_the_old_scope() {
	let (origin, primary, relay, child) = (0, 1, 2, 3);
	let mut sim = Sim::with(Rule::PATH, 4);
	sim.join(origin, primary, 1);
	sim.join(primary, relay, 1);
	sim.join(relay, child, 1);
	sim.watch(relay, "a");
	sim.watch(child, "a/x");
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	sim.announce(origin, "a/y", 0, None);
	assert!(sim.run());
	sim.watch(relay, "");
	assert!(sim.run());
	assert!(
		sim.path(relay).routes(7).all(|(_, route)| route.offer().is_some()),
		"{}",
		mesh(&sim)
	);
	assert_eq!(sim.reaches(child, 7, &p("a/x")), Some(origin));
}

#[test]
fn overlapping_scopes_do_not_oscillate_between_covering_paths() {
	let mut sim = Sim::with(Rule::PATH, 6);
	for (a, b, cost) in [
		(5, 0, 1),
		(0, 1, 3),
		(0, 2, 2),
		(1, 3, 3),
		(1, 4, 1),
		(2, 3, 2),
		(2, 4, 3),
		(3, 4, 2),
	] {
		sim.join(a, b, cost);
	}
	for node in [2, 3, 4] {
		sim.watch(node, "b");
	}
	sim.originate(5, 7, 0);
	sim.announce(5, "b/x", 0, None);
	assert!(sim.run(), "{}", mesh(&sim));
	for node in 0..5 {
		assert_eq!(sim.reaches(node, 7, &p("b/x")), Some(5));
	}
}

/// Retiring a narrow stream must not leave its former route circulating while
/// another neighbour still asks for that prefix.
#[test]
fn widening_a_relay_does_not_oscillate_its_former_prefix() {
	let mut sim = Sim::with(Rule::PATH, 5);
	sim.join(0, 1, 2);
	sim.join(0, 3, 2);
	sim.join(1, 3, 3);
	sim.join(1, 2, 3);
	sim.join(2, 3, 3);
	sim.leaf(4);
	sim.join(4, 0, 1);
	sim.join(4, 1, 1);
	sim.watch(0, "a");
	sim.watch(2, "a");
	sim.originate(4, 7, 0);
	sim.announce(4, "a/x", 0, None);
	assert!(sim.run());

	sim.watch(0, "");
	assert!(sim.run(), "{}", mesh(&sim));
	quiet(&sim, 4, "widened");
}

/// A DEAD that repaths a scoped route keeps the publisher's original key for
/// forwarding: the neighbour admitted under the grant never hears the qualified key.
#[test]
fn a_repathed_scoped_route_keeps_its_original_key() {
	let (origin, primary, backup, receiver, near) = (0, 1, 2, 3, 4);
	let mut sim = Sim::with(Rule::PATH, 5);
	sim.leaf(origin);
	sim.join(origin, near, 1);
	sim.join(near, primary, 1);
	sim.join(origin, backup, 3);
	sim.join(primary, backup, 1);
	sim.link(
		receiver,
		primary,
		SimLink::cost(1).grant(Grant {
			subject: "ga",
			scope: "a",
		}),
		SimLink::cost(1),
	);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	assert!(sim.run());
	let key = qualified("ga", 7);
	assert_eq!(sim.reaches(receiver, key, &p("a/x")), Some(origin));
	sim.disconnect(near, primary);
	assert!(sim.run());
	assert_eq!(sim.shows(receiver, key, "a/x"), Some(true));
	assert_eq!(sim.reaches(receiver, key, &p("a/x")), Some(origin));
}

/// A qualified route switches onto a raw route from its former child. The raw
/// route is a different key: the child cannot answer a hold for the qualified one.
#[test]
fn qualification_does_not_turn_another_key_into_a_child() {
	let (origin, raw, relay, alternate) = (0, 1, 2, 3);
	let grant = Grant {
		subject: "ga",
		scope: "a",
	};
	let mut sim = Sim::with(Rule::PATH, 4);
	sim.leaf(origin);
	sim.join(origin, raw, 1);
	sim.link(relay, raw, SimLink::cost(4).grant(grant), SimLink::cost(1));
	sim.link(alternate, origin, SimLink::cost(1).grant(grant), SimLink::cost(1));
	sim.join(alternate, relay, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	assert!(sim.run());
	let key = qualified("ga", 7);
	assert_eq!(sim.reaches(raw, key, &p("a/x")), Some(origin));
	sim.disconnect(alternate, relay);
	assert!(sim.run());
	assert_eq!(sim.shows(raw, key, "a/x"), None);
	assert!(!sim.holds(raw, key));
	assert_eq!(sim.reaches(relay, key, &p("a/x")), Some(origin));
}

/// Cancelling the only stream for a grant has no new stream whose SYNCED could
/// retire its old routes. The local cancellation retires them instead.
#[test]
fn an_unwatched_grant_retires_its_previous_stream() {
	let mut sim = Sim::with(Rule::PATH, 2);
	sim.link(
		1,
		0,
		SimLink::cost(1).grant(Grant {
			subject: "ga",
			scope: "b",
		}),
		SimLink::cost(1),
	);
	sim.originate(0, 7, 0);
	sim.announce(0, "b/x", 0, None);
	assert!(sim.run());
	let key = qualified("ga", 7);
	assert_eq!(sim.shows(1, key, "b/x"), Some(true));
	sim.watch(1, "a");
	assert!(sim.run());
	assert!(!sim.holds(1, key));
	assert!(sim.shown(1).is_empty());
	sim.watch(1, "b");
	assert!(sim.run());
	assert_eq!(sim.reaches(1, key, &p("b/x")), Some(0));
}

/// A snapshot already in flight must not restore a cancelled grant's routes.
#[test]
fn a_cancelled_stream_cannot_restore_a_route() {
	let mut sim = Sim::with(Rule::PATH, 2);
	sim.link(
		1,
		0,
		SimLink::cost(1).grant(Grant {
			subject: "ga",
			scope: "b",
		}),
		SimLink::cost(1),
	);
	sim.originate(0, 7, 0);
	sim.announce(0, "b/x", 0, None);
	// The original snapshot is still in flight when the grant is cancelled.
	sim.watch(1, "a");
	assert!(sim.run());
	assert!(!sim.holds(1, qualified("ga", 7)), "{}", sim.path(1).dump());
	assert!(sim.shown(1).is_empty());
	sim.watch(1, "b");
	assert!(sim.run());
	assert_eq!(sim.reaches(1, qualified("ga", 7), &p("b/x")), Some(0));
}

/// The regions of each key that reach each node from `origin` along a path of
/// sessions that are up and visit no node twice: each session passes the meet of
/// what the receiver asks for and the scope its grant gives the sender, and files
/// it under the key that grant qualifies, refusing one already qualified.
fn reachable(sim: &Sim, origin: Node, id: Id) -> Vec<BTreeSet<(Id, Path)>> {
	fn visit(sim: &Sim, at: Node, key: Id, region: Path, seen: &mut Vec<Node>, out: &mut [BTreeSet<(Id, Path)>]) {
		out[at].insert((key, region.clone()));
		seen.push(at);
		for to in 0..out.len() {
			let Some(side) = sim.links[to][at] else {
				continue;
			};
			if seen.contains(&to) || !sim.connected(at, to) {
				continue;
			}
			let ask = match side.grant {
				None => Some(sim.watching(to).clone()),
				Some(grant) => meet(sim.watching(to), &p(grant.scope)),
			};
			let Some(region) = ask.and_then(|ask| meet(&region, &ask)) else {
				continue;
			};
			let key = match side.grant {
				None => key,
				Some(_) if key & QUALIFIED != 0 => continue,
				Some(grant) => qualified(grant.subject, key),
			};
			visit(sim, to, key, region, seen, out);
		}
		seen.pop();
	}
	let mut out = vec![BTreeSet::new(); sim.nodes.len()];
	visit(sim, origin, id, Path::empty(), &mut Vec::new(), &mut out);
	out
}

/// The sessions up, each side's grant, and each node's interest, for a failure report.
fn mesh(sim: &Sim) -> String {
	let mut out = String::new();
	for ((from, to), queue) in &sim.queues {
		if !queue.is_empty() {
			out += &format!("  {from} -> {to}: {} queued, next {:?}\n", queue.len(), queue.front());
		}
	}
	for a in 0..sim.nodes.len() {
		out += &format!("  node {a} watches {:?}\n", sim.watching(a));
		out += &sim.path(a).dump();
		for b in 0..sim.nodes.len() {
			if let Some(side) = sim.links[a][b] {
				let up = if sim.connected(a, b) { "up" } else { "down" };
				out += &format!("    from {b}: {up} cost {} grant {:?}\n", side.cost, side.grant);
			}
		}
	}
	out
}

/// The claims of `id` presented at each of `regions` and not inside a wider one.
fn presenting(sim: &Sim, id: Id, regions: impl IntoIterator<Item = Path>) -> BTreeMap<Path, Item> {
	let regions: BTreeSet<Path> = regions.into_iter().collect();
	let rev = sim.revision(id);
	(regions.iter())
		.filter(|region| !regions.iter().any(|wider| wider != *region && within(region, wider)))
		.flat_map(|region| sim.presented(id, region, rev))
		.collect()
}

/// At a quiet point each relay shows exactly what its live routes cover, as the
/// publisher now has it. Each route is the end of a real loop-free path, and a relay
/// shows every claim some such path brings it, under one key or another. A request
/// for each claim reaches the publisher, nothing is held or waits, and a relay keeps
/// nothing of a key no path brings it.
fn quiet(sim: &Sim, origin: Node, label: &str) {
	sim.check().unwrap_or_else(|err| panic!("{label}: {err}"));
	assert_eq!(sim.pending(), 0, "{label}: something waits\n{}", mesh(sim));
	let reach = reachable(sim, origin, 7);
	let keys: Vec<Id> = std::iter::once(7)
		.chain(SUBJECTS.iter().map(|subject| qualified(subject, 7)))
		.collect();
	for node in (0..sim.nodes.len()).filter(|&node| node != origin) {
		let router = sim.path(node);
		let mut expected = BTreeMap::new();
		for &key in &keys {
			let routes: Vec<(&Path, &path::Route<Node>)> = router.routes(key).collect();
			assert!(
				routes.iter().all(|(_, route)| route.offer().is_some()),
				"{label}: node {node} holds {key}\n{}{}",
				router.dump(),
				mesh(sim)
			);
			for (prefix, _) in &routes {
				assert!(
					(reach[node].iter()).any(|(reached, region)| *reached == key && within(prefix, region)),
					"{label}: node {node} routes {key} {prefix:?}, which no path brings it\n{}{}reach {:?}",
					router.dump(),
					mesh(sim),
					reach[node]
				);
			}
			let kept = reach[node].iter().any(|(reached, _)| *reached == key);
			// A neighbour may offer our own path back under a qualified key. It is
			// retained to answer holds but cannot feed a route or a claim view here.
			assert!(
				kept || routes.is_empty(),
				"{label}: node {node} routes {key}\n{}",
				router.dump()
			);
			assert!(
				kept || router.views().all(|((id, _), _)| *id != key),
				"{label}: node {node} keeps a view of {key}\n{}",
				router.dump()
			);
			for (claim, item) in presenting(sim, 7, routes.into_iter().map(|(prefix, _)| prefix.clone())) {
				expected.insert((key, claim), item);
			}
		}
		let shown: BTreeMap<(Id, Path), Item> = (sim.shown(node).iter())
			.map(|(key, shown)| {
				assert!(shown.available, "{label}: node {node} {key:?} unavailable");
				(key.clone(), shown.item)
			})
			.collect();
		assert_eq!(shown, expected, "{label}: node {node}\n{}", router.dump());
		let claims: BTreeSet<&Path> = shown.keys().map(|(_, claim)| claim).collect();
		let paths = presenting(sim, 7, reach[node].iter().map(|(_, region)| region.clone()));
		for claim in paths.keys() {
			assert!(
				claims.contains(claim),
				"{label}: node {node} shows no {claim:?} though a path brings it\n{}",
				router.dump()
			);
		}
		for (key, claim) in shown.keys() {
			assert_eq!(
				sim.reaches(node, *key, claim),
				Some(origin),
				"{label}: node {node} {key} {claim:?}"
			);
		}
	}
}

/// The review harness's scoped and two-prefix families on the path rule, beside
/// `sweep_no_route_outgrows_a_loop_free_path`: random meshes of three to five relays
/// and a publisher whose claims sit under two prefixes. In one family some sessions
/// publish under a grant scoped to one prefix, in another some relays ask for one
/// prefix, and the third has both. Links flap, the publisher's most, relays restart,
/// claims and interests change, doubt timers fire, and every message arrives in a
/// random order. Loops are only ever stale and every view is some revision of the
/// publisher's truth; at every quiet point each relay shows what the rule says and
/// nothing waits.
#[test]
fn sweep_scoped_meshes_show_what_the_rule_says() {
	let mut rng = XorShift(0x5c0b_e2ed);
	let (mut quiets, mut qualified_shown, mut narrowed_shown, mut cut) = (0, 0, 0, 0);
	for scenario in 0..scenarios() {
		let relays = 3 + rng.next(3) as usize;
		let origin = relays;
		let scoped = scenario % 3 != 1;
		let narrowed = scenario % 3 != 0;
		let mut sim = Sim::with(Rule::PATH, relays + 1);
		let mut links = Vec::new();
		for a in 0..relays {
			for b in a + 1..relays {
				if b == a + 1 || rng.chance(50) {
					let side = SimLink::cost(1 + rng.next(3));
					let grant = |rng: &mut XorShift| Grant {
						subject: SUBJECTS[rng.next(2) as usize],
						scope: SCOPES[rng.next(2) as usize],
					};
					match (scoped && rng.chance(30), scoped && rng.chance(30)) {
						(false, false) => sim.link(a, b, side, side),
						(true, false) => sim.link(a, b, side.grant(grant(&mut rng)), side),
						(false, true) => sim.link(a, b, side, side.grant(grant(&mut rng))),
						(true, true) => {
							let (ab, ba) = (grant(&mut rng), grant(&mut rng));
							sim.link(a, b, side.grant(ab), side.grant(ba))
						}
					}
					links.push((a, b));
				}
			}
		}
		for _ in 0..1 + rng.next(2) {
			let relay = rng.next(relays as u64) as usize;
			if !sim.connected(origin, relay) {
				sim.join(origin, relay, 1 + rng.next(2));
				links.insert(0, (origin, relay));
			}
		}
		if narrowed {
			for relay in 0..relays {
				if rng.chance(40) {
					sim.watch(relay, WATCHES[rng.next(WATCHES.len() as u64) as usize]);
				}
			}
		}
		sim.originate(origin, 7, 0);
		for claim in CLAIMS {
			if rng.chance(40) {
				sim.announce(origin, claim, rng.next(3), None);
			}
		}

		let mut quiet_at = |sim: &mut Sim, step: usize| {
			assert!(
				sim.run(),
				"scenario {scenario} step {step} never settles\n{}",
				mesh(sim)
			);
			quiet(sim, origin, &format!("scenario {scenario} step {step}"));
			quiets += 1;
			for node in 0..relays {
				let shown = sim.shown(node);
				qualified_shown += usize::from(shown.keys().any(|(key, _)| key & QUALIFIED != 0));
				narrowed_shown += usize::from(!sim.watching(node).is_empty() && !shown.is_empty());
				cut += usize::from(shown.is_empty());
			}
		};
		for step in 0..120 {
			match rng.next(24) {
				0..=2 => {
					let claim = CLAIMS[rng.next(CLAIMS.len() as u64) as usize];
					match rng.chance(60) {
						true => sim.announce(origin, claim, rng.next(3), None),
						false => sim.unannounce(origin, claim),
					}
				}
				3 if narrowed => {
					let relay = rng.next(relays as u64) as usize;
					sim.watch(relay, WATCHES[rng.next(WATCHES.len() as u64) as usize]);
				}
				4 | 5 => {
					let pick = match rng.chance(50) {
						true => 0,
						false => rng.next(links.len() as u64) as usize,
					};
					let (a, b) = links[pick];
					match sim.connected(a, b) {
						true => sim.disconnect(a, b),
						false => sim.connect(a, b),
					}
				}
				6 if rng.chance(25) => {
					let relay = rng.next(relays as u64) as usize;
					sim.kill(relay);
					sim.reconnect(relay);
				}
				7 => sim.wait(HOP),
				8 => quiet_at(&mut sim, step),
				_ => {
					let busy = sim.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						sim.deliver(from, to);
					}
				}
			}
			sim.check()
				.unwrap_or_else(|err| panic!("scenario {scenario} step {step}: {err}"));
			assert_loop_free(&sim, relays + 1, 7);
		}
		quiet_at(&mut sim, usize::MAX);
	}
	assert!(
		quiets > 0 && qualified_shown > 0 && narrowed_shown > 0 && cut > 0,
		"{quiets} quiet points, {qualified_shown} qualified, {narrowed_shown} narrowed, {cut} cut"
	);
}

/// Relays 0 and 1 ask for "a/" alone, each nearer the other's choice of open relay.
/// Relay 3 takes its narrow route from 0, which answers it from 4's covering route,
/// and 4 takes its narrow route from 1, which answers it from 3's. When the
/// publisher's session closes and the covering routes end, each held stream would
/// fall back to the narrow route, on another path, and the holds would wait on
/// each other for good. A stream holds only the path it last sent, so they end.
#[test]
fn holds_never_close_a_cycle() {
	let (origin, hub) = (5, 2);
	let mut sim = Sim::with(Rule::PATH, 6);
	for (a, b, cost) in [(hub, 3, 1), (hub, 4, 1), (0, 3, 2), (0, 4, 1), (1, 3, 1), (1, 4, 2)] {
		sim.join(a, b, cost);
	}
	sim.join(origin, hub, 1);
	sim.watch(0, "a/");
	sim.watch(1, "a/");
	sim.originate(origin, 7, 0);
	sim.announce(origin, "a/x", 0, None);
	assert!(sim.run());
	quiet(&sim, origin, "connected");
	for (relay, next) in [(3, 0), (4, 1)] {
		let narrow = sim.path(relay).routes(7).find(|(prefix, _)| **prefix == p("a/"));
		assert_eq!(narrow.and_then(|(_, route)| route.next()), Some(next), "relay {relay}");
	}
	sim.disconnect(origin, hub);
	assert!(sim.run());
	quiet(&sim, origin, "cut off");
	for node in 0..origin {
		assert_eq!(sim.path(node).routes(7).count(), 0, "node {node}\n{}", mesh(&sim));
	}
}

/// A DEAD needs a path from this peer naming both the hop and its far end.
#[test]
fn unrelated_dead_messages_do_not_allocate_or_poison_routes() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	for peer in [1, 2] {
		router.connect(peer, peer as Id, Link::default()).unwrap();
	}
	let now = Instant::ZERO;
	for node in 100..1100 {
		router.dead(
			1,
			path::Dead::Closed {
				hop: path::Hop { node, session: 1 },
				far: node + 1,
				moved: Vec::new(),
				gone: true,
			},
			now,
		);
	}
	assert_eq!((router.retained().dead, router.retained().doubt), (0, 0));
	router.route(1, 7, p(""), Some(offered(1, 0)), 0);
	router.batch(1, 7, p(""), vec![(p("a"), Some(1))], Some(1), 0);
	let hop = path::Hop { node: 1, session: 1 };
	// Another peer has no credit for this hop, even though this node heard it.
	router.dead(
		2,
		path::Dead::Closed {
			hop,
			far: 7,
			moved: Vec::new(),
			gone: true,
		},
		now,
	);
	// Nor can the advertising peer attach arbitrary far nodes to a known hop.
	for far in 100..1100 {
		router.dead(
			1,
			path::Dead::Closed {
				hop,
				far,
				moved: Vec::new(),
				gone: true,
			},
			now,
		);
	}
	assert_eq!((router.retained().dead, router.retained().doubt), (0, 0));
	assert!(router.shown()[&(7, p("a"))].available);
	router.dead(
		1,
		path::Dead::Closed {
			hop,
			far: 7,
			moved: Vec::new(),
			gone: true,
		},
		now,
	);
	assert_eq!(router.retained().dead, 1);
	assert!(router.routes(7).next().is_none());
}

#[test]
fn dead_replacement_paths_obey_the_same_limit_as_route_paths() {
	for length in [0, MAX_HOPS - 1, MAX_HOPS] {
		let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
		router.connect(1, 1, Link::default()).unwrap();
		router.route(1, 7, p(""), Some(offered(1, 0)), 0);
		router.batch(1, 7, p(""), vec![(p("a"), Some(1))], Some(1), 0);
		let offer = path::Offer {
			path: (0..length)
				.map(|i| path::Hop {
					node: 100 + i as Id,
					session: 1,
				})
				.collect(),
			draining: false,
			priority: i8::MAX,
			metric: 1,
			down_only: false,
			rtt: None,
		};
		router.dead(
			1,
			path::Dead::Closed {
				hop: path::Hop { node: 1, session: 1 },
				far: 7,
				moved: vec![path::Moved {
					id: 7,
					prefix: p(""),
					offer,
				}],
				gone: true,
			},
			Instant::ZERO,
		);
		assert_eq!(
			router.routes(7).any(|(_, route)| route.offer().is_some()),
			length == MAX_HOPS - 1,
			"replacement length {length}"
		);
		if length == MAX_HOPS - 1 {
			assert_eq!(router.routes(7).next().unwrap().1.offer().unwrap().path.len(), MAX_HOPS);
		}
	}
}

#[test]
fn an_upstream_stream_uses_a_narrower_exportable_source() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	for peer in [1, 2, 3] {
		router
			.connect(
				peer,
				peer as Id,
				Link {
					upstream: peer != 2,
					..Link::default()
				},
			)
			.unwrap();
	}
	router.route(1, 7, p(""), Some(offered(1, 0)), 0);
	router.batch(1, 7, p(""), vec![(p("a/x"), Some(1))], Some(1), 0);
	let path::Reach::Finite(mut narrow) = offered(2, 0) else {
		unreachable!()
	};
	narrow.metric = 2;
	router.route(2, 7, p("a"), Some(path::Reach::Finite(narrow)), 0);
	router.peer_interest(3, Some(p("a"))).unwrap();
	assert!(router.drain().iter().any(|msg| matches!(msg, path::Out::Route { to: 3, prefix, reach: Some(path::Reach::Finite(offer)), .. } if *prefix == p("a") && !offer.down_only && offer.path[1].node == 2)));
}

#[test]
fn replacing_a_childs_interest_keeps_the_hold_until_its_answer() {
	for interest in [None, Some(p("a"))] {
		let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
		for peer in [1, 2] {
			router.connect(peer, peer as Id, Link::default()).unwrap();
		}
		router.route(1, 7, p(""), Some(offered(1, 0)), 0);
		router.batch(1, 7, p(""), vec![(p("a/x"), Some(1))], Some(1), 0);
		router.peer_interest(2, Some(p(""))).unwrap();
		let path::Reach::Finite(mut child) = offered(2, 0) else {
			unreachable!()
		};
		child.path = [
			path::Hop { node: 7, session: 0 },
			path::Hop { node: 1, session: 1 },
			path::Hop { node: 99, session: 1 },
			path::Hop { node: 2, session: 1 },
		]
		.into();
		router.route(2, 7, p(""), Some(path::Reach::Finite(child)), 0);
		router.disconnect([1], Instant::ZERO);
		assert!(!router.shown()[&(7, p("a/x"))].available);
		router.events();
		router.peer_interest(2, interest).unwrap();
		assert!(
			!router.shown()[&(7, p("a/x"))].available,
			"the child still owes an answer"
		);
		assert!(router.events().is_empty());
		router.route(2, 7, p(""), Some(offered(2, 0)), 0);
		assert!(router.shown()[&(7, p("a/x"))].available);
		assert!(router.events().iter().all(|event| event.kind == Kind::Available));
	}
}

/// An unusable covering path must not replace a narrow hold on an upstream
/// stream: the child still owes an answer about the path it learned here.
#[test]
fn an_upstream_stream_keeps_a_hold_over_an_unexportable_live_source() {
	for (live, held) in [("", "a"), ("a", "")] {
		let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
		for peer in [1, 2, 3] {
			router
				.connect(
					peer,
					peer as Id,
					Link {
						upstream: peer != 2,
						..Link::default()
					},
				)
				.unwrap();
		}
		router.route(1, 7, p(live), Some(offered(1, 0)), 0);
		router.batch(1, 7, p(live), vec![(p("a/x"), Some(1))], Some(1), 0);
		let path::Reach::Finite(mut offer) = offered(2, 0) else {
			unreachable!()
		};
		offer.metric = 2;
		router.route(2, 7, p(held), Some(path::Reach::Finite(offer)), 0);
		router.batch(2, 7, p(held), vec![(p("a/x"), Some(1))], Some(1), 0);
		router.peer_interest(3, Some(p("a"))).unwrap();
		let path::Reach::Finite(mut child) = offered(3, 0) else {
			unreachable!()
		};
		child.path = [
			path::Hop { node: 7, session: 0 },
			path::Hop { node: 2, session: 1 },
			path::Hop { node: 99, session: 2 },
			path::Hop { node: 3, session: 1 },
		]
		.into();
		router.route(3, 7, p("a"), Some(path::Reach::Finite(child)), 0);
		router.drain();
		router.disconnect([2], Instant::ZERO);
		let messages = router.drain();
		assert!(
			router
				.routes(7)
				.any(|(prefix, route)| *prefix == p(held) && route.offer().is_none()),
			"keep {held:?} while its child answers: {messages:?}"
		);
		assert!(
			messages.iter().any(|msg| matches!(
				msg,
				path::Out::Dead {
					to: 3,
					dead: path::Dead::Closed { gone: false, .. }
				}
			)),
			"the child must hear a hold, not an end: {messages:?}"
		);
		assert!(
			!messages
				.iter()
				.any(|msg| matches!(msg, path::Out::Route { to: 3, reach: None, .. }))
		);
	}
}

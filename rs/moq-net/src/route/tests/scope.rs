//! What a route per scope keeps: [`path::Router`] on meshes where neighbours ask for
//! different prefixes, and how it compares with [`Router`] on the same scripts.

use super::*;

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

const CLAIMS: [&str; 6] = ["a/x", "a/y/", "a/", "b/x", "b/y/z", ""];
const WATCHES: [&str; 5] = ["", "", "a/", "b/", "a/x"];
const SCOPES: [&str; 2] = ["a/", "b/"];
const SUBJECTS: [&str; 2] = ["ga", "gb"];

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

/// The prefix two regions share, or `None` when they are disjoint.
fn meet(a: &Path, b: &Path) -> Option<Path> {
	match (within(a, b), within(b, a)) {
		(true, _) => Some(a.clone()),
		(_, true) => Some(b.clone()),
		_ => None,
	}
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
	assert_eq!(sim.pending(), 0, "{label}: something waits");
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
				routes.iter().all(|(_, route)| route.offer.is_some()),
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
			assert!(sim.run(), "scenario {scenario} step {step} never settles");
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

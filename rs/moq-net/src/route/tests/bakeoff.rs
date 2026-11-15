//! The bake-off: the multicast epic's rule ([`multi::Router`]) beside today's
//! [`path::Router`] on the same meshes and failure schedules.
//!
//! Every arm builds the same [`Mesh`], sees the same failure at the same instant, and
//! is measured by the same [`Lab`]: messages, hops of delay until every relay's
//! request reaches its publisher for good, the relay-rounds a reader went unfed
//! meanwhile, loops among selected routes, and what readers were shown. The ignored
//! benchmarks print the tables; the tests pin what they show.

use std::collections::BTreeSet;

use super::compare::*;
use super::multi::{Suspect, Tie};
use super::*;

/// The arms measured, in the report's order.
fn arms() -> Vec<Arm> {
	vec![
		Arm::TODAY,
		Arm::PLAIN,
		Arm::with(|tune| tune.suspect = Suspect::Flagged),
		Arm::with(|tune| tune.suspect = Suspect::Local),
		Arm::with(|tune| tune.quiet = true),
		Arm::with(|tune| tune.batch = true),
		Arm::with(|tune| tune.agree = true),
		Arm::with(|tune| tune.drain = true),
		Arm::with(|tune| {
			tune.quiet = true;
			tune.batch = true;
			tune.drain = true;
		}),
		Arm::with(|tune| {
			tune.suspect = Suspect::Local;
			tune.quiet = true;
			tune.batch = true;
			tune.drain = true;
		}),
		Arm::TUNED,
	]
}

/// The prototype's arms.
const MULTI: [Arm; 2] = [Arm::PLAIN, Arm::TUNED];

/// How long a silent death takes its neighbours to notice, in hops of delay: an idle
/// timeout of 3 s.
const IDLE: usize = 120;

/// A mesh to measure, and whether today's rule can run it.
struct Shape {
	name: &'static str,
	mesh: Mesh,
	/// Today's rule qualifies an identity across one scoped link and refuses to
	/// again, so a publisher two grants away is never seen.
	today: bool,
}

fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

/// Publishers spread over `relays`, the first on the first.
fn spread(relays: &[Node], publishers: usize) -> Vec<Node> {
	(0..publishers).map(|index| relays[index * 7 % relays.len()]).collect()
}

/// CDN tiers: two cores per region, each region's cores linked to each other and
/// to both cores of the regions beside it, and every edge on both of its region's
/// cores, which it marks as upstream. Edges of one region peer when `peered`.
/// Publishers join edges.
fn tiers(regions: usize, edges: usize, peered: bool, publishers: usize) -> Mesh {
	let core = |region: usize, side: usize| 2 * (region % regions) + side;
	let edge = |region: usize, index: usize| 2 * regions + region * edges + index;
	let mut links = BTreeSet::new();
	let mut ties = BTreeMap::new();
	for region in 0..regions {
		links.insert(pair(core(region, 0), core(region, 1)));
		if regions > 1 {
			for (a, b) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
				links.insert(pair(core(region, a), core(region + 1, b)));
			}
		}
		for index in 0..edges {
			for side in 0..2 {
				// Cores come first, so the pair already orders the edge second.
				links.insert((core(region, side), edge(region, index)));
				ties.insert((edge(region, index), core(region, side)), Tie::Up);
			}
			for other in index + 1..edges {
				if peered {
					links.insert((edge(region, index), edge(region, other)));
				}
			}
		}
	}
	let all: Vec<Node> = (0..regions * edges).map(|index| 2 * regions + index).collect();
	Mesh {
		relays: 2 * regions + regions * edges,
		links: links.into_iter().map(|(a, b)| (a, b, 1)).collect(),
		ties,
		homes: spread(&all, publishers),
		claims: 1,
	}
}

/// A ring with a chord from every relay at each power of two: log N of them.
fn chords(relays: usize, publishers: usize) -> Mesh {
	let mut links = BTreeSet::new();
	let mut step = 1;
	while step <= relays / 2 {
		for a in 0..relays {
			links.insert(pair(a, (a + step) % relays));
		}
		step *= 2;
	}
	Mesh {
		relays,
		links: links.into_iter().filter(|(a, b)| a != b).map(|(a, b)| (a, b, 1)).collect(),
		ties: BTreeMap::new(),
		homes: spread(&(0..relays).collect::<Vec<_>>(), publishers),
		claims: 1,
	}
}

fn sparse(relays: usize, publishers: usize, rng: &mut XorShift) -> Mesh {
	Mesh {
		homes: spread(&(0..relays).collect::<Vec<_>>(), publishers),
		claims: 1,
		..Mesh::sparse(relays, rng)
	}
}

fn clique(relays: usize, publishers: usize) -> Mesh {
	Mesh {
		homes: spread(&(0..relays).collect::<Vec<_>>(), publishers),
		claims: 1,
		..Mesh::clique(relays, 1)
	}
}

/// A chorded ring of `core` relays with `leaves` more, each on two of them and
/// marking both links, like a relay on a camera. Publishers join leaves.
fn leafy(core: usize, leaves: usize, publishers: usize) -> Mesh {
	let mut mesh = chords(core, 1);
	for leaf in 0..leaves {
		for at in [leaf * 3 % core, (leaf * 3 + core / 2) % core] {
			mesh.links.push((at, core + leaf, 1));
			mesh.ties.insert((core + leaf, at), Tie::Up);
		}
	}
	mesh.relays = core + leaves;
	mesh.homes = spread(&(core..core + leaves).collect::<Vec<_>>(), publishers);
	mesh
}

/// `count` chorded rings of `size` relays, each joined to the next by `joins`
/// scoped links, and the last to the first when `ring`. Publishers join the first.
fn joined(count: usize, size: usize, joins: usize, ring: bool, publishers: usize) -> Mesh {
	let one = chords(size, 1);
	let mut mesh = Mesh {
		relays: count * size,
		links: Vec::new(),
		ties: BTreeMap::new(),
		homes: spread(&(0..size).collect::<Vec<_>>(), publishers),
		claims: 1,
	};
	for at in 0..count {
		(mesh.links).extend(one.links.iter().map(|&(a, b, cost)| (at * size + a, at * size + b, cost)));
		if at + 1 == count && !(ring && count > 2) {
			continue;
		}
		let next = (at + 1) % count;
		for join in 0..joins {
			// Each boundary leaves a mesh at another relay than the one it entered by.
			let (a, b) = (at * size + size / 2 + join, next * size + 1 + join);
			mesh.links.push((a, b, 1));
			mesh.ties.insert((a, b), Tie::Scoped);
		}
	}
	mesh
}

/// Every shape, at about `relays` relays each.
fn shapes(relays: usize, publishers: usize) -> Vec<Shape> {
	let mut rng = XorShift(0x6d6f_7100 + relays as u64);
	let open = |name, mesh| Shape {
		name,
		mesh,
		today: true,
	};
	let (regions, edges) = match relays {
		..12 => (2, 2),
		12..24 => (3, 3),
		24..48 => (4, 6),
		_ => (6, 8),
	};
	vec![
		open("tiers", tiers(regions, edges, true, publishers)),
		open("tiers_unpeered", tiers(regions, edges, false, publishers)),
		open("chords", chords(relays, publishers)),
		open("sparse", sparse(relays, publishers, &mut rng)),
		open("clique", clique(relays, publishers)),
		open("leaves", leafy(relays / 2, relays - relays / 2, publishers)),
		open("two_meshes_one_grant", joined(2, relays / 2, 1, false, publishers)),
		open("two_meshes_two_grants", joined(2, relays / 2, 2, false, publishers)),
		Shape {
			name: "three_meshes_chain",
			mesh: joined(3, relays / 3, 1, false, publishers),
			today: false,
		},
		Shape {
			name: "three_meshes_ring",
			mesh: joined(3, relays / 3, 1, true, publishers),
			today: false,
		},
	]
}

/// The failures measured, each from a settled mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Failure {
	/// A link relays route over closes cleanly, and every relay still has a path.
	Close,
	/// It returns.
	Reopen,
	/// It closes and reopens eight times, a hop of delay apart: faster than any timer.
	Flap,
	/// It and a link sharing no relay with it close at the same instant.
	Twin,
	/// Every link between the far half of the mesh and the publisher's half closes.
	Partition,
	/// They return.
	Heal,
	/// The relay the most others route through dies, noticed a hop of delay apart.
	Death,
	/// It restarts.
	Restart,
	/// The same death, noticed over an idle timeout.
	SlowDeath,
	/// The relay with the most neighbours dies, noticed over an idle timeout.
	HubDeath,
	/// A publisher on two relays loses the session to one.
	LoseOne,
	/// Then to the other.
	LoseBoth,
	/// A publisher's session closes and it joins a neighbouring relay at once.
	Move,
	/// The relay the most others route through sends GOAWAY.
	Goaway,
	/// Then closes every session.
	Drained,
	/// The publisher's own relay dies.
	Home,
	/// The publisher's session closes.
	Leave,
}

impl Failure {
	fn as_str(self) -> &'static str {
		match self {
			Self::Close => "link_closes",
			Self::Reopen => "link_reopens",
			Self::Flap => "link_flaps",
			Self::Twin => "two_links_close_at_once",
			Self::Partition => "partition",
			Self::Heal => "partition_heals",
			Self::Death => "transit_dies",
			Self::Restart => "transit_restarts",
			Self::SlowDeath => "transit_dies_slowly",
			Self::HubDeath => "hub_dies_slowly",
			Self::LoseOne => "two_homes_lose_one",
			Self::LoseBoth => "two_homes_lose_both",
			Self::Move => "publisher_moves",
			Self::Goaway => "transit_goaway",
			Self::Drained => "transit_drained",
			Self::Home => "home_dies",
			Self::Leave => "publisher_leaves",
		}
	}
}

/// What one failure cost and showed.
#[derive(Clone, Debug)]
struct Row {
	cost: Cost,
	faults: Faults,
	/// Dead hops, doubts, and suspected nodes left once it settled.
	pending: usize,
	/// The hops every relay's request crosses once it settled, summed.
	hops: usize,
	/// Whether every relay with a path routed to its publisher once it settled.
	right: Result<(), String>,
}

fn observe(lab: &mut Lab, change: &dyn Fn(&mut Lab)) -> Row {
	let before = lab.net.faults();
	let cost = lab.measure(change);
	let after = lab.net.faults();
	Row {
		cost,
		faults: Faults {
			false_ends: after.false_ends - before.false_ends,
			revivals: after.revivals - before.revivals,
		},
		pending: lab.net.held().pending,
		hops: lab.hops(),
		right: match cost.settled {
			true => lab.check(),
			false => Err("never settled".into()),
		},
	}
}

/// The relay the most others route through toward the first publisher, its own aside.
fn transit(mesh: &Mesh, tree: &[Option<Node>]) -> Option<Node> {
	let mut through = vec![0; mesh.relays];
	for relay in 0..mesh.relays {
		let mut at = relay;
		while let Some(next) = tree[at] {
			through[next] += 1;
			at = next;
		}
	}
	(0..mesh.relays)
		.filter(|relay| !mesh.homes.contains(relay) && through[*relay] > 0)
		.max_by_key(|&relay| (through[relay], std::cmp::Reverse(relay)))
}

/// How far each relay is from the first publisher's along the cheapest-path tree.
fn depths(tree: &[Option<Node>]) -> Vec<usize> {
	(0..tree.len())
		.map(|relay| std::iter::successors(tree[relay], |&at| tree[at]).count())
		.collect()
}

/// Run every failure on `mesh` under `arm`, each from a settled mesh.
fn fail(arm: Arm, mesh: &Mesh, coarse: bool) -> Vec<(Failure, Row)> {
	let tree = mesh.tree();
	let depth = depths(&tree);
	let fresh = || {
		let mut lab = Lab::new(arm, mesh);
		lab.coarse = coarse;
		lab
	};
	let mut out = Vec::new();
	let (node, _) = mesh.publisher(0);
	let home = mesh.homes[0];
	let degree = |relay: Node| mesh.links.iter().filter(|&&(a, b, _)| a == relay || b == relay).count();

	let mut lab = fresh();
	if let Some((a, b)) = lab.tree_link(0, true, &tree) {
		out.push((Failure::Close, observe(&mut lab, &|lab| lab.cut(a, b))));
		out.push((Failure::Reopen, observe(&mut lab, &|lab| lab.mend(a, b))));
		let flap = observe(&mut lab, &|lab| {
			for _ in 0..8 {
				lab.cut(a, b);
				lab.pass(1);
				lab.mend(a, b);
				lab.pass(1);
			}
		});
		out.push((Failure::Flap, flap));
		// The link farthest from the publisher that shares no relay with the first.
		let other = (mesh.links.iter())
			.filter(|&&(c, d, _)| ![c, d].contains(&a) && ![c, d].contains(&b) && ![c, d].contains(&home))
			.max_by_key(|&&(c, d, _)| (depth[c] + depth[d], c, d));
		if let Some(&(c, d, _)) = other {
			let twin = observe(&mut lab, &|lab| {
				lab.cut(a, b);
				lab.cut(c, d);
			});
			out.push((Failure::Twin, twin));
		}
	}

	// The half of the relays farthest from the publisher lose every link to the rest.
	let mut far: Vec<Node> = (0..mesh.relays).filter(|&relay| relay != home).collect();
	far.sort_by_key(|&relay| (std::cmp::Reverse(depth[relay]), relay));
	let far: BTreeSet<Node> = far.into_iter().take(mesh.relays / 2).collect();
	let crossing: Vec<(Node, Node)> = (mesh.links.iter())
		.filter(|&&(a, b, _)| far.contains(&a) != far.contains(&b))
		.map(|&(a, b, _)| (a, b))
		.collect();
	let mut lab = fresh();
	let partition = observe(&mut lab, &|lab| {
		for &(a, b) in &crossing {
			lab.cut(a, b);
		}
	});
	out.push((Failure::Partition, partition));
	let heal = observe(&mut lab, &|lab| {
		for &(a, b) in &crossing {
			lab.mend(a, b);
		}
	});
	out.push((Failure::Heal, heal));

	let transit = transit(mesh, &tree);
	if let Some(relay) = transit {
		let mut lab = fresh();
		out.push((Failure::Death, observe(&mut lab, &|lab| lab.crash(relay))));
		out.push((Failure::Restart, observe(&mut lab, &|lab| lab.revive(relay))));
		let mut lab = fresh();
		let slow = observe(&mut lab, &|lab| lab.crash_over(relay, IDLE));
		out.push((Failure::SlowDeath, slow));
		let mut lab = fresh();
		out.push((Failure::Goaway, observe(&mut lab, &|lab| lab.net.goaway(relay))));
		let drained = observe(&mut lab, &|lab| {
			for peer in lab.links_of(relay) {
				lab.cut(relay, peer);
			}
		});
		out.push((Failure::Drained, drained));
	}
	let hub = (0..mesh.relays)
		.filter(|relay| !mesh.homes.contains(relay) && Some(*relay) != transit)
		.max_by_key(|&relay| (degree(relay), std::cmp::Reverse(relay)));
	if let Some(relay) = hub {
		let mut lab = fresh();
		let slow = observe(&mut lab, &|lab| lab.crash_over(relay, IDLE));
		out.push((Failure::HubDeath, slow));
	}

	// A second home as far from the first as the mesh allows.
	let second = (0..mesh.relays)
		.filter(|&relay| relay != home)
		.max_by_key(|&relay| (depth[relay], std::cmp::Reverse(relay)))
		.expect("two relays");
	let mut lab = fresh();
	lab.attach(node, second);
	assert!(lab.settle());
	out.push((Failure::LoseOne, observe(&mut lab, &|lab| lab.cut(node, home))));
	out.push((Failure::LoseBoth, observe(&mut lab, &|lab| lab.cut(node, second))));

	let next = (mesh.links.iter()).find_map(|&(a, b, _)| match home {
		_ if a == home => Some(b),
		_ if b == home => Some(a),
		_ => None,
	});
	if let Some(next) = next {
		let mut lab = fresh();
		let moved = observe(&mut lab, &|lab| {
			lab.cut(node, home);
			lab.attach(node, next);
		});
		out.push((Failure::Move, moved));
	}

	let mut lab = fresh();
	out.push((Failure::Leave, observe(&mut lab, &|lab| lab.cut(node, home))));
	let mut lab = fresh();
	out.push((Failure::Home, observe(&mut lab, &|lab| lab.crash(home))));
	out
}

/// The prototype carries every change of the path rule's comparison on its shapes,
/// and is right whenever it settles.
#[test]
fn the_prototype_converges_on_every_shape() {
	let mut rng = XorShift(0x6d6f_7100);
	let mut meshes = vec![Mesh::ring(6), Mesh::clique(6, 1), Mesh::clique(4, 3)];
	meshes.extend((0..8).map(|_| Mesh::sparse(6, &mut rng)));
	for mesh in &meshes {
		for arm in MULTI {
			run(arm, mesh);
		}
	}
}

/// Under churn the prototype settles and is right once it does.
#[test]
fn the_prototype_settles_after_churn() {
	for arm in MULTI {
		for two_homes in [false, true] {
			churn(arm, 0..60, two_homes);
		}
	}
}

/// The prototype carries every failure on every shape: it settles, every relay with a
/// path routes to its publisher, and no reader sees an end the publisher never made
/// or a life it had seen end.
#[test]
fn the_prototype_carries_every_failure_on_every_shape() {
	for shape in shapes(12, 2) {
		for arm in MULTI {
			for (failure, row) in fail(arm, &shape.mesh, false) {
				let case = format!("{}: {} on {}", arm.name(), failure.as_str(), shape.name);
				assert_eq!(row.right, Ok(()), "{case}");
				assert_eq!(row.faults, Faults::default(), "{case}");
			}
		}
	}
}

/// Every failure on every shape, per arm: what it cost and what readers saw.
#[test]
#[ignore = "a benchmark; Nightly reports it"]
fn benchmark_bakeoff_failures() {
	println!(
		"shape,relays,links,failure,rule,route_messages,claim_messages,settle_ms,fed_ms,unfed,loops,offline,unavailable,false_ends,revivals,pending,hops,right"
	);
	let sizes: Vec<usize> = std::env::var("MOQ_BAKEOFF_RELAYS").map_or(vec![12, 32], |sizes| {
		sizes.split(',').map(|size| size.parse().expect("MOQ_BAKEOFF_RELAYS")).collect()
	});
	for relays in sizes {
		for shape in shapes(relays, 2) {
			for arm in arms() {
				if arm == Arm::TODAY && !shape.today {
					continue;
				}
				for (failure, row) in fail(arm, &shape.mesh, relays > 16) {
					let ms = |rounds: usize| rounds as f64 * HOP.as_secs_f64() * 1000.0;
					println!(
						"{},{},{},{},{},{},{},{:.0},{:.0},{},{},{},{},{},{},{},{},{}",
						shape.name,
						shape.mesh.relays,
						shape.mesh.links.len(),
						failure.as_str(),
						arm.name(),
						row.cost.sent.msgs,
						row.cost.sent.claims,
						ms(row.cost.rounds),
						ms(row.cost.right),
						row.cost.stale + row.cost.dark,
						row.cost.loops,
						row.cost.sent.offline,
						row.cost.sent.unavailable,
						row.faults.false_ends,
						row.faults.revivals,
						row.pending,
						row.hops,
						row.right.as_ref().map_or_else(|err| err.replace(',', ";"), |()| "yes".into()),
					);
				}
			}
		}
	}
}

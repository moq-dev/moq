//! The bake-off: the multicast epic's rule as prototyped ([`multi::Router`]) beside
//! the router that ships, on the same meshes and failure schedules.
//!
//! Every arm builds the same [`Mesh`], sees the same failure at the same instant, and
//! is measured by the same [`Lab`]: messages, hops of delay until every relay's
//! request reaches its publisher for good, the relay-rounds a reader went unfed
//! meanwhile, loops among selected routes, and what readers were shown. The ignored
//! benchmarks print the tables; the tests pin what they show.

use std::collections::BTreeSet;

use super::compare::*;
use super::multi::{MultiSim, Suspect, Tie};
use super::*;

/// The arms measured, in the report's order.
fn arms() -> Vec<Arm> {
	vec![
		Arm::PLAIN,
		Arm::with(|tune| tune.suspect = Suspect::Flagged),
		Arm::with(|tune| tune.suspect = Suspect::Local),
		Arm::with(|tune| tune.quiet = true),
		Arm::with(|tune| tune.floor = true),
		Arm::with(|tune| tune.batch = true),
		Arm::with(|tune| tune.agree = true),
		Arm::with(|tune| tune.drain = true),
		Arm::with(|tune| {
			tune.quiet = true;
			tune.batch = true;
		}),
		Arm::with(|tune| {
			tune.floor = true;
			tune.batch = true;
		}),
		Arm::TUNED,
		Arm::LEAN,
		Arm::KEPT,
		Arm::RELAY,
		Arm::with(|tune| tune.claimed = true),
		Arm::with(|tune| {
			tune.suspect = Suspect::Flagged;
			tune.floor = true;
			tune.batch = true;
			tune.drain = true;
		}),
	]
}

/// The prototype's arms.
pub(super) const MULTI: [Arm; 4] = [Arm::PLAIN, Arm::TUNED, Arm::LEAN, Arm::KEPT];

/// How long a silent death takes its neighbours to notice, in hops of delay: an idle
/// timeout of 3 s.
pub(super) const IDLE: usize = 120;

/// A mesh to measure.
pub(super) struct Shape {
	pub name: &'static str,
	pub mesh: Mesh,
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
pub(super) fn tiers(regions: usize, edges: usize, peered: bool, publishers: usize) -> Mesh {
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
pub(super) fn chords(relays: usize, publishers: usize) -> Mesh {
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
		links: links
			.into_iter()
			.filter(|(a, b)| a != b)
			.map(|(a, b)| (a, b, 1))
			.collect(),
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

pub(super) fn clique(relays: usize, publishers: usize) -> Mesh {
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
/// `None` when a mesh of `size` has no room for its gateways.
fn joined(count: usize, size: usize, joins: usize, ring: bool, publishers: usize) -> Option<Mesh> {
	// A mesh that is both entered and left, any but the ends of a chain, leaves by
	// relays past the ones it is entered by, so a claim crosses it between grants.
	let through = ring || count > 2;
	let exit = match through {
		true => (size / 2).max(1 + joins),
		false => size / 2,
	};
	if size < 2 || exit + joins > size {
		return None;
	}
	let one = chords(size, 1);
	let mut mesh = Mesh {
		relays: count * size,
		links: Vec::new(),
		ties: BTreeMap::new(),
		homes: spread(&(0..size).collect::<Vec<_>>(), publishers),
		claims: 1,
	};
	for at in 0..count {
		(mesh.links).extend(
			one.links
				.iter()
				.map(|&(a, b, cost)| (at * size + a, at * size + b, cost)),
		);
		if at + 1 == count && !(ring && count > 2) {
			continue;
		}
		let next = (at + 1) % count;
		for join in 0..joins {
			let (a, b) = (at * size + exit + join, next * size + 1 + join);
			mesh.links.push((a, b, 1));
			mesh.ties.insert((a, b), Tie::Scoped);
		}
	}
	Some(mesh)
}

/// Every shape, at about `relays` relays each.
pub(super) fn shapes(relays: usize, publishers: usize) -> Vec<Shape> {
	let mut rng = XorShift(0x6d6f_7100 + relays as u64);
	let open = |name, mesh| Shape { name, mesh };
	let grants = |name, mesh: Option<Mesh>| mesh.map(|mesh| Shape { name, mesh });
	let (regions, edges) = match relays {
		..12 => (2, 2),
		12..24 => (3, 3),
		24..48 => (4, 6),
		_ => (6, 8),
	};
	let mut shapes = vec![
		open("tiers", tiers(regions, edges, true, publishers)),
		open("tiers_unpeered", tiers(regions, edges, false, publishers)),
		open("chords", chords(relays, publishers)),
		open("sparse", sparse(relays, publishers, &mut rng)),
		open("clique", clique(relays, publishers)),
		open("leaves", leafy(relays / 2, relays - relays / 2, publishers)),
	];
	// A mesh too small for its gateways is left out.
	shapes.extend(
		[
			grants("two_meshes_one_grant", joined(2, relays / 2, 1, false, publishers)),
			grants("two_meshes_two_grants", joined(2, relays / 2, 2, false, publishers)),
			grants("three_meshes_chain", joined(3, relays / 3, 1, false, publishers)),
			grants("three_meshes_ring", joined(3, relays / 3, 1, true, publishers)),
		]
		.into_iter()
		.flatten(),
	);
	shapes
}

/// The failures measured, each from a settled mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Failure {
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
	pub fn as_str(self) -> &'static str {
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
pub(super) struct Row {
	pub cost: Cost,
	pub faults: Faults,
	/// Dead hops, doubts, and suspected nodes left once it settled.
	pub pending: usize,
	/// Pairs of nodes with a session known closed, kept for good.
	pub lasting: usize,
	/// The hops every relay's request crosses once it settled, summed.
	pub hops: usize,
	/// Whether every relay with a path routed to its publisher once it settled.
	pub right: Result<(), String>,
	/// Per publisher and relay, the nodes its request visits once it settled, and
	/// whether it reached the publisher.
	pub answers: Vec<Vec<Option<Chased>>>,
}

/// The nodes a request visits, and whether it reached its publisher.
pub(super) type Chased = (Vec<Node>, bool);

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
		lasting: lab.net.held().lasting,
		hops: lab.hops(),
		right: match cost.settled {
			true => lab.check(),
			false => Err("never settled".into()),
		},
		answers: (0..lab.mesh.homes.len())
			.map(|index| {
				let (_, id) = lab.mesh.publisher(index);
				(0..lab.mesh.relays).map(|relay| lab.net.chase(relay, id)).collect()
			})
			.collect(),
	}
}

/// The relay the most others route through toward the first publisher, its own aside.
pub(super) fn transit(mesh: &Mesh, tree: &[Option<Node>]) -> Option<Node> {
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
pub(super) fn fail(arm: Arm, mesh: &Mesh, coarse: bool) -> Vec<(Failure, Row)> {
	fail_only(arm, mesh, coarse, None)
}

/// Run only the requested subset of failures (or all when `only` is `None`), skipping
/// unrelated scenario groups while keeping prerequisite steps (such as Goaway before Drained).
fn fail_only(arm: Arm, mesh: &Mesh, coarse: bool, only: Option<&[Failure]>) -> Vec<(Failure, Row)> {
	let want = |f: Failure| only.is_none_or(|set| set.contains(&f));
	let want_any = |fs: &[Failure]| fs.iter().copied().any(want);
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

	if want_any(&[Failure::Close, Failure::Reopen, Failure::Flap, Failure::Twin]) {
		let mut lab = fresh();
		if let Some((a, b)) = lab.tree_link(0, true, &tree) {
			out.push((Failure::Close, observe(&mut lab, &|lab| lab.cut(a, b))));
			if want_any(&[Failure::Reopen, Failure::Flap, Failure::Twin]) {
				out.push((Failure::Reopen, observe(&mut lab, &|lab| lab.mend(a, b))));
			}
			if want_any(&[Failure::Flap, Failure::Twin]) {
				let flap = observe(&mut lab, &|lab| {
					for _ in 0..8 {
						lab.cut(a, b);
						lab.pass(1);
						lab.mend(a, b);
						lab.pass(1);
					}
				});
				out.push((Failure::Flap, flap));
			}
			if want(Failure::Twin) {
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
		}
	}

	if want_any(&[Failure::Partition, Failure::Heal]) {
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
		if want(Failure::Heal) {
			let heal = observe(&mut lab, &|lab| {
				for &(a, b) in &crossing {
					lab.mend(a, b);
				}
			});
			out.push((Failure::Heal, heal));
		}
	}

	let transit = transit(mesh, &tree);
	if let Some(relay) = transit {
		if want_any(&[Failure::Death, Failure::Restart]) {
			let mut lab = fresh();
			out.push((Failure::Death, observe(&mut lab, &|lab| lab.crash(relay))));
			if want(Failure::Restart) {
				out.push((Failure::Restart, observe(&mut lab, &|lab| lab.revive(relay))));
			}
		}
		if want(Failure::SlowDeath) {
			let mut lab = fresh();
			let slow = observe(&mut lab, &|lab| lab.crash_over(relay, IDLE));
			out.push((Failure::SlowDeath, slow));
		}
		if want_any(&[Failure::Goaway, Failure::Drained]) {
			let mut lab = fresh();
			out.push((Failure::Goaway, observe(&mut lab, &|lab| lab.net.goaway(relay))));
			if want(Failure::Drained) {
				let drained = observe(&mut lab, &|lab| {
					for peer in lab.links_of(relay) {
						lab.cut(relay, peer);
					}
				});
				out.push((Failure::Drained, drained));
			}
		}
	}
	if want(Failure::HubDeath) {
		let hub = (0..mesh.relays)
			.filter(|relay| !mesh.homes.contains(relay) && Some(*relay) != transit)
			.max_by_key(|&relay| (degree(relay), std::cmp::Reverse(relay)));
		if let Some(relay) = hub {
			let mut lab = fresh();
			let slow = observe(&mut lab, &|lab| lab.crash_over(relay, IDLE));
			out.push((Failure::HubDeath, slow));
		}
	}

	if want_any(&[Failure::LoseOne, Failure::LoseBoth]) {
		// A second home as far from the first as the mesh allows.
		let second = (0..mesh.relays)
			.filter(|&relay| relay != home)
			.max_by_key(|&relay| (depth[relay], std::cmp::Reverse(relay)))
			.expect("two relays");
		let mut lab = fresh();
		lab.attach(node, second);
		assert!(lab.settle());
		out.push((Failure::LoseOne, observe(&mut lab, &|lab| lab.cut(node, home))));
		if want(Failure::LoseBoth) {
			out.push((Failure::LoseBoth, observe(&mut lab, &|lab| lab.cut(node, second))));
		}
	}

	if want(Failure::Move) {
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
	}

	if want(Failure::Leave) {
		let mut lab = fresh();
		out.push((Failure::Leave, observe(&mut lab, &|lab| lab.cut(node, home))));
	}
	if want(Failure::Home) {
		let mut lab = fresh();
		out.push((Failure::Home, observe(&mut lab, &|lab| lab.crash(home))));
	}
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

/// Every failure on every shape under `arm`: it settles, every relay with a path
/// routes to its publisher, and no reader sees an end the publisher never made or a
/// life it had seen end. On three meshes in a ring that includes the chain stopping
/// a claim circling: once the publisher leaves, every relay lets go.
pub(super) fn carries(arm: Arm, relays: usize) {
	for shape in shapes(relays, 2) {
		for (failure, row) in fail(arm, &shape.mesh, relays > 16) {
			let case = format!("{}: {} on {}", arm.name(), failure.as_str(), shape.name);
			assert_eq!(row.right, Ok(()), "{case}");
			assert_eq!(row.faults, Faults::default(), "{case}");
			// A relay that sent GOAWAY stays named while routes cross it.
			if failure != Failure::Goaway {
				assert_eq!(row.pending, 0, "{case}");
			}
		}
	}
}

#[test]
fn the_prototype_carries_every_failure_on_every_shape() {
	for arm in MULTI {
		carries(arm, 9);
	}
}

/// The same at the quest's largest size, for the rule the bake-off recommends.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn sweep_the_prototype_carries_every_failure_at_32_relays() {
	carries(Arm::TUNED, 32);
	carries(Arm::LEAN, 32);
	carries(Arm::KEPT, 32);
}

/// A publisher is an endpoint of its relay, so one leaving, moving, or losing a home
/// changes claims and no route, however the mesh is shaped.
#[test]
fn an_endpoint_changes_no_route() {
	for shape in shapes(9, 2) {
		for (failure, row) in fail(Arm::TUNED, &shape.mesh, false) {
			if matches!(
				failure,
				Failure::Leave | Failure::Move | Failure::LoseOne | Failure::LoseBoth
			) {
				assert_eq!(row.cost.sent.msgs, 0, "{} on {}", failure.as_str(), shape.name);
			}
		}
	}
}

/// With routes only to relays that announce, a publisher leaving still changes no
/// route, since its relay's route stands, but one arriving at a relay that never
/// announced starts that relay's route at every node. Across a scoped grant a
/// leaving claim can do the same: a gateway that hears the other mesh's echo of it
/// for a moment announces it again, its first claim.
#[test]
fn a_first_claim_starts_a_route() {
	for arm in [Arm::LEAN, Arm::RELAY] {
		let (mut echoed, mut started) = (0, 0);
		for shape in shapes(9, 2) {
			let scoped = shape.mesh.ties.values().any(|tie| *tie == Tie::Scoped);
			for (failure, row) in fail(arm, &shape.mesh, false) {
				let case = format!("{} on {} under {}", failure.as_str(), shape.name, arm.name());
				match failure {
					Failure::Leave | Failure::LoseOne | Failure::LoseBoth if scoped => echoed += row.cost.sent.msgs,
					Failure::Leave | Failure::LoseOne | Failure::LoseBoth => {
						assert_eq!(row.cost.sent.msgs, 0, "{case}")
					}
					// Unless the relay it moves to has announced before.
					Failure::Move => started += row.cost.sent.msgs,
					_ => {}
				}
			}
		}
		assert!(started > 0, "no move started a route under {}", arm.name());
		assert!(echoed > 0, "no gateway announced an echo under {}", arm.name());
	}
}

/// A relay with nothing to announce costs no route anywhere: the routes a mesh
/// holds follow the relays that announce, not the relays.
#[test]
fn a_relay_with_no_claim_offers_no_route() {
	let held = |arm: Arm, publishers: usize| {
		let lab = Lab::new(arm, &tiers(3, 3, true, publishers));
		lab.net.held()
	};
	for publishers in [1, 2, 9] {
		let all = held(Arm::TUNED, publishers);
		for arm in [Arm::LEAN, Arm::RELAY] {
			let lean = held(arm, publishers);
			println!("{publishers} publishers: {all:?} {lean:?} under {}", arm.name());
			assert!(
				lean.offers * 15 <= all.offers * publishers.min(9) + all.offers,
				"{publishers} under {}",
				arm.name()
			);
		}
	}
}

/// A failure costs route messages per forwarding node, never per publisher behind it.
/// Every relay is one under the prototype as tuned; under the router, every relay a
/// publisher joins is, so it is measured with the same relays behind more publishers.
#[test]
fn a_failure_costs_the_same_with_more_publishers() {
	let cost = |arm: Arm, mesh: &Mesh| {
		let rows = fail_only(arm, mesh, false, Some(&[Failure::Close, Failure::Partition]));
		let of = |failure: Failure| {
			rows.iter()
				.find(|(found, _)| *found == failure)
				.map(|(_, row)| row.cost.sent.msgs)
		};
		(of(Failure::Close), of(Failure::Partition))
	};
	let mesh = |publishers: usize| tiers(3, 3, true, publishers);
	assert_eq!(cost(Arm::TUNED, &mesh(1)), cost(Arm::TUNED, &mesh(12)));
	let few = mesh(3);
	let many = Mesh {
		homes: few.homes.iter().cycle().take(12).copied().collect(),
		..few.clone()
	};
	assert_eq!(cost(Arm::RELAY, &few), cost(Arm::RELAY, &many));
}

/// Remembering the highest session closed between a pair of nodes stops a stale
/// offer reviving a path through it, which is most of what the rule as written
/// hunts through when a relay drains and closes.
#[test]
fn a_session_floor_stops_most_of_the_hunting() {
	let mesh = chords(12, 2);
	let cost = |arm: Arm| {
		let rows = fail_only(arm, &mesh, false, Some(&[Failure::Drained]));
		let drained = rows.iter().find(|(failure, _)| *failure == Failure::Drained);
		drained.expect("a transit relay").1.cost.sent.msgs
	};
	let (plain, floored) = (cost(Arm::PLAIN), cost(Arm::with(|tune| tune.floor = true)));
	assert!(floored * 10 < plain, "{floored} against {plain}");
}

/// Deliver everything queued, as one hop of delay. False once nothing is left.
fn deliver(net: &mut MultiSim) -> bool {
	let ready = net.ready();
	for &((from, to), count) in &ready {
		for _ in 0..count {
			net.deliver(from, to);
		}
	}
	net.flush();
	!ready.is_empty()
}

fn quiesce(net: &mut MultiSim) {
	for _ in 0..200 {
		if !deliver(net) {
			return;
		}
	}
	panic!("never settles");
}

fn tune(arm: Arm) -> multi::Tune {
	match arm {
		Arm::Multi(tune) => tune,
		Arm::Relay(_) | Arm::Layer(_) => panic!("not the prototype"),
	}
}

/// Two relays that share a draining neighbour and a scoped link each hold the
/// other's copy of its claim. The copy says it is draining too, so neither prefers
/// it to its own route and the two never trade places.
#[test]
fn a_claim_across_a_grant_keeps_its_drain() {
	for arm in MULTI {
		let mut net = MultiSim::new(tune(arm), 4);
		net.client(3);
		net.publish(3, 100);
		net.link(0, 1, 1, Tie::Peer);
		net.link(0, 2, 1, Tie::Peer);
		net.link(1, 2, 1, Tie::Scoped);
		net.link(3, 0, 1, Tie::Peer);
		net.claim(3, "live/0", true);
		quiesce(&mut net);
		// Both read the GOAWAY before either hears from the other.
		net.goaway(0);
		quiesce(&mut net);
		let broadcast = (100, p("live/0"));
		for relay in [1, 2] {
			let router = net.router(relay).expect("a relay");
			assert_eq!(
				router.pick(&broadcast),
				Some(hop(0, 0)),
				"relay {relay} under {}",
				arm.name()
			);
		}
	}
}

/// A scoped neighbour that sends GOAWAY is draining for the claims it gave, so a
/// node holding the same claim from another neighbour announces that one instead.
#[test]
fn a_scoped_neighbour_draining_drains_its_claims() {
	for arm in MULTI {
		// A publisher on 0 and 2, each of which grants 1 the claim, 0 the cheaper.
		let mut net = MultiSim::new(tune(arm), 4);
		net.client(3);
		net.publish(3, 100);
		net.link(3, 0, 1, Tie::Peer);
		net.link(3, 2, 1, Tie::Peer);
		net.link(0, 1, 1, Tie::Scoped);
		net.link(2, 1, 2, Tie::Scoped);
		net.claim(3, "live/0", true);
		quiesce(&mut net);
		let broadcast = (100, p("live/0"));
		let source = |net: &MultiSim| net.router(1).expect("a relay").step(hop(1, 0), &broadcast);
		assert_eq!(source(&net), Some(multi::Next::Local(0)), "under {}", arm.name());
		net.goaway(0);
		quiesce(&mut net);
		assert_eq!(source(&net), Some(multi::Next::Local(2)), "under {}", arm.name());
	}
}

/// A relay that takes a newer copy of a node's claims from another neighbour tells
/// its own neighbours the floor even when nothing in the copy differs from what it
/// had, so one that heard a claim in between, by a route since gone, lets go of it.
#[test]
fn a_newer_floor_travels_with_nothing_changed() {
	for arm in MULTI {
		// 0 announces. 3 hears it through 1, and later 2; 4 hears it through 3 and 5.
		let mut net = MultiSim::new(tune(arm), 7);
		net.client(6);
		net.publish(6, 100);
		for (a, b) in [(0, 1), (0, 2), (1, 3), (3, 4), (0, 5), (5, 4), (6, 0)] {
			net.link(a, b, 1, Tie::Peer);
		}
		net.claim(6, "live/a", true);
		quiesce(&mut net);
		let late = (100, p("live/b"));
		let shown = |net: &MultiSim, relay: Node| net.router(relay).expect("a relay").shown(&late);

		// 3 misses the claim starting, which reaches 4 through 5.
		net.hold(1, 3);
		net.hold(4, 3);
		net.claim(6, "live/b", true);
		quiesce(&mut net);
		assert_eq!((shown(&net, 3), shown(&net, 4)), (None, Some(true)));

		// 5 misses it ending, and then 4 loses its route through 5.
		net.hold(0, 5);
		net.claim(6, "live/b", false);
		quiesce(&mut net);
		net.close(5, 4);
		quiesce(&mut net);
		assert_eq!(shown(&net, 4), Some(true), "nothing has told it yet");

		// 3 hears a newer copy from 2 that matches what it holds.
		net.link(2, 3, 1, Tie::Peer);
		quiesce(&mut net);
		assert_eq!(shown(&net, 4), None, "under {}", arm.name());
	}
}

/// The floor's cost: a relay that restarts is a new node, so each restart leaves
/// its neighbours an entry nothing removes. Filed per pair of relays, with the
/// restart in the session's order, a restart replaces the entry instead.
#[test]
fn a_session_floor_keeps_an_entry_per_restart_unless_filed_by_relay() {
	let kept = |arm: Arm| {
		let mut net = MultiSim::new(tune(arm), 3);
		for (a, b) in [(0, 1), (1, 2), (0, 2)] {
			net.link(a, b, 1, Tie::Peer);
		}
		quiesce(&mut net);
		let mut kept = Vec::new();
		for _ in 0..8 {
			net.reset(0);
			quiesce(&mut net);
			for peer in [1, 2] {
				net.open(0, peer);
			}
			quiesce(&mut net);
			kept.push(net.kept().closed);
		}
		kept
	};
	let (grows, bounded) = (
		kept(Arm::TUNED),
		kept(Arm::with(|tune| {
			tune.floor = true;
			tune.epoch = true;
		})),
	);
	println!("pairs kept after each of 8 restarts of one relay in three: {grows:?}, by relay {bounded:?}");
	assert!(grows.windows(2).all(|pair| pair[1] > pair[0]), "{grows:?}");
	// Two ways round each of the three links at most.
	assert!(bounded.iter().all(|&pairs| pairs <= 6), "{bounded:?}");
	assert_eq!(bounded[0], bounded[7], "{bounded:?}");
}

/// Every failure on every shape, per arm: what it cost and what readers saw.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn benchmark_bakeoff_failures() {
	println!(
		"shape,relays,links,failure,rule,route_messages,claim_items,settle_ms,fed_ms,unfed,loops,offline,unavailable,false_ends,revivals,pending,lasting,hops,right"
	);
	let sizes: Vec<usize> = std::env::var("MOQ_BAKEOFF_RELAYS").map_or(vec![12, 32], |sizes| {
		sizes
			.split(',')
			.map(|size| size.parse().expect("MOQ_BAKEOFF_RELAYS"))
			.collect()
	});
	for relays in sizes {
		for shape in shapes(relays, 2) {
			for arm in arms() {
				let unwanted = |var: &str, name: &str| {
					std::env::var(var).is_ok_and(|list| !list.split(',').any(|want| want == name))
				};
				// Without the session floor and a burst read first, a 32-relay mesh
				// hunts through millions of messages: the 12-relay rows show it.
				let hunts = matches!(arm, Arm::Multi(tune) if !(tune.floor && tune.batch));
				if (relays > 16 && hunts && std::env::var("MOQ_BAKEOFF_ARMS").is_err())
					|| unwanted("MOQ_BAKEOFF_ARMS", &arm.name())
					|| unwanted("MOQ_BAKEOFF_SHAPES", shape.name)
				{
					continue;
				}
				for (failure, row) in fail(arm, &shape.mesh, relays > 16) {
					let ms = |rounds: usize| rounds as f64 * HOP.as_secs_f64() * 1000.0;
					println!(
						"{},{},{},{},{},{},{},{:.0},{:.0},{},{},{},{},{},{},{},{},{},{}",
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
						row.lasting,
						row.hops,
						row.right
							.as_ref()
							.map_or_else(|err| err.replace(',', ";"), |()| "yes".into()),
					);
				}
			}
		}
	}
}

/// What a mesh costs to build and hold, and what one link closing and one relay
/// dying cost on it, as relays, publishers, and broadcasts grow.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn benchmark_bakeoff_growth() {
	println!(
		"grows,relays,links,publishers,broadcasts,rule,build_route_messages,build_claim_items,held_routes,heard_routes,held_claims,lasting_after_failures,close_route_messages,death_route_messages,death_claim_items,work"
	);
	let tiers_of = |relays: usize, publishers: usize, claims: usize| {
		let (regions, edges) = match relays {
			..24 => (3, 3),
			24..48 => (4, 6),
			_ => (6, 8),
		};
		Mesh {
			claims,
			..tiers(regions, edges, true, publishers)
		}
	};
	let mut cases = Vec::new();
	cases.extend([15, 32, 60].map(|relays| ("relays", tiers_of(relays, 8, 1))));
	cases.extend([1, 8, 64, 256].map(|publishers| ("publishers", tiers_of(32, publishers, 1))));
	cases.extend([1, 8, 64].map(|claims| ("broadcasts", tiers_of(32, 8, claims))));
	cases.extend([8, 16, 32].map(|relays| ("links", clique(relays, 8))));
	for (grows, mesh) in cases {
		for arm in [Arm::TUNED, Arm::LEAN, Arm::KEPT, Arm::RELAY] {
			let tree = mesh.tree();
			let mut lab = Lab::new(arm, &mesh);
			lab.coarse = true;
			let (built, held) = (lab.net.sent(), lab.net.held());
			let close = lab.tree_link(0, true, &tree).map(|(a, b)| {
				let cost = lab.measure(&|lab| lab.cut(a, b));
				lab.mend(a, b);
				assert!(lab.settle());
				cost.sent.msgs
			});
			let death = transit(&mesh, &tree).map(|relay| lab.measure(&|lab| lab.crash(relay)));
			println!(
				"{grows},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
				mesh.relays,
				mesh.links.len(),
				mesh.homes.len(),
				mesh.homes.len() * mesh.claims,
				arm.name(),
				built.msgs,
				built.claims,
				held.routes,
				held.offers,
				held.claims,
				lab.net.held().lasting,
				close.unwrap_or(0),
				death.map_or(0, |cost| cost.sent.msgs),
				death.map_or(0, |cost| cost.sent.claims),
				lab.net.work(),
			);
		}
	}
}

/// The cases the routes epic measured the rule it shipped on, for each arm.
#[test]
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
fn benchmark_bakeoff_baseline() {
	println!("case,rule,route_messages,claim_items,settle_ms,fed_ms,loops,stale,dark");
	let ms = |rounds: f64| rounds * HOP.as_secs_f64() * 1000.0;
	for arm in [Arm::PLAIN, Arm::TUNED, Arm::LEAN, Arm::KEPT, Arm::RELAY] {
		let mut rng = XorShift(0x6d6f_7100 + 24);
		let sparse: Vec<Mesh> = (0..16).map(|_| Mesh::sparse(24, &mut rng)).collect();
		let groups = [
			("sparse_24", sparse),
			("clique_24", vec![Mesh::clique(24, 1)]),
			("clique_24_8_publishers", vec![Mesh::clique(24, 8)]),
		];
		for (shape, meshes) in &groups {
			let mut all: BTreeMap<super::compare::Change, Vec<Cost>> = BTreeMap::new();
			for mesh in meshes {
				for (change, cost) in run(arm, mesh) {
					all.entry(change).or_default().push(cost);
				}
			}
			for (change, costs) in all {
				let mean =
					|field: fn(&Cost) -> usize| costs.iter().map(field).sum::<usize>() as f64 / costs.len() as f64;
				println!(
					"{shape}:{},{},{:.1},{:.1},{:.0},{:.0},{:.1},{:.1},{:.1}",
					change.as_str(),
					arm.name(),
					mean(|cost| cost.sent.msgs),
					mean(|cost| cost.sent.claims),
					ms(mean(|cost| cost.rounds)),
					ms(mean(|cost| cost.right)),
					mean(|cost| cost.loops),
					mean(|cost| cost.stale),
					mean(|cost| cost.dark),
				);
			}
		}
		// One link comes up on a full mesh of 32.
		let mut mesh = Mesh::clique(32, 1);
		mesh.links.retain(|&(a, b, _)| (a, b) != (30, 31));
		let mut lab = Lab::new(arm, &mesh);
		let cost = lab.measure(&|lab| lab.attach(30, 31));
		println!(
			"clique_32:link_comes_up,{},{},{},{:.0},{:.0},{},{},{}",
			arm.name(),
			cost.sent.msgs,
			cost.sent.claims,
			ms(cost.rounds as f64),
			ms(cost.right as f64),
			cost.loops,
			cost.stale,
			cost.dark,
		);
		let churn = churn(arm, 0..400, false);
		println!(
			"churn_{}_steps,{},{},{},0,0,{},{},{}",
			churn.steps,
			arm.name(),
			churn.sent.msgs,
			churn.sent.claims,
			churn.loops,
			churn.stale,
			churn.dark,
		);
	}
}

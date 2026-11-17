//! Three ways to find a warm copy, measured on [`WarmSim`] over the multicast
//! prototype's route rule, for the pick `quest/m3/cache-routing/select.md` builds.
//!
//! Each shape is built under every [`Arm`], then a script of reader changes and link
//! closes runs in phases, each settled before the next: what each broadcast's tree
//! costs against the cheapest, how many copies leave the publisher's relay, the
//! messages and rounds it took, and whether pulls ever formed a cycle or left a reader
//! unfed. The ignored benchmarks print the tables; the tests pin what they show.

use super::multi::Tie;
use super::warm::*;
use super::*;

/// Every arm measured, in the report's order.
fn arms() -> Vec<Arm> {
	let mut out = Vec::new();
	for hops in [true, false] {
		out.push(Arm {
			mode: Mode::Path,
			hops,
			merge: false,
			chain: false,
			stay: false,
			rejoin: false,
			reroute: false,
		});
	}
	for mode in [
		Mode::Ask(1),
		Mode::Ask(2),
		Mode::Ask(usize::MAX),
		Mode::Advertise(Audience::All),
		Mode::Advertise(Audience::Useful),
	] {
		for hops in [true, false] {
			for merge in [false, true] {
				out.push(Arm {
					mode,
					hops,
					merge,
					chain: false,
					stay: false,
					rejoin: false,
					reroute: false,
				});
			}
		}
	}
	out
}

const fn arm(mode: Mode, merge: bool) -> Arm {
	Arm {
		mode,
		hops: true,
		merge,
		chain: false,
		stay: false,
		rejoin: false,
		reroute: false,
	}
}

const PATH: Arm = arm(Mode::Path, false);
const ASK: Arm = arm(Mode::Ask(usize::MAX), false);
const ASK_MERGE: Arm = arm(Mode::Ask(usize::MAX), true);
const ADVERTISE: Arm = arm(Mode::Advertise(Audience::All), false);
const ADVERTISE_MERGE: Arm = arm(Mode::Advertise(Audience::All), true);
const USEFUL: Arm = arm(Mode::Advertise(Audience::Useful), false);

/// A mesh of relays with one publisher.
#[derive(Clone, Debug)]
struct Shape {
	name: &'static str,
	relays: usize,
	links: Vec<(Node, Node, Price, Tie)>,
	/// The relay the publisher joins.
	home: Node,
	claims: usize,
}

impl Shape {
	fn build(&self, arm: Arm, seed: u64) -> WarmSim {
		let mut sim = WarmSim::new(arm, self.relays, 1, seed);
		for &(a, b, price, tie) in &self.links {
			sim.link(a, b, price, tie);
		}
		assert!(sim.settle(), "{} never builds", self.name);
		sim.publish(0, self.home, self.claims);
		assert!(sim.settle(), "{} never announces", self.name);
		sim
	}
}

/// One change a script makes.
#[derive(Clone, Copy, Debug)]
enum Step {
	/// A reader at a relay starts or stops reading a broadcast.
	Read(Node, usize, bool),
	Close(Node, Node),
	/// Hops of delay pass, delivering what is queued.
	Pass(usize),
}

/// Steps applied together, then settled.
#[derive(Clone, Debug)]
struct Phase {
	name: &'static str,
	steps: Vec<Step>,
}

fn phase(name: &'static str, steps: Vec<Step>) -> Phase {
	Phase { name, steps }
}

/// What a phase cost and left.
#[derive(Clone, Debug)]
struct Row {
	phase: &'static str,
	right: Result<(), String>,
	trees: Vec<Tree>,
	/// The cheapest tree per broadcast.
	cheapest: Vec<Metric>,
	sent: Sent,
	/// Reader starts and stops in the phase.
	changes: usize,
	/// Rounds each new reader waited for its first frame.
	waits: Vec<usize>,
	watch: Watch,
	/// Relays with demand and no reader, per broadcast: what a join cost other relays.
	carriers: Vec<Vec<Node>>,
}

impl Row {
	fn uplinks(&self) -> usize {
		self.trees.iter().map(|tree| tree.uplinks).sum()
	}

	fn cost(&self) -> Metric {
		self.trees.iter().map(|tree| tree.cost).sum()
	}

	fn copies(&self) -> usize {
		self.trees.iter().map(|tree| tree.copies).max().unwrap_or(0)
	}
}

fn run(shape: &Shape, arm: Arm, phases: &[Phase], seed: u64) -> Vec<Row> {
	let mut sim = shape.build(arm, seed);
	let mut rows = Vec::new();
	for phase in phases {
		let (sent, watch, waits) = (sim.sent, sim.watch, sim.waits.len());
		let mut changes = 0;
		for &step in &phase.steps {
			match step {
				Step::Read(node, cast, reading) => {
					changes += 1;
					sim.read(node, cast, reading);
				}
				Step::Close(a, b) => sim.close(a, b),
				Step::Pass(rounds) => {
					for _ in 0..rounds {
						sim.round();
					}
				}
			}
		}
		let right = match sim.settle() {
			true => sim.check(),
			false => Err("never settled".into()),
		};
		let casts = 0..sim.casts();
		rows.push(Row {
			phase: phase.name,
			right,
			trees: casts.clone().map(|cast| sim.tree(cast)).collect(),
			cheapest: casts.clone().map(|cast| sim.cheapest(cast)).collect(),
			sent: sim.sent - sent,
			changes,
			waits: sim.waits[waits..].to_vec(),
			watch: sim.watch.since(watch),
			carriers: casts.map(|cast| sim.carriers(cast)).collect(),
		});
	}
	rows
}

// --- The shapes ---

/// Pops on a line from the publisher's, `0`, with long links from `0` that pass over
/// the pops between. A long link is cheaper cold than the pops it passes over, and
/// dearer warm than joining a warm pop beside its far end.
fn pops() -> Shape {
	let links = vec![
		(0, 1, Price::new(6, 1), Tie::Peer),
		(1, 2, Price::new(4, 1), Tie::Peer),
		(2, 3, Price::new(4, 1), Tie::Peer),
		(0, 2, Price::flat(6), Tie::Peer),
		(0, 3, Price::flat(8), Tie::Peer),
	];
	Shape {
		name: "pops",
		relays: 4,
		links,
		home: 0,
		claims: 1,
	}
}

fn pop_phases() -> Vec<Phase> {
	use Step::Read;
	vec![
		phase("far_pop_reads", vec![Read(3, 0, true)]),
		phase("near_pop_reads", vec![Read(1, 0, true)]),
		phase("middle_pop_reads", vec![Read(2, 0, true)]),
		phase("near_pop_leaves", vec![Read(1, 0, false)]),
		phase("middle_pop_leaves", vec![Read(2, 0, false)]),
	]
}

/// The relay `0` joins the CDN, where the publisher is; `drones` more are meshed
/// with each other over cheap links, each with a costly uplink to it.
fn drones(drones: usize) -> Shape {
	let mut links = Vec::new();
	for a in 1..=drones {
		links.push((0, a, Price::new(10, 4), Tie::Peer));
		for b in a + 1..=drones {
			links.push((a, b, Price::new(2, 1), Tie::Peer));
		}
	}
	Shape {
		name: "drones",
		relays: drones + 1,
		links,
		home: 0,
		claims: 1,
	}
}

/// The drone carrying the broadcast's only uplink, once every drone reads it.
fn uplinked(rows: &[Row]) -> Option<Node> {
	let tree = rows.last()?.trees.first()?;
	let mut uplinks = tree.edges.iter().filter(|&&(parent, _)| parent == 0);
	let (_, drone) = uplinks.next()?;
	uplinks.next().is_none().then_some(*drone)
}

fn drone_phases(drones: usize, together: bool) -> Vec<Phase> {
	let reads: Vec<Step> = (1..=drones).map(|drone| Step::Read(drone, 0, true)).collect();
	match together {
		true => vec![phase("every_drone_reads_at_once", reads)],
		false => reads
			.into_iter()
			.map(|step| phase("one_more_drone_reads", vec![step]))
			.collect(),
	}
}

/// A publisher on `0`, two cores `1` and `2` linked to it and each other, and two
/// edges, `3` nearer `1` and `4` nearer `2`.
fn edges() -> Shape {
	let links = vec![
		(0, 1, Price::new(3, 1), Tie::Peer),
		(0, 2, Price::new(3, 1), Tie::Peer),
		(1, 2, Price::new(2, 1), Tie::Peer),
		(3, 1, Price::new(1, 1), Tie::Up),
		(3, 2, Price::new(2, 1), Tie::Up),
		(4, 2, Price::new(1, 1), Tie::Up),
		(4, 1, Price::new(2, 1), Tie::Up),
	];
	Shape {
		name: "edges",
		relays: 5,
		links,
		home: 0,
		claims: 1,
	}
}

/// A ring of `size` relays, each with its own uplink to the publisher's relay `0`,
/// carrying `claims` broadcasts.
fn ring(size: usize, claims: usize) -> Shape {
	let mut links = Vec::new();
	for at in 1..=size {
		links.push((0, at, Price::new(8, 2), Tie::Peer));
		links.push((at, at % size + 1, Price::new(1, 1), Tie::Peer));
	}
	Shape {
		name: "ring",
		relays: size + 1,
		links,
		home: 0,
		claims,
	}
}

fn ring_phases(size: usize, claims: usize, together: bool) -> Vec<Phase> {
	let all: Vec<Step> = (0..claims)
		.flat_map(|cast| (1..=size).map(move |at| Step::Read(at, cast, true)))
		.collect();
	match together {
		true => vec![phase("every_relay_reads_at_once", all)],
		false => all
			.into_iter()
			.map(|step| phase("one_more_reads", vec![step]))
			.collect(),
	}
}

/// CDN tiers: `regions` regions of two cores each, linked within a region and to
/// the next region's cores, and `edges` edges per region on both of its cores,
/// peered with each other. The publisher is on a core of the first region.
fn tiers(regions: usize, edges: usize, claims: usize) -> Shape {
	let core = |region: usize, side: usize| 2 * region + side;
	let edge = |region: usize, index: usize| 2 * regions + region * edges + index;
	let mut links = Vec::new();
	for region in 0..regions {
		links.push((core(region, 0), core(region, 1), Price::new(2, 1), Tie::Peer));
		if region + 1 < regions {
			for (a, b) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
				links.push((core(region, a), core(region + 1, b), Price::new(6, 2), Tie::Peer));
			}
		}
		for index in 0..edges {
			for side in 0..2 {
				links.push((edge(region, index), core(region, side), Price::new(3, 1), Tie::Up));
			}
			for other in index + 1..edges {
				links.push((edge(region, index), edge(region, other), Price::new(1, 1), Tie::Peer));
			}
		}
	}
	Shape {
		name: "tiers",
		relays: regions * (2 + edges),
		links,
		home: 0,
		claims,
	}
}

/// The first edge relay of a [`tiers`] shape.
fn first_edge(regions: usize) -> Node {
	2 * regions
}

/// Readers start on a seeded random half of `relays` for each broadcast, one at a
/// time, then leave in another order.
fn crowd(relays: std::ops::Range<Node>, claims: usize, seed: u64) -> Vec<Phase> {
	let mut rng = XorShift(seed);
	let mut joins = Vec::new();
	for cast in 0..claims {
		for relay in relays.clone() {
			if rng.chance(50) {
				joins.push(Step::Read(relay, cast, true));
			}
		}
	}
	for i in (1..joins.len()).rev() {
		joins.swap(i, rng.next(i as u64 + 1) as usize);
	}
	let mut leaves: Vec<Step> = (joins.iter())
		.map(|&step| match step {
			Step::Read(relay, cast, _) => Step::Read(relay, cast, false),
			step => step,
		})
		.collect();
	for i in (1..leaves.len()).rev() {
		leaves.swap(i, rng.next(i as u64 + 1) as usize);
	}
	let half = leaves.len() / 2;
	let mut phases: Vec<Phase> = joins
		.into_iter()
		.map(|step| phase("reader_joins", vec![step]))
		.collect();
	phases.extend(
		leaves
			.into_iter()
			.take(half)
			.map(|step| phase("reader_leaves", vec![step])),
	);
	phases
}

// --- The pins ---

fn last(rows: &[Row], name: &str) -> Row {
	rows.iter()
		.rev()
		.find(|row| row.phase == name)
		.expect("a phase")
		.clone()
}

fn settles(rows: &[Row], case: &str) {
	for row in rows {
		assert_eq!(row.right, Ok(()), "{case}: {}", row.phase);
	}
}

/// A far pop's join skips every pop without demand, whatever the arm: the long link
/// is the route. Path joins never learn of the near pops' demand later; asking and
/// advertising move the far pops through them and back once it ends.
#[test]
fn a_join_skips_pops_without_demand_and_warm_ones_draw_it() {
	for arm in arms().into_iter().chain(narrower()) {
		let rows = run(&pops(), arm, &pop_phases(), 1);
		let case = arm.name();
		settles(&rows, &case);
		let far = last(&rows, "far_pop_reads");
		assert_eq!(far.carriers[0], Vec::<Node>::new(), "{case}");
		assert_eq!(far.trees[0].edges, vec![(4, 0), (0, 3)], "{case}");
		// The middle pop's reader lands on the near pop's copy, and the far pop follows.
		let middle = last(&rows, "middle_pop_reads");
		let edges: BTreeSet<(Node, Node)> = middle.trees[0].edges.iter().copied().collect();
		let joined = BTreeSet::from([(4, 0), (0, 1), (1, 2), (2, 3)]);
		let skip = BTreeSet::from([(4, 0), (0, 1), (0, 2), (0, 3)]);
		// A settled pull stays: the middle pop joins the near pop's copy, but the far
		// pop, settled on its long link, never moves for the warm pops beside it.
		let stayed = BTreeSet::from([(4, 0), (0, 1), (1, 2), (0, 3)]);
		match arm.mode {
			Mode::Path => assert_eq!(edges, skip, "{case}"),
			_ if arm.stay => assert_eq!(edges, stayed, "{case}"),
			// One query goes to the cheapest link, which for the middle pop is the near
			// pop's and for the far pop the middle's.
			_ => assert_eq!(edges, joined, "{case}"),
		}
		// The near pop's reader leaves and the middle pop returns to its long link,
		// unless its pull stays, when the near pop carries on for it.
		let near = last(&rows, "near_pop_leaves");
		let carriers: &[Node] = if arm.stay { &[1] } else { &[] };
		assert_eq!(
			near.trees[0].edges.contains(&(0, 2)),
			!arm.stay,
			"{case}: {:?}",
			near.trees[0].edges
		);
		assert_eq!(near.carriers[0], carriers, "{case}");
	}
}

/// Drones that read one after another share one uplink when they can hear of each
/// other, and path joins take one each.
#[test]
fn drones_reading_in_turn_share_one_uplink() {
	for arm in arms().into_iter().chain(narrower()) {
		let rows = run(&drones(4), arm, &drone_phases(4, false), 2);
		let case = arm.name();
		settles(&rows, &case);
		let uplinks = rows.last().expect("a phase").uplinks();
		match arm.mode {
			Mode::Path => assert_eq!(uplinks, 4, "{case}"),
			_ => assert_eq!(uplinks, 1, "{case}"),
		}
	}
}

/// Drones that read at once each reach the CDN before any is warm. Summed costs
/// alone keep every uplink: no drone's copy plus the mesh beats its own. Merging on
/// the hash leaves one.
#[test]
fn drones_reading_at_once_merge_only_on_the_hash() {
	for arm in arms() {
		let rows = run(&drones(4), arm, &drone_phases(4, true), 3);
		let case = arm.name();
		settles(&rows, &case);
		let uplinks = rows.last().expect("a phase").uplinks();
		match arm.merge {
			true => assert_eq!(uplinks, 1, "{case}"),
			false => assert_eq!(uplinks, 4, "{case}"),
		}
	}
}

/// The drone holding the one uplink loses it. It can recover only through a drone
/// it feeds, so every one of them takes its own uplink at once: the race again,
/// which only the merge undoes, and only for a relay that hears of more than one
/// neighbour.
#[test]
fn drones_take_over_a_lost_uplink() {
	for arm in arms().into_iter().filter(|arm| arm.mode != Mode::Path) {
		let mut phases = drone_phases(4, false);
		let built = run(&drones(4), arm, &phases, 4);
		let holder = uplinked(&built).expect("one uplink");
		phases.push(phase("its_uplink_closes", vec![Step::Close(0, holder)]));
		let rows = run(&drones(4), arm, &phases, 4);
		let case = arm.name();
		settles(&rows, &case);
		let uplinks = rows.last().expect("a phase").uplinks();
		let merged = arm.merge && arm.mode != Mode::Ask(1);
		assert_eq!(uplinks, if merged { 1 } else { 3 }, "{case}");
	}
}

// --- Growth ---

const NOT_NEXT: Arm = arm(Mode::Advertise(Audience::NotNext), false);
const NOT_DOWN: Arm = arm(Mode::Advertise(Audience::NotDown), false);
const NOT_DOWN_ASK: Arm = arm(Mode::Advertise(Audience::NotDownAsk), false);
const NOT_DOWN_ASK_UP: Arm = arm(Mode::Advertise(Audience::NotDownAskUp), false);
const NOT_DOWN_QUERY: Arm = arm(Mode::Advertise(Audience::NotDownQuery), false);
/// The pick with the chain in place of the rule that a relay never takes a warm
/// signal from one named on its own pull's path.
const CHAIN: Arm = Arm {
	chain: true,
	..NOT_DOWN_QUERY
};

/// The pick with settled pulls staying: no move for price after the join.
const STAY: Arm = Arm {
	stay: true,
	..NOT_DOWN_QUERY
};

/// [`STAY`], choosing again when a reader of its own starts.
const REJOIN: Arm = Arm { rejoin: true, ..STAY };

/// [`STAY`], choosing again when its route changes.
const REROUTE: Arm = Arm { reroute: true, ..STAY };

/// The narrower audiences for useful adverts, which keep them off a CDN's edges.
/// [`NOT_DOWN_QUERY`] is the pick, [`CHAIN`] the pick with the chain, [`STAY`] the
/// pick whose settled pulls stay, and [`REJOIN`] and [`REROUTE`] that, choosing again
/// on a reader of its own or a new route.
fn narrower() -> Vec<Arm> {
	vec![
		NOT_NEXT,
		NOT_DOWN,
		NOT_DOWN_ASK,
		NOT_DOWN_ASK_UP,
		NOT_DOWN_QUERY,
		CHAIN,
		STAY,
		REJOIN,
		REROUTE,
	]
}

/// The cheaper ways to advertise measured beside the rest: the narrower audiences,
/// telling only once two others want it, and never over a link marked upstream.
fn audiences() -> Vec<Arm> {
	let mut arms = narrower();
	arms.extend(
		[Audience::Shared, Audience::Down]
			.into_iter()
			.map(|audience| arm(Mode::Advertise(audience), false)),
	);
	arms
}

/// The arms swept as the core mesh and the drone mesh grow, every one with the path
/// in its offers.
fn growing() -> Vec<Arm> {
	let mut arms = vec![
		PATH,
		arm(Mode::Ask(1), false),
		arm(Mode::Ask(2), false),
		ASK,
		ADVERTISE,
		USEFUL,
	];
	arms.extend(audiences());
	arms
}

/// The arms swept as a region's edges grow.
fn widening() -> Vec<Arm> {
	let mut arms = vec![PATH, arm(Mode::Ask(1), false), arm(Mode::Ask(2), false), USEFUL];
	arms.extend(narrower());
	arms
}

/// A CDN: `regions` pairs of cores, every core meshed with every other, and `edges`
/// edges per region, each homed on both of its region's cores over links it marks
/// upstream, so a region's edge has `edges` siblings under each core. With
/// `peered` a region's edges are meshed with each other too. The publisher joins
/// core 0.
fn cdn(regions: usize, edges: usize, peered: bool, claims: usize) -> Shape {
	let cores = 2 * regions;
	let edge = |region: usize, index: usize| cores + region * edges + index;
	let mut links = Vec::new();
	for a in 0..cores {
		for b in a + 1..cores {
			links.push((a, b, Price::new(6, 2), Tie::Peer));
		}
	}
	for region in 0..regions {
		for index in 0..edges {
			for core in [2 * region, 2 * region + 1] {
				links.push((edge(region, index), core, Price::new(3, 1), Tie::Up));
			}
			for other in (index + 1..edges).filter(|_| peered) {
				links.push((edge(region, index), edge(region, other), Price::new(1, 1), Tie::Peer));
			}
		}
	}
	Shape {
		name: "cdn",
		relays: cores + regions * edges,
		links,
		home: 0,
		claims,
	}
}

/// Who reads: one viewer per broadcast at a random reader relay, every one at once,
/// or a crowd of up to nine on each arriving and leaving in turn, as a crowd does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
	OneViewer,
	Crowd,
}

impl Load {
	fn name(self) -> &'static str {
		match self {
			Self::OneViewer => "one_viewer",
			Self::Crowd => "crowd",
		}
	}

	/// Readers per broadcast, drawn from `relays`.
	fn readers(self, relays: std::ops::Range<Node>, claims: usize, seed: u64) -> Vec<(Node, usize)> {
		let mut rng = XorShift(seed);
		let mut out = Vec::new();
		for cast in 0..claims {
			let mut pool: Vec<Node> = relays.clone().collect();
			let count = match self {
				Self::OneViewer => 1,
				Self::Crowd => pool.len().min(9),
			};
			for _ in 0..count {
				let at = rng.next(pool.len() as u64) as usize;
				out.push((pool.swap_remove(at), cast));
			}
		}
		out
	}
}

/// What one arm cost on one shape and load: every reader joins, then every one
/// leaves, each settled before the next when `in_turn`.
struct Grown {
	changes: usize,
	sent: Sent,
	/// Warm state each relay held with every reader in place.
	held: Vec<usize>,
	cost: Metric,
	cheapest: Metric,
	first: f64,
	right: Result<(), String>,
}

impl Grown {
	/// The mean warm state held across `relays`.
	fn held(&self, relays: std::ops::Range<Node>) -> f64 {
		let count = relays.len().max(1);
		self.held[relays].iter().sum::<usize>() as f64 / count as f64
	}
}

fn grow(shape: &Shape, readers: &[(Node, usize)], in_turn: bool, arm: Arm, cheapest: bool) -> Grown {
	let mut sim = shape.build(arm, 11);
	let (sent, waits) = (sim.sent, sim.waits.len());
	let mut settled = true;
	for &(node, cast) in readers {
		sim.read(node, cast, true);
		if in_turn {
			settled &= sim.settle();
		}
	}
	let mut right = match settled && sim.settle() {
		true => sim.check(),
		false => Err("never settled".into()),
	};
	let held: Vec<usize> = (0..shape.relays).map(|node| sim.held(node)).collect();
	let casts = 0..sim.casts();
	let cost = casts.clone().map(|cast| sim.tree(cast).cost).sum();
	let cheapest = match cheapest {
		true => casts.map(|cast| sim.cheapest(cast)).sum(),
		false => 0,
	};
	let first = mean(&sim.waits[waits..]);
	for &(node, cast) in readers {
		sim.read(node, cast, false);
		if in_turn {
			settled &= sim.settle();
		}
	}
	if right.is_ok() && !(settled && sim.settle()) {
		right = Err("never settled after leaving".into());
	}
	Grown {
		changes: 2 * readers.len(),
		sent: sim.sent - sent,
		held,
		cost,
		cheapest,
		first,
		right,
	}
}

/// A first viewer's adverts on a CDN: told to every neighbour they grow with the
/// core mesh, and told only where they could be taken they do not, since a core
/// with demand is never cheaper for another core than its own route.
#[test]
fn useful_adverts_do_not_grow_with_the_core_mesh() {
	let warm = |arm: Arm, regions: usize| {
		let shape = cdn(regions, 3, false, 1);
		// The first edge of region 1, which the publisher's core is not in.
		let grown = grow(&shape, &[(2 * regions + 3, 0)], false, arm, false);
		assert_eq!(grown.right, Ok(()), "{}", arm.name());
		grown.sent.warm()
	};
	assert_eq!(warm(USEFUL, 4), warm(USEFUL, 8));
	assert!(warm(ADVERTISE, 4) < warm(ADVERTISE, 8));
}

/// Under the pick a warm core tells none of its region's edges, and each edge queries
/// the upstreams beside its path join once on subscribe, so a first viewer's warm
/// messages stay flat as a region grows its edges and as the core mesh grows. Useful
/// adverts grow with the edges, since a warm core tells every edge it could feed.
#[test]
fn queries_toward_upstreams_do_not_grow_with_the_edges() {
	let warm = |arm: Arm, regions: usize, edges: usize| {
		let shape = cdn(regions, edges, false, 1);
		// The first edge of region 1, which the publisher's core is not in.
		let grown = grow(&shape, &[(2 * regions + edges, 0)], false, arm, false);
		assert_eq!(grown.right, Ok(()), "{}", arm.name());
		grown.sent.warm()
	};
	for arm in [NOT_DOWN_QUERY, NOT_DOWN_ASK_UP] {
		assert_eq!(warm(arm, 4, 3), warm(arm, 4, 16), "{}", arm.name());
		assert_eq!(warm(arm, 4, 3), warm(arm, 8, 3), "{}", arm.name());
	}
	assert!(warm(USEFUL, 4, 3) < warm(USEFUL, 4, 16));
}

/// An edge whose cheaper uplink is to the cold core reaches the warm core only on a
/// warm signal over an upstream link: an advert down it, or the edge's own query.
/// Skipping both leaves it on its path join.
#[test]
fn an_edge_finds_the_warm_core_beside_its_route() {
	let cost = |arm: Arm| {
		let rows = run(&unequal(), arm, &unequal_phases(), 7);
		settles(&rows, &arm.name());
		let cost: Metric = rows.iter().map(Row::cost).sum();
		let cheapest: Metric = rows.iter().flat_map(|row| row.cheapest.iter()).sum();
		(cost, cheapest)
	};
	for arm in [USEFUL, NOT_DOWN_ASK_UP, NOT_DOWN_QUERY] {
		let (cost, cheapest) = cost(arm);
		assert_eq!(cost, cheapest, "{}", arm.name());
	}
	for arm in [NOT_NEXT, NOT_DOWN, NOT_DOWN_ASK] {
		let (cost, cheapest) = cost(arm);
		assert!(cost > cheapest, "{}", arm.name());
	}
}

/// The price of taking no warm signal from a relay on the taker's own path. Relay 1
/// is warm for a reader of its own, and 3 pulls through 1 and 2 at 4. Once the
/// direct link 1 to 3 opens, 1's warm copy is 3 away, but 1 is 3's ancestor, so the
/// pick stays: its tree costs 14 where 13 is cheapest, the publisher's link
/// included. The chain, which tells 1 that 2's pull is 3's demand, takes it.
#[test]
fn an_ancestor_warm_on_its_own_is_passed_over() {
	let shape = Shape {
		name: "ancestor",
		relays: 4,
		links: vec![
			(0, 1, Price::new(11, 2), Tie::Peer),
			(1, 2, Price::new(1, 1), Tie::Peer),
			(2, 3, Price::new(1, 1), Tie::Peer),
			(1, 3, Price::new(1, 1), Tie::Peer),
		],
		home: 0,
		claims: 1,
	};
	let tree = |arm: Arm| {
		let mut sim = shape.build(arm, 7);
		sim.close(1, 3);
		sim.read(1, 0, true);
		sim.read(3, 0, true);
		assert!(sim.settle(), "{}", arm.name());
		assert_eq!(sim.tree(0).cost, 14, "{}", arm.name());
		sim.reopen(1, 3);
		assert!(sim.settle(), "{}", arm.name());
		assert_eq!(sim.check(), Ok(()), "{}", arm.name());
		(sim.tree(0).cost, sim.cheapest(0))
	};
	assert_eq!(tree(NOT_DOWN_QUERY), (14, 13));
	assert_eq!(tree(CHAIN), (13, 13));
	// A settled pull stays on 14. The new link moves its route, but the route reaches
	// 3 before 1's advert does, so the one choice the move buys finds only the route.
	assert_eq!(tree(STAY), (14, 13));
	assert_eq!(tree(REROUTE), (14, 13));
}

/// One CSV row of the growth sweeps.
struct Size {
	shape: Shape,
	/// Cores and edges per core, or the CDN relay and its drones.
	cores: usize,
	edges: usize,
	peered: bool,
	/// Where one viewer per broadcast reads, and where a crowd does.
	viewers: std::ops::Range<Node>,
	crowd: std::ops::Range<Node>,
	arms: Vec<Arm>,
	one_viewer: &'static [usize],
	crowds: &'static [usize],
}

/// Warm messages and warm state as relays and demanded broadcasts grow, for one
/// viewer per broadcast and for a crowd: CDN cores fully meshed with 3 edges per
/// core, then edges per core growing at 8 and 32 cores, with and without the edges
/// of a region peered, then full drone meshes. `MOQ_WARMTH_GROWTH` caps the
/// broadcasts.
#[test]
#[ignore = "a benchmark; Nightly runs it from test/route-sweep.sh"]
fn benchmark_warmth_growth() {
	let most: usize = std::env::var("MOQ_WARMTH_GROWTH").map_or(4096, |most| most.parse().expect("MOQ_WARMTH_GROWTH"));
	println!(
		"shape,cores,edges_per_core,peered,relays,neighbours,broadcasts,load,arm,reader_changes,messages,warm_messages,per_change_warm_messages,held_per_core,held_per_edge,held_at_most,tree_cost,cheapest_cost,first_frame_ms,right"
	);
	let mut sizes = Vec::new();
	let region = |regions: usize, edges: usize, peered: bool, arms: Vec<Arm>, wide: bool| {
		let cores = 2 * regions;
		Size {
			shape: cdn(regions, edges, peered, 1),
			cores,
			edges,
			peered,
			viewers: cores..cores + regions * edges,
			// A crowd in region 1, which the publisher's core is not in.
			crowd: cores + edges..cores + 2 * edges,
			arms,
			one_viewer: if wide { &[256] } else { &[1, 16, 256, 4096] },
			crowds: if wide { &[16] } else { &[1, 4, 16] },
		}
	};
	for regions in [2, 4, 8, 16] {
		sizes.push(region(regions, 3, false, growing(), false));
	}
	for regions in [4, 16] {
		for edges in [3, 16, 64, 128] {
			for peered in [false, true] {
				// A region of a hundred edges does not peer every pair, so the full mesh
				// is measured only while it stays small.
				if peered && edges * regions > 256 {
					continue;
				}
				sizes.push(region(regions, edges, peered, widening(), true));
			}
		}
	}
	for count in [5, 16, 64] {
		sizes.push(Size {
			shape: drones(count),
			cores: 1,
			edges: count,
			peered: true,
			viewers: 1..count + 1,
			crowd: 1..count + 1,
			arms: growing(),
			one_viewer: &[1, 16, 256, 4096],
			crowds: &[1, 4, 16],
		});
	}
	for size in sizes {
		for load in [Load::OneViewer, Load::Crowd] {
			let (counts, relays) = match load {
				Load::OneViewer => (size.one_viewer, size.viewers.clone()),
				Load::Crowd => (size.crowds, size.crowd.clone()),
			};
			for &claims in counts.iter().filter(|&&claims| claims <= most) {
				let shape = Shape {
					claims,
					..size.shape.clone()
				};
				let reads = load.readers(relays.clone(), claims, 0x6772_6f77 + claims as u64);
				let neighbours = shape.build(PATH, 11).degree();
				let mut cheapest = None;
				for &arm in &size.arms {
					let grown = grow(&shape, &reads, load == Load::Crowd, arm, cheapest.is_none());
					let cheapest = *cheapest.get_or_insert(grown.cheapest);
					println!(
						"{},{},{},{},{},{neighbours},{claims},{},{},{},{},{},{:.2},{:.2},{:.2},{},{},{cheapest},{:.0},{}",
						shape.name,
						size.cores,
						size.edges,
						size.peered,
						shape.relays,
						load.name(),
						arm.name(),
						grown.changes,
						grown.sent.total(),
						grown.sent.warm(),
						grown.sent.warm() as f64 / grown.changes as f64,
						grown.held(0..size.cores),
						grown.held(size.cores..shape.relays),
						grown.held.iter().max().unwrap_or(&0),
						grown.cost,
						grown.first * HOP.as_secs_f64() * 1000.0,
						match grown.right {
							Ok(()) => "yes".into(),
							Err(err) => err.replace(',', ";"),
						},
					);
				}
			}
		}
	}
}

// --- Churn ---

/// What churn showed for one arm, summed over its cases.
#[derive(Clone, Debug, Default)]
struct Churned {
	cases: usize,
	watch: Watch,
	sent: Sent,
	changes: usize,
	/// Cases that never settled or settled wrong once churn stopped, and the first.
	failed: usize,
	first: Option<String>,
	/// Rounds each case took to settle once churn stopped, summed.
	settle: usize,
	/// The cost of the trees each case settled on, and of the cheapest trees for the
	/// same readers, summed over the cases that settled right.
	tree: Metric,
	cheapest: Metric,
}

/// A random mesh of four to seven relays, a chain and about two in five of the
/// other links, each with a random cold price up to 6 and warm price up to that.
fn random(rng: &mut XorShift) -> Shape {
	let relays = 4 + rng.next(4) as usize;
	let mut links = Vec::new();
	for a in 0..relays {
		for b in a + 1..relays {
			if b == a + 1 || rng.chance(40) {
				let cold = 1 + rng.next(6);
				links.push((a, b, Price::new(cold, 1 + rng.next(cold)), Tie::Peer));
			}
		}
	}
	Shape {
		name: "random",
		relays,
		home: rng.next(relays as u64) as usize,
		links,
		claims: 2,
	}
}

/// Readers come and go, links flap, relays restart, and every message is delivered
/// in a random order: 300 steps a case, the same for every arm. A step is one
/// delivery, so a hop of delay is eight of them. Once churn stops the mesh must
/// settle with every reader fed and no pulls circling.
fn churn(arm: Arm, cases: std::ops::Range<usize>, shape: fn(&mut XorShift) -> Shape) -> Churned {
	let mut out = Churned::default();
	for case in cases {
		let mut script = XorShift(0x0bad_c0de + case as u64 * 0x9e37_79b9);
		let mut pick = XorShift(0x5eed_0002 + case as u64);
		let shape = shape(&mut script);
		let mut sim = shape.build(arm, 0x5eed_0003 + case as u64);
		// Far beyond what any mesh here settles in, so only a storm that keeps growing.
		sim.budget = Some(1_000_000);
		let (sent, watch) = (sim.sent, sim.watch);
		for _ in 0..2 {
			let relay = script.next(shape.relays as u64) as usize;
			sim.read(relay, 0, true);
		}
		for step in 0..300 {
			match script.next(20) {
				0..=2 => {
					let relay = script.next(shape.relays as u64) as usize;
					let cast = script.next(shape.claims as u64) as usize;
					let reading = !sim.reading(relay, cast);
					out.changes += 1;
					sim.read(relay, cast, reading);
				}
				3 => {
					let (a, b, ..) = shape.links[script.next(shape.links.len() as u64) as usize];
					match sim.is_open(a, b) {
						true => sim.close(a, b),
						false => sim.reopen(a, b),
					}
				}
				4 if case % 4 == 0 && script.chance(30) => sim.restart(script.next(shape.relays as u64) as usize),
				_ => {
					sim.deliver(&mut pick);
				}
			}
			if step % 8 == 7 {
				sim.tick();
			}
		}
		let seen = sim.watch.since(watch);
		let round = sim.round;
		let settled = sim.settle();
		out.settle += sim.round - round;
		let right = match settled {
			true => sim.check(),
			false => Err("never settled".into()),
		};
		if right.is_ok() {
			out.tree += (0..shape.claims).map(|cast| sim.tree(cast).cost).sum::<Metric>();
			out.cheapest += (0..shape.claims).map(|cast| sim.cheapest(cast)).sum::<Metric>();
		}
		if let Err(err) = right {
			out.failed += 1;
			out.first
				.get_or_insert_with(|| format!("case {case}: {err} on {shape:?}\n{}", sim.dump()));
		}
		out.cases += 1;
		out.watch.steps += seen.steps;
		out.watch.cycles += seen.cycles;
		out.watch.starved += seen.starved;
		out.watch.merged += seen.merged;
		out.watch.longest = out.watch.longest.max(seen.longest);
		out.watch.unfed += seen.unfed;
		out.watch.extra += seen.extra;
		out.sent = out.sent + (sim.sent - sent);
	}
	out
}

/// Drones with readers coming and going and uplinks flapping.
/// A random CDN region: two or three cores fully meshed, and two to four edges, each
/// marking two cores upstream at random prices, some peering each other.
fn regional(rng: &mut XorShift) -> Shape {
	let cores = 2 + rng.next(2) as usize;
	let edges = 2 + rng.next(3) as usize;
	let mut links = Vec::new();
	let price = |rng: &mut XorShift, low: u64, span: u64| {
		let cold = low + rng.next(span);
		Price::new(cold, 1 + rng.next(cold))
	};
	for a in 0..cores {
		for b in a + 1..cores {
			links.push((a, b, price(rng, 4, 4), Tie::Peer));
		}
	}
	for edge in cores..cores + edges {
		let first = rng.next(cores as u64) as usize;
		for core in [first, (first + 1) % cores] {
			links.push((edge, core, price(rng, 2, 4), Tie::Up));
		}
		for other in edge + 1..cores + edges {
			if rng.chance(30) {
				links.push((edge, other, price(rng, 1, 3), Tie::Peer));
			}
		}
	}
	Shape {
		name: "regional",
		relays: cores + edges,
		home: rng.next((cores + edges) as u64) as usize,
		links,
		claims: 2,
	}
}

fn flying(rng: &mut XorShift) -> Shape {
	drones(3 + rng.next(3) as usize)
}

/// Under churn every arm settles once it stops, with every reader fed and no pulls
/// circling: the cycles it formed lasted only while messages were in flight.
#[test]
fn every_arm_settles_after_churn() {
	for arm in [
		PATH,
		ASK,
		ASK_MERGE,
		ADVERTISE,
		ADVERTISE_MERGE,
		USEFUL,
		NOT_DOWN_ASK_UP,
		NOT_DOWN_QUERY,
		CHAIN,
		STAY,
		REROUTE,
	] {
		for shape in [random as fn(&mut XorShift) -> Shape, flying, regional] {
			let out = churn(arm, 0..12, shape);
			assert_eq!(out.failed, 0, "{}: {:?}", arm.name(), out.first);
		}
	}
}

/// One viewer per broadcast on CDN tiers: a join that follows its route costs no
/// message beyond its subscriptions, and both ways to hear of warmth spend some on
/// every first viewer.
#[test]
fn a_first_viewer_costs_warm_messages_unless_joins_follow_the_route() {
	let case = cases()
		.into_iter()
		.find(|case| case.name == "tiers_one_viewer")
		.expect("the case");
	for arm in [PATH, ASK, ADVERTISE] {
		let rows = run(&case.shape, arm, &case.phases, 7);
		settles(&rows, &arm.name());
		for row in &rows {
			match arm.mode {
				Mode::Path => assert_eq!(row.sent.warm(), 0, "{}", arm.name()),
				_ => assert!(row.sent.warm() > 0, "{}", arm.name()),
			}
		}
	}
}

/// On CDN tiers two edges under one core cost the same, and a merge between them
/// leaves the core with one child, whose offer then rises: without hops to tell a
/// sibling the merge undoes itself for good. With them it settles on a cheaper tree.
#[test]
fn a_merge_settles_on_tiers_once_offers_name_their_hops() {
	let case = cases()
		.into_iter()
		.find(|case| case.name == "tiers_crowd")
		.expect("the case");
	for (alone, merged) in [(ASK, ASK_MERGE), (ADVERTISE, ADVERTISE_MERGE)] {
		let cost = |arm: Arm| {
			let rows = run(&case.shape, arm, &case.phases, 7);
			settles(&rows, &arm.name());
			rows.iter().map(Row::cost).sum::<Metric>()
		};
		assert!(cost(merged) <= cost(alone), "{}", merged.name());
	}
}

// --- The tables ---

/// One measured script on one shape.
#[derive(Clone)]
struct Case {
	name: String,
	shape: Shape,
	phases: Vec<Phase>,
}

fn case(name: &str, shape: Shape, phases: Vec<Phase>) -> Case {
	Case {
		name: name.into(),
		shape,
		phases,
	}
}

/// Every case the quest names, sized as it does.
fn cases() -> Vec<Case> {
	let regions = 3;
	let per = 3;
	let first = first_edge(regions);
	let last = first + regions * per;
	let mut rng = XorShift(0x7761_726d);
	let single: Vec<Phase> = (0..16)
		.map(|cast| {
			let relay = first + rng.next((last - first) as u64) as usize;
			phase("one_viewer", vec![Step::Read(relay, cast, true)])
		})
		.collect();
	let at_once = |gap: usize| -> Vec<Phase> {
		let mut steps = Vec::new();
		for drone in 1..=4 {
			steps.push(Step::Read(drone, 0, true));
			steps.push(Step::Pass(gap));
		}
		vec![phase("drones_read_a_gap_apart", steps)]
	};
	vec![
		case("pops", pops(), pop_phases()),
		case("drones_in_turn", drones(5), drone_phases(5, false)),
		case("drones_at_once", drones(5), drone_phases(5, true)),
		case("drones_one_hop_apart", drones(4), at_once(1)),
		case("drones_two_hops_apart", drones(4), at_once(2)),
		case(
			"edges_in_turn",
			edges(),
			vec![
				phase("near_core_edge", vec![Step::Read(3, 0, true)]),
				phase("other_edge", vec![Step::Read(4, 0, true)]),
			],
		),
		case(
			"edges_at_once",
			edges(),
			vec![phase(
				"both_edges",
				vec![Step::Read(3, 0, true), Step::Read(4, 0, true)],
			)],
		),
		case("ring_in_turn", ring(6, 8), ring_phases(6, 8, false)),
		case("ring_at_once", ring(6, 8), ring_phases(6, 8, true)),
		case(
			"tiers_crowd",
			tiers(regions, per, 4),
			crowd(first..last, 4, 0x6372_6f77),
		),
		case("tiers_one_viewer", tiers(regions, per, 16), single),
		case("region_unequal", unequal(), unequal_phases()),
	]
}

/// Each edge of [`unequal`] reads in turn.
fn unequal_phases() -> Vec<Phase> {
	vec![
		phase("first_edge", vec![Step::Read(3, 0, true)]),
		phase("second_edge", vec![Step::Read(4, 0, true)]),
	]
}

/// One region of a CDN: the publisher's core 0, the region's cores 1 and 2, and its
/// edges 3 and 4, each with its cheaper uplink to a different core. No hash sends
/// both to one core; only a warm signal over an upstream link can.
fn unequal() -> Shape {
	Shape {
		name: "unequal",
		relays: 5,
		links: vec![
			(0, 1, Price::new(6, 2), Tie::Peer),
			(0, 2, Price::new(6, 2), Tie::Peer),
			(1, 2, Price::new(6, 2), Tie::Peer),
			(3, 1, Price::new(3, 1), Tie::Up),
			(3, 2, Price::new(4, 1), Tie::Up),
			(4, 1, Price::new(4, 1), Tie::Up),
			(4, 2, Price::new(3, 1), Tie::Up),
		],
		home: 0,
		claims: 1,
	}
}

/// The drone shape's last phase closes the uplink the arm settled on, or the first
/// reader's where the arm keeps more than one, as path joins keep one per drone.
fn takeover(arm: Arm) -> Case {
	let mut phases = drone_phases(5, false);
	let holder = uplinked(&run(&drones(5), arm, &phases, 4)).unwrap_or(1);
	phases.push(phase("its_uplink_closes", vec![Step::Close(0, holder)]));
	case("drones_lose_the_uplink", drones(5), phases)
}

fn mean(values: &[usize]) -> f64 {
	match values.len() {
		0 => 0.0,
		count => values.iter().sum::<usize>() as f64 / count as f64,
	}
}

/// Per case and arm: what the trees cost against the path joins and the cheapest,
/// summed over every phase; the copies left at the end; messages; the wait for a
/// first frame; and cycles and unfed readers while it moved.
#[test]
#[ignore = "a benchmark; Nightly runs it from test/route-sweep.sh"]
fn benchmark_warmth_cases() {
	println!(
		"case,arm,phases,tree_cost,path_cost,cheapest_cost,uplinks,max_copies,carriers,messages,warm_messages,messages_per_reader_change,warm_per_reader_change,first_frame_ms,cycle_steps,longest_cycle,unfed_reader_rounds,advert_messages,query_messages,chain_messages,right"
	);
	let ms = |rounds: f64| rounds * HOP.as_secs_f64() * 1000.0;
	for base in cases().into_iter().map(Some).chain([None]) {
		let mut path_cost = None;
		for arm in arms().into_iter().chain(audiences()) {
			let case = base.as_ref().map_or_else(|| takeover(arm), |case| case.clone());
			let rows = run(&case.shape, arm, &case.phases, 7);
			let sum = |field: fn(&Row) -> usize| rows.iter().map(field).sum::<usize>();
			let cost: Metric = rows.iter().map(Row::cost).sum();
			let cheapest: Metric = rows.iter().flat_map(|row| row.cheapest.iter()).sum();
			if arm == PATH {
				path_cost = Some(cost);
			}
			let sent = rows.iter().fold(Sent::default(), |all, row| all + row.sent);
			let changes = sum(|row| row.changes).max(1) as f64;
			let waits: Vec<usize> = rows.iter().flat_map(|row| row.waits.iter().copied()).collect();
			let end = rows.last().expect("a phase");
			let right = rows.iter().find_map(|row| row.right.clone().err());
			println!(
				"{},{},{},{cost},{},{cheapest},{},{},{},{},{},{:.2},{:.2},{:.0},{},{},{},{},{},{},{}",
				case.name,
				arm.name(),
				rows.len(),
				path_cost.map_or("-".into(), |cost| cost.to_string()),
				end.uplinks(),
				end.copies(),
				end.carriers.iter().map(Vec::len).sum::<usize>(),
				sent.total(),
				sent.warm(),
				sent.total() as f64 / changes,
				sent.warm() as f64 / changes,
				ms(mean(&waits)),
				sum(|row| row.watch.cycles),
				rows.iter().map(|row| row.watch.longest).max().unwrap_or(0),
				sum(|row| row.watch.unfed),
				sent.adverts,
				sent.asks + sent.told,
				sent.chains,
				right.map_or("yes".into(), |err| err.replace(',', ";")),
			);
		}
	}
}

/// Per arm, random meshes, drone meshes, and CDN regions under churn: cycles, unfed readers, and
/// standing races while it churned, and whether each settled right once it stopped.
#[test]
#[ignore = "a benchmark; Nightly runs it from test/route-sweep.sh"]
fn benchmark_warmth_churn() {
	println!(
		"mesh,arm,cases,deliveries,cycle_deliveries,starved_deliveries,merge_cycle_deliveries,longest_cycle,unfed_reader_hops,extra_uplink_hops,reader_changes,messages,warm_messages,advert_messages,query_messages,chain_messages,settle_rounds,settled_tree_cost,settled_cheapest_cost,failed"
	);
	let cases: usize = std::env::var("MOQ_WARMTH_CASES").map_or(400, |cases| cases.parse().expect("MOQ_WARMTH_CASES"));
	// A comma-separated list of arm names to run, for a longer sweep of a few.
	let only = std::env::var("MOQ_WARMTH_ARMS").ok();
	let picked = |arm: &Arm| {
		only.as_ref()
			.is_none_or(|only| only.split(',').any(|name| name == arm.name()))
	};
	for (name, shape) in [
		("random", random as fn(&mut XorShift) -> Shape),
		("drones", flying),
		("regional", regional),
	] {
		for arm in arms().into_iter().chain(audiences()).filter(picked) {
			let out = churn(arm, 0..cases, shape);
			println!(
				"{name},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
				arm.name(),
				out.cases,
				out.watch.steps,
				out.watch.cycles,
				out.watch.starved,
				out.watch.merged,
				out.watch.longest,
				out.watch.unfed,
				out.watch.extra,
				out.changes,
				out.sent.total(),
				out.sent.warm(),
				out.sent.adverts,
				out.sent.asks + out.sent.told,
				out.sent.chains,
				out.settle,
				out.tree,
				out.cheapest,
				out.first.map_or("0".into(), |first| format!(
					"{}: {}",
					out.failed,
					first.replace(',', ";")
				)),
			);
		}
	}
}

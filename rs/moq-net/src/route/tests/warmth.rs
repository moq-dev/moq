//! Three ways to find a warm copy, measured on [`WarmSim`] over the multicast
//! prototype's route rule (`quest/m2/multicast/warmth.md`).
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
		});
	}
	for mode in [Mode::Ask(1), Mode::Ask(2), Mode::Ask(usize::MAX), Mode::Advertise] {
		for hops in [true, false] {
			for merge in [false, true] {
				out.push(Arm { mode, hops, merge });
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
	}
}

const PATH: Arm = arm(Mode::Path, false);
const ASK: Arm = arm(Mode::Ask(usize::MAX), false);
const ASK_MERGE: Arm = arm(Mode::Ask(usize::MAX), true);
const ADVERTISE: Arm = arm(Mode::Advertise, false);
const ADVERTISE_MERGE: Arm = arm(Mode::Advertise, true);

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
	settled: bool,
	right: Result<(), String>,
	trees: Vec<Tree>,
	/// The cheapest tree per broadcast.
	cheapest: Vec<Metric>,
	sent: Sent,
	/// Reader starts and stops in the phase.
	changes: usize,
	rounds: usize,
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
		let (sent, watch, round, waits) = (sim.sent, sim.watch, sim.round, sim.waits.len());
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
		let settled = sim.settle();
		let casts = 0..sim.casts();
		rows.push(Row {
			phase: phase.name,
			settled,
			right: if settled {
				sim.check()
			} else {
				Err("never settled".into())
			},
			trees: casts.clone().map(|cast| sim.tree(cast)).collect(),
			cheapest: casts.clone().map(|cast| sim.cheapest(cast)).collect(),
			sent: sim.sent - sent,
			changes,
			rounds: sim.round - round,
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
	for arm in arms() {
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
		match arm.mode {
			Mode::Path => assert_eq!(edges, skip, "{case}"),
			// One query goes to the cheapest link, which for the middle pop is the near
			// pop's and for the far pop the middle's.
			_ => assert_eq!(edges, joined, "{case}"),
		}
		// The near pop's reader leaves and the middle pop returns to its long link.
		let near = last(&rows, "near_pop_leaves");
		assert!(
			near.trees[0].edges.contains(&(0, 2)),
			"{case}: {:?}",
			near.trees[0].edges
		);
		assert!(near.carriers[0].is_empty(), "{case}: {:?}", near.carriers[0]);
	}
}

/// Drones that read one after another share one uplink when they can hear of each
/// other, and path joins take one each.
#[test]
fn drones_reading_in_turn_share_one_uplink() {
	for arm in arms() {
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
		if right.is_err() && std::env::var("WARM_DUMP").is_ok() {
			println!("{} case {case} on {shape:?}\n{}", arm.name(), sim.dump());
		}
		if let Err(err) = right {
			out.failed += 1;
			out.first.get_or_insert(format!("case {case}: {err}"));
		}
		out.cases += 1;
		out.watch.steps += seen.steps;
		out.watch.cycles += seen.cycles;
		out.watch.starved += seen.starved;
		out.watch.longest = out.watch.longest.max(seen.longest);
		out.watch.unfed += seen.unfed;
		out.watch.extra += seen.extra;
		out.sent = out.sent + (sim.sent - sent);
	}
	out
}

/// Drones with readers coming and going and uplinks flapping.
fn flying(rng: &mut XorShift) -> Shape {
	drones(3 + rng.next(3) as usize)
}

/// Under churn every arm settles once it stops, with every reader fed and no pulls
/// circling: the cycles it formed lasted only while messages were in flight.
#[test]
fn every_arm_settles_after_churn() {
	for arm in [PATH, ASK, ASK_MERGE, ADVERTISE, ADVERTISE_MERGE] {
		for shape in [random as fn(&mut XorShift) -> Shape, flying] {
			let out = churn(arm, 0..12, shape);
			assert_eq!(out.failed, 0, "{}: {:?}", arm.name(), out.first);
		}
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
	]
}

/// The drone shape's last phase closes the uplink the arm settled on.
fn takeover(arm: Arm) -> Case {
	let mut phases = drone_phases(5, false);
	let holder = uplinked(&run(&drones(5), arm, &phases, 4));
	if let Some(holder) = holder {
		phases.push(phase("its_uplink_closes", vec![Step::Close(0, holder)]));
	}
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
		"case,arm,phases,tree_cost,path_cost,cheapest_cost,uplinks,max_copies,carriers,messages,warm_messages,messages_per_reader_change,warm_per_reader_change,first_frame_ms,cycle_steps,longest_cycle,unfed_reader_rounds,right"
	);
	let ms = |rounds: f64| rounds * HOP.as_secs_f64() * 1000.0;
	for base in cases().into_iter().map(Some).chain([None]) {
		let mut path_cost = None;
		for arm in arms() {
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
				"{},{},{},{cost},{},{cheapest},{},{},{},{},{},{:.2},{:.2},{:.0},{},{},{},{}",
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
				right.map_or("yes".into(), |err| err.replace(',', ";")),
			);
		}
	}
}

/// Per arm, random meshes and drone meshes under churn: cycles, unfed readers, and
/// standing races while it churned, and whether each settled right once it stopped.
#[test]
#[ignore = "a benchmark; Nightly runs it from test/route-sweep.sh"]
fn benchmark_warmth_churn() {
	println!(
		"mesh,arm,cases,deliveries,cycle_deliveries,starved_deliveries,longest_cycle,unfed_reader_hops,extra_uplink_hops,reader_changes,messages,warm_messages,settle_rounds,failed"
	);
	let cases: usize = std::env::var("MOQ_WARMTH_CASES").map_or(400, |cases| cases.parse().expect("MOQ_WARMTH_CASES"));
	for (name, shape) in [("random", random as fn(&mut XorShift) -> Shape), ("drones", flying)] {
		for arm in arms() {
			let out = churn(arm, 0..cases, shape);
			println!(
				"{name},{},{},{},{},{},{},{},{},{},{},{},{},{}",
				arm.name(),
				out.cases,
				out.watch.steps,
				out.watch.cycles,
				out.watch.starved,
				out.watch.longest,
				out.watch.unfed,
				out.watch.extra,
				out.changes,
				out.sent.total(),
				out.sent.warm(),
				out.settle,
				out.first.map_or("0".into(), |first| format!(
					"{}: {}",
					out.failed,
					first.replace(',', ";")
				)),
			);
		}
	}
}

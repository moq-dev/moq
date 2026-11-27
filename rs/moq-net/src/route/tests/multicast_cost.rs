//! What one event costs a relay under [`relay::Router`], swept along each axis it could
//! fan out over: the forwarding nodes it routes to, the publishers behind each, the
//! broadcasts each publisher claims, and the sessions it holds: upstream, each neighbour
//! offering every route; downstream, each sent every route; and endpoints watching, each
//! asking for the whole tree or for a prefix no claim falls under. A sweep holds the other
//! axes fixed and counts the measured relay's work and messages, so an event that visits
//! what it did not change fails here rather than only timing slower: `cost.rs` for the
//! rule the switch puts lite-07 sessions on. The events: a claim announced, a route the
//! relay does not use breaking, the one it uses going round dearer, a downstream session
//! leaving, and a copy of a forwarding node's claims overtaking the relay's.

use crate::route::relay;

use super::multi::Tie;
use super::multicast::{RelaySim, quiesce};
use super::*;

/// The measured relay.
const ME: Node = 0;

#[derive(Clone, Copy, Debug)]
struct Shape {
	/// Forwarding nodes, each a relay its publishers join.
	origins: usize,
	/// Publishers on each forwarding node.
	publishers: usize,
	/// Broadcasts each publisher claims.
	broadcasts: usize,
	/// Relays between the measured one and every forwarding node.
	upstream: usize,
	/// Relays the measured one alone reaches the mesh through.
	downstream: usize,
	/// Endpoints on the measured relay that ask it for something.
	watchers: usize,
	/// Whether each watcher asks for the whole tree, or a prefix no claim falls under.
	watched: bool,
}

const BASE: Shape = Shape {
	origins: 4,
	publishers: 2,
	broadcasts: 2,
	upstream: 2,
	downstream: 2,
	watchers: 2,
	watched: true,
};

/// `BASE` with no watcher asking for any claim, so a route's cost reaches nobody's view.
const QUIET: Shape = Shape { watched: false, ..BASE };

/// What one event cost the measured relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cost {
	/// What it visited.
	work: usize,
	/// Messages it sent.
	sent: usize,
}

/// A mesh shaped as a [`Shape`], settled.
struct Bed {
	sim: RelaySim,
	shape: Shape,
	/// The next broadcast the first publisher claims.
	next: usize,
}

impl Bed {
	fn up(&self, index: usize) -> Node {
		1 + index
	}

	fn origin(&self, index: usize) -> Node {
		1 + self.shape.upstream + index
	}

	fn down(&self, index: usize) -> Node {
		1 + self.shape.upstream + self.shape.origins + index
	}

	fn publisher(&self, origin: usize, index: usize) -> Node {
		1 + self.shape.upstream + self.shape.origins + self.shape.downstream + origin * self.shape.publishers + index
	}

	fn watcher(&self, index: usize) -> Node {
		self.publisher(self.shape.origins, 0) + index
	}

	/// Built with every router rebuilding its indexes after each input, if `checked`.
	fn new(shape: Shape, checked: bool) -> Self {
		let nodes =
			1 + shape.upstream + shape.origins + shape.downstream + shape.origins * shape.publishers + shape.watchers;
		let mut bed = Self {
			sim: RelaySim::new(nodes, relay::debounce(HOP)),
			shape,
			next: 0,
		};
		if !checked {
			bed.sim.unchecked();
		}
		for origin in 0..shape.origins {
			for index in 0..shape.publishers {
				let publisher = bed.publisher(origin, index);
				bed.sim.client(publisher);
				bed.sim.publish(publisher, publisher as Id);
			}
		}
		for up in 0..shape.upstream {
			// The first is so much cheaper that the measured relay keeps to it even when
			// its route goes round.
			let cost = if up == 0 { 1 } else { 8 };
			bed.sim.link(ME, bed.up(up), cost, Tie::Peer);
			for origin in 0..shape.origins {
				bed.sim.link(bed.up(up), bed.origin(origin), 1, Tie::Peer);
			}
		}
		for down in 0..shape.downstream {
			bed.sim.link(ME, bed.down(down), 1, Tie::Peer);
		}
		for origin in 0..shape.origins {
			for index in 0..shape.publishers {
				let publisher = bed.publisher(origin, index);
				bed.sim.link(publisher, bed.origin(origin), 1, Tie::Peer);
				for broadcast in 0..shape.broadcasts {
					bed.sim
						.claim(publisher, &format!("o{origin}/p{index}/b{broadcast}"), true);
				}
			}
		}
		for index in 0..shape.watchers {
			let watcher = bed.watcher(index);
			bed.sim.client(watcher);
			bed.sim.link(watcher, ME, 1, Tie::Peer);
			bed.sim.ask(watcher, ME, if shape.watched { "" } else { "none" });
		}
		quiesce(&mut bed.sim);
		bed
	}

	fn measure(&mut self, change: impl FnOnce(&mut Self)) -> Cost {
		let router = |bed: &Self| bed.sim.router(ME).expect("the measured relay").work_done();
		let sent = |bed: &Self| bed.sim.sent.get(&ME).copied().unwrap_or(0);
		let (work, was) = (router(self), sent(self));
		change(self);
		quiesce(&mut self.sim);
		Cost {
			work: router(self) - work,
			sent: sent(self) - was,
		}
	}

	/// The first publisher claims one more broadcast.
	fn announce(&mut self) -> Cost {
		let (publisher, claim) = (self.publisher(0, 0), format!("o0/p0/new{}", self.next));
		self.next += 1;
		self.measure(|bed| bed.sim.claim(publisher, &claim, true))
	}

	/// The last upstream relay's session to the first forwarding node closes, then reopens
	/// unmeasured: a route the measured relay does not use breaks.
	fn cut(&mut self) -> Cost {
		self.sever(self.shape.upstream - 1)
	}

	/// The first upstream relay's session to the first forwarding node closes, then
	/// reopens unmeasured: the route the measured relay uses goes round through another
	/// forwarding node, dearer, on the same next hop.
	fn detour(&mut self) -> Cost {
		self.sever(0)
	}

	fn sever(&mut self, up: usize) -> Cost {
		let (up, origin) = (self.up(up), self.origin(0));
		let cost = self.measure(|bed| bed.sim.close(up, origin));
		self.sim.open(up, origin);
		quiesce(&mut self.sim);
		cost
	}

	/// The first downstream relay's session closes, then reopens unmeasured.
	fn leave(&mut self) -> Cost {
		let down = self.down(0);
		let cost = self.measure(|bed| bed.sim.close(ME, down));
		self.sim.open(ME, down);
		quiesce(&mut self.sim);
		cost
	}
}

/// The cost of `event` on meshes shaped as `BASE` with one axis set to each size,
/// measured after one warm-up event so first-time effects are left out.
fn sweep(axis: fn(&mut Shape, usize), sizes: [usize; 3], event: fn(&mut Bed) -> Cost) -> [Cost; 3] {
	sweep_at(BASE, axis, sizes, event, true)
}

/// [`sweep`] from `QUIET`.
fn sweep_quiet(axis: fn(&mut Shape, usize), sizes: [usize; 3], event: fn(&mut Bed) -> Cost) -> [Cost; 3] {
	sweep_at(QUIET, axis, sizes, event, true)
}

fn sweep_at<const N: usize>(
	base: Shape,
	axis: fn(&mut Shape, usize),
	sizes: [usize; N],
	event: fn(&mut Bed) -> Cost,
	checked: bool,
) -> [Cost; N] {
	sizes.map(|size| {
		let mut shape = base;
		axis(&mut shape, size);
		let mut bed = Bed::new(shape, checked);
		event(&mut bed);
		event(&mut bed)
	})
}

fn origins(shape: &mut Shape, n: usize) {
	shape.origins = n;
}

fn publishers(shape: &mut Shape, n: usize) {
	shape.publishers = n;
}

fn broadcasts(shape: &mut Shape, n: usize) {
	shape.broadcasts = n;
}

fn upstream(shape: &mut Shape, n: usize) {
	shape.upstream = n;
}

fn downstream(shape: &mut Shape, n: usize) {
	shape.downstream = n;
}

fn watched(shape: &mut Shape, n: usize) {
	shape.watchers = n;
}

fn unwatched(shape: &mut Shape, n: usize) {
	shape.watchers = n;
	shape.watched = false;
}

/// One of each, a few, and many.
const FLAT: [usize; 3] = [1, 4, 16];
/// From two forwarding nodes, so a route that breaks has another to go round through.
const AROUND: [usize; 3] = [2, 4, 16];
/// Doublings, for axes that may grow linearly. Two upstream at least, so a cut leaves the
/// measured relay a route.
const LINEAR: [usize; 3] = [4, 8, 16];

fn assert_flat(name: &str, costs: [Cost; 3]) {
	assert!(
		costs.iter().all(|cost| *cost == costs[0]),
		"{name} must not grow: {costs:?}"
	);
}

fn assert_linear(name: &str, costs: [Cost; 3]) {
	for pair in costs.windows(2) {
		// Quadratic work would quadruple.
		assert!(
			pair[1].work <= 5 * pair[0].work / 2,
			"{name} grows faster than linear: {costs:?}"
		);
	}
}

/// A claim announced behind a forwarding node costs the relay the sessions it hears it on
/// and the sessions it tells, and nothing per other forwarding node, publisher, broadcast,
/// or watcher whose prefix it misses: the change travels as itself, never as a resend of
/// the claims beside it.
#[test]
fn an_announce_costs_the_sessions_it_reaches() {
	assert_flat("forwarding nodes", sweep(origins, FLAT, Bed::announce));
	assert_flat("publishers", sweep(publishers, FLAT, Bed::announce));
	assert_flat("broadcasts", sweep(broadcasts, FLAT, Bed::announce));
	assert_flat("watchers it misses", sweep(unwatched, FLAT, Bed::announce));
	assert_linear("upstream", sweep(upstream, LINEAR, Bed::announce));
	assert_linear("downstream", sweep(downstream, LINEAR, Bed::announce));
	assert_linear("watchers it reaches", sweep(watched, LINEAR, Bed::announce));
}

/// A neighbour's route that breaks, one the relay does not use, costs it the neighbours
/// offering the same route, which it ranks, and nothing per other forwarding node,
/// publisher, broadcast, or session it sends to.
#[test]
fn a_cut_costs_the_neighbours_offering_the_route() {
	assert_flat("forwarding nodes", sweep(origins, AROUND, Bed::cut));
	assert_flat("publishers", sweep(publishers, FLAT, Bed::cut));
	assert_flat("broadcasts", sweep(broadcasts, FLAT, Bed::cut));
	assert_flat("downstream", sweep(downstream, FLAT, Bed::cut));
	assert_flat("watchers", sweep(watched, FLAT, Bed::cut));
	assert_linear("upstream", sweep(upstream, LINEAR, Bed::cut));
}

/// A route the relay uses that grows dearer costs it the neighbours offering the route
/// and the ones it tells, and the claims behind it only where the cost reaches a view:
/// nothing per publisher or broadcast while no watcher is shown them, and each broadcast
/// once per watcher shown its new cost.
#[test]
fn a_detour_costs_the_claims_whose_cost_is_shown() {
	assert_flat("forwarding nodes", sweep_quiet(origins, AROUND, Bed::detour));
	assert_flat("publishers", sweep_quiet(publishers, FLAT, Bed::detour));
	assert_flat("broadcasts", sweep_quiet(broadcasts, FLAT, Bed::detour));
	assert_flat("watchers it misses", sweep_quiet(unwatched, FLAT, Bed::detour));
	assert_linear("upstream", sweep_quiet(upstream, LINEAR, Bed::detour));
	assert_linear("downstream", sweep_quiet(downstream, LINEAR, Bed::detour));
	assert_flat("forwarding nodes, watched", sweep(origins, AROUND, Bed::detour));
	assert_linear("publishers, watched", sweep(publishers, LINEAR, Bed::detour));
	assert_linear("broadcasts, watched", sweep(broadcasts, LINEAR, Bed::detour));
	assert_linear("watchers it reaches", sweep(watched, LINEAR, Bed::detour));
}

/// A neighbour's copy of a forwarding node's claims that overtakes the relay's own, as
/// when the neighbour it heard them from lags, costs what the two copies hold, never
/// their product: every claim replaced, read back from the snapshot a new neighbour
/// sends.
#[test]
fn an_overtaking_copy_costs_the_claims_it_holds() {
	assert_linear("claims", [8, 16, 32].map(overtake));
}

fn overtake(claims: usize) -> Cost {
	const R: Node = 0;
	const LAGGING: Node = 1;
	const ORIGIN: Node = 2;
	const FRESH: Node = 3;
	const PUBLISHER: Node = 4;
	let mut sim = RelaySim::new(5, relay::debounce(HOP));
	sim.client(PUBLISHER);
	sim.publish(PUBLISHER, PUBLISHER as Id);
	sim.link(R, LAGGING, 1, Tie::Peer);
	sim.link(LAGGING, ORIGIN, 1, Tie::Peer);
	sim.link(FRESH, ORIGIN, 1, Tie::Peer);
	sim.link(PUBLISHER, ORIGIN, 1, Tie::Peer);
	for index in 0..claims {
		sim.claim(PUBLISHER, &format!("old{index}"), true);
	}
	quiesce(&mut sim);
	sim.hold(LAGGING, R);
	for index in 0..claims {
		sim.claim(PUBLISHER, &format!("old{index}"), false);
		sim.claim(PUBLISHER, &format!("new{index}"), true);
	}
	quiesce(&mut sim);
	let router = |sim: &RelaySim| sim.router(R).expect("the measured relay").work_done();
	let sent = |sim: &RelaySim| sim.sent.get(&R).copied().unwrap_or(0);
	let (work, was) = (router(&sim), sent(&sim));
	sim.link(R, FRESH, 1, Tie::Peer);
	quiesce(&mut sim);
	let new = format!("new{}", claims - 1);
	let shown = sim.router(R).expect("the measured relay").shown();
	assert!(shown.contains_key(&(PUBLISHER as Id, p(&new))), "the newer copy won");
	Cost {
		work: router(&sim) - work,
		sent: sent(&sim) - was,
	}
}

/// A downstream session that closes costs the relay the routes it was sent, one per
/// forwarding node, and nothing per publisher, broadcast, or other session.
#[test]
fn a_leave_costs_the_routes_it_was_sent() {
	assert_flat("publishers", sweep(publishers, FLAT, Bed::leave));
	assert_flat("broadcasts", sweep(broadcasts, FLAT, Bed::leave));
	assert_flat("upstream", sweep(upstream, FLAT, Bed::leave));
	assert_flat("downstream", sweep(downstream, FLAT, Bed::leave));
	assert_flat("watchers", sweep(watched, FLAT, Bed::leave));
	assert_linear("forwarding nodes", sweep(origins, LINEAR, Bed::leave));
}

/// Each event's cost along each axis, at sizes past the gate's.
#[test]
#[ignore = "a benchmark; Nightly runs it from test/route-bench.sh"]
fn benchmark_relay_event_work() {
	type Axis = (&'static str, fn(&mut Shape, usize));
	let axes: [Axis; 7] = [
		("forwarding_nodes", origins),
		("publishers", publishers),
		("broadcasts", broadcasts),
		("upstream", upstream),
		("downstream", downstream),
		("watched", watched),
		("unwatched", unwatched),
	];
	type Event = (&'static str, Shape, fn(&mut Bed) -> Cost);
	let events: [Event; 5] = [
		("announce", BASE, Bed::announce),
		("cut", BASE, Bed::cut),
		("detour", BASE, Bed::detour),
		("detour_quiet", QUIET, Bed::detour),
		("leave", BASE, Bed::leave),
	];
	println!("event,axis,size,work,sent");
	for (event, base, run) in events {
		for (axis, set) in axes {
			let sizes = [2, 16, 128];
			// A mesh this large checks itself in the gate's sweeps instead.
			for (size, cost) in sizes.into_iter().zip(sweep_at(base, set, sizes, run, false)) {
				println!("{event},{axis},{size},{},{}", cost.work, cost.sent);
			}
		}
	}
}

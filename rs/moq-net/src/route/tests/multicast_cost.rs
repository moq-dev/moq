//! What one event costs a relay under [`relay::Router`], swept along each axis it could
//! fan out over: the forwarding nodes it routes to, the publishers behind each, the
//! broadcasts each publisher claims, and the sessions it holds: upstream, each neighbour
//! offering every route; downstream, each sent every route; and endpoints watching, each
//! asking for the whole tree or for a prefix no claim falls under. A sweep holds the other
//! axes fixed and counts the measured relay's work and messages, so an event that visits
//! what it did not change fails here rather than only timing slower: `cost.rs` for the
//! rule the switch puts lite-07 sessions on.

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
			bed.sim.link(ME, bed.up(up), 1, Tie::Peer);
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

	/// The first upstream relay's session to the first forwarding node closes, then
	/// reopens unmeasured.
	fn cut(&mut self) -> Cost {
		let (up, origin) = (self.up(0), self.origin(0));
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
	sweep_at(axis, sizes, event, true)
}

fn sweep_at<const N: usize>(
	axis: fn(&mut Shape, usize),
	sizes: [usize; N],
	event: fn(&mut Bed) -> Cost,
	checked: bool,
) -> [Cost; N] {
	sizes.map(|size| {
		let mut shape = BASE;
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

/// A neighbour's route that breaks costs the relay the neighbours offering the same
/// route, which it ranks, and nothing per other forwarding node, publisher, broadcast, or
/// session it sends to.
#[test]
fn a_cut_costs_the_neighbours_offering_the_route() {
	assert_flat("forwarding nodes", sweep(origins, FLAT, Bed::cut));
	assert_flat("publishers", sweep(publishers, FLAT, Bed::cut));
	assert_flat("broadcasts", sweep(broadcasts, FLAT, Bed::cut));
	assert_flat("downstream", sweep(downstream, FLAT, Bed::cut));
	assert_flat("watchers", sweep(watched, FLAT, Bed::cut));
	assert_linear("upstream", sweep(upstream, LINEAR, Bed::cut));
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
#[ignore = "a sweep; Nightly runs it from test/route-sweep.sh"]
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
	type Event = (&'static str, fn(&mut Bed) -> Cost);
	let events: [Event; 3] = [("announce", Bed::announce), ("cut", Bed::cut), ("leave", Bed::leave)];
	println!("event,axis,size,work,sent");
	for (event, run) in events {
		for (axis, set) in axes {
			let sizes = [2, 16, 128];
			// A mesh this large checks itself in the gate's sweeps instead.
			for (size, cost) in sizes.into_iter().zip(sweep_at(set, sizes, run, false)) {
				println!("{event},{axis},{size},{},{}", cost.work, cost.sent);
			}
		}
	}
}

//! What one route change costs a relay, swept along each axis it could fan out over:
//! the origins it hears, the claims behind each origin (one included), the neighbours
//! offering the same route, and the streams it answers, each watching the change or a
//! prefix the change misses. A sweep holds the other axes fixed and counts
//! [`path::Router::work`], what the change sends, and what the reader sees, so a change
//! that expands into per-claim rewrites or visits streams it cannot reach fails here
//! rather than only timing slower. The `residual_` tests pin axes that still grow and
//! must not: a fix flips them.

use super::*;

/// This relay, in paths.
const RELAY: Id = 99;
/// The far node a heard route crosses before its sender, and the one a repath moves to.
const FAR: [Id; 2] = [500, 501];
/// The first key of the streams this relay answers.
const DOWN: Node = 10_000;

#[derive(Clone, Copy, Debug)]
struct Shape {
	/// Origins heard, each under a prefix of its own.
	origins: usize,
	/// Claims behind each origin.
	claims: usize,
	/// Upstream neighbours, each offering every origin's route.
	neighbours: usize,
	/// Streams this relay answers.
	streams: usize,
	/// Whether each stream watches the root, covering every origin, or a prefix no
	/// origin claims under.
	watched: bool,
}

const BASE: Shape = Shape {
	origins: 4,
	claims: 4,
	neighbours: 2,
	streams: 4,
	watched: true,
};

/// What one change cost this relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cost {
	/// Routes, prefixes, streams, and reader claims visited.
	work: usize,
	/// Identities recomputed.
	touched: usize,
	/// ROUTE messages sent.
	routes: usize,
	/// Claims sent in batches.
	claims: usize,
	/// Any other message sent.
	other: usize,
	/// Shown claims whose state changed.
	shown: usize,
}

struct Relay {
	router: path::Router<Node, u8>,
	far: usize,
	rtt: u64,
	closed: usize,
}

/// The route `neighbour` offers for origin `id` through `far`, carrying an RTT so a
/// change to the link's RTT changes the offer this relay sends on.
fn offer(id: Id, far: Id, neighbour: usize) -> path::Reach {
	let mut offer = along(
		&[(id, 0), (far, 1), (1000 + neighbour as Id, 1)],
		1 + neighbour as Metric,
	);
	offer.rtt = Some(Duration::ZERO);
	path::Reach::Finite(offer)
}

fn origin(origin: usize) -> Path {
	p(&format!("o{origin}"))
}

impl Relay {
	fn new(shape: Shape) -> Self {
		let mut router = path::Router::new(RELAY, 1, Doubt::DEFAULT);
		for up in 0..shape.neighbours {
			router.connect(up, 1000 + up as Id, Link::default()).unwrap();
			router.edited(up);
		}
		for stream in 0..shape.streams {
			let down = DOWN + stream;
			router.connect(down, 2000 + stream as Id, Link::default()).unwrap();
			router.edited(down);
			let prefix = match shape.watched {
				true => String::new(),
				false => format!("x{stream}"),
			};
			reask(&mut router, down, &[&prefix]).unwrap();
		}
		for o in 0..shape.origins {
			let id = 1 + o as Id;
			for up in 0..shape.neighbours {
				router.route(up, id, origin(o), Some(offer(id, FAR[0], up)));
				let changes = (0..shape.claims)
					.map(|claim| (p(&format!("o{o}/b{claim}")), Some(1)))
					.collect();
				router.batch(up, id, origin(o), changes, Some(1));
			}
		}
		let mut relay = Self {
			router,
			far: 0,
			rtt: 0,
			closed: 0,
		};
		relay.measure(|_| {});
		relay
	}

	/// The first neighbour moves the first origin's route onto a path through the other
	/// far node: one route changes, and none of its claims.
	fn repath(&mut self) -> Cost {
		self.far = 1 - self.far;
		let reach = offer(1, FAR[self.far], 0);
		self.measure(|router| router.route(0, 1, origin(0), Some(reach)))
	}

	/// The RTT of the link to the first neighbour changes: every route it offers does.
	fn relink(&mut self) -> Cost {
		self.rtt = self.rtt % 2 + 1;
		let rtt = Duration::from_millis(5 * self.rtt);
		self.measure(|router| router.rtt(0, Some(rtt)))
	}

	/// The session of the next stream this relay answers closes.
	fn close(&mut self) -> Cost {
		let down = DOWN + self.closed;
		self.closed += 1;
		self.measure(|router| router.disconnect([down], Instant::ZERO))
	}

	fn measure(&mut self, change: impl FnOnce(&mut path::Router<Node, u8>)) -> Cost {
		let (work, touched) = (self.router.work(), self.router.touched());
		change(&mut self.router);
		let mut cost = Cost {
			work: self.router.work() - work,
			touched: self.router.touched() - touched,
			..Cost::default()
		};
		for out in self.router.drain() {
			match out {
				path::Out::Route { .. } => cost.routes += 1,
				path::Out::Batch { changes, .. } if !changes.is_empty() => cost.claims += changes.len(),
				_ => cost.other += 1,
			}
		}
		self.router.events();
		cost.shown = self.router.changed().values().map(BTreeSet::len).sum();
		cost
	}
}

/// The cost of `change` on relays shaped as `BASE` with one axis set to each size,
/// measured after one warm-up change so first-time effects are left out.
fn sweep(axis: fn(&mut Shape, usize), sizes: [usize; 3], change: fn(&mut Relay) -> Cost) -> [Cost; 3] {
	sizes.map(|size| {
		let mut shape = BASE;
		axis(&mut shape, size);
		let mut relay = Relay::new(shape);
		change(&mut relay);
		change(&mut relay)
	})
}

fn origins(shape: &mut Shape, n: usize) {
	shape.origins = n;
}

fn claims(shape: &mut Shape, n: usize) {
	shape.claims = n;
}

fn neighbours(shape: &mut Shape, n: usize) {
	shape.neighbours = n;
}

fn watched(shape: &mut Shape, n: usize) {
	shape.streams = n;
}

fn unwatched(shape: &mut Shape, n: usize) {
	shape.streams = n;
	shape.watched = false;
}

/// One broadcast per origin, a few, and many.
const FLAT: [usize; 3] = [1, 8, 64];
/// Doublings, for axes that may grow linearly.
const LINEAR: [usize; 3] = [8, 16, 32];

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

/// Moving one route onto another path sends each stream watching it one ROUTE, and no
/// claim: the work is flat in the origins and the claims behind the route, and grows
/// linearly in the streams it reaches and the neighbours offering the same route, which
/// it ranks.
#[test]
fn a_repath_costs_the_streams_it_reaches_and_nothing_per_claim() {
	assert_flat("origins", sweep(origins, FLAT, Relay::repath));
	assert_flat("claims", sweep(claims, FLAT, Relay::repath));
	let reached = sweep(watched, LINEAR, Relay::repath);
	assert_linear("streams the change reaches", reached);
	assert_eq!(reached.map(|cost| cost.routes), LINEAR, "a ROUTE per stream");
	assert_linear("neighbours", sweep(neighbours, LINEAR, Relay::repath));
	for cost in reached {
		assert_eq!(
			(cost.touched, cost.claims, cost.other, cost.shown),
			(1, 0, 0, 0),
			"{cost:?}"
		);
	}
}

/// An RTT change on one link re-offers each route through it to each stream watching
/// that route, and sends no claim: the work is flat in the claims behind each route, and
/// grows linearly in the routes through the link and the streams they reach.
#[test]
fn a_link_update_costs_the_routes_through_it_and_nothing_per_claim() {
	assert_flat("claims", sweep(claims, FLAT, Relay::relink));
	let routes = sweep(origins, LINEAR, Relay::relink);
	assert_linear("routes through the link", routes);
	assert_eq!(routes.map(|cost| cost.touched), LINEAR);
	assert_eq!(routes.map(|cost| cost.routes), LINEAR.map(|n| n * BASE.streams));
	let reached = sweep(watched, LINEAR, Relay::relink);
	assert_linear("streams the change reaches", reached);
	assert_eq!(reached.map(|cost| cost.routes), LINEAR.map(|n| n * BASE.origins));
	for cost in routes.into_iter().chain(reached) {
		assert_eq!((cost.claims, cost.other, cost.shown), (0, 0, 0), "{cost:?}");
	}
}

/// A route change visits every stream this relay answers for each identity it
/// recomputes, even a stream whose prefix no route meets: `advertise` walks every
/// answering stream, and so does `run` for the sources a selection change moves. An
/// update with no single route in scope (an RTT change, a DEAD, a drain, a closed
/// session) also plans every prefix of each, since `stream` returns all of them when no
/// region narrows it. Both should be flat in the streams the change misses, which needs
/// an index of the streams by the prefixes they ask for and the routes they were sent.
#[test]
fn residual_a_route_change_visits_every_unrelated_stream() {
	for change in [Relay::repath as fn(&mut Relay) -> Cost, Relay::relink] {
		let costs = sweep(unwatched, FLAT, change);
		assert!(costs.iter().all(|cost| cost.routes == 0), "{costs:?}");
		assert!(
			costs.windows(2).all(|pair| pair[1].work > pair[0].work),
			"fixed, so flip this residual: {costs:?}"
		);
	}
}

/// Closing one stream's session, which sent this relay nothing, re-plans every route
/// it was sent for every other stream, which then sends nothing: viewer churn costs
/// each close the identities it watched times all the streams left. This should be
/// flat in the other streams.
#[test]
fn residual_closing_a_stream_revisits_every_other_stream() {
	let costs = sweep(watched, LINEAR, Relay::close);
	assert!(
		costs.iter().all(|cost| cost.routes == 0 && cost.other == 0),
		"{costs:?}"
	);
	assert!(
		costs.windows(2).all(|pair| pair[1].work > pair[0].work),
		"fixed, so flip this residual: {costs:?}"
	);
}

/// The routes `router` holds for `ids` with no live path.
fn held(router: &path::Router<Node, u8>, ids: impl IntoIterator<Item = Id>) -> usize {
	(ids.into_iter())
		.map(|id| router.routes(id).filter(|(_, route)| route.offer().is_none()).count())
		.sum()
}

/// Churn while no timer fires: a far session that flaps, and publishers that restart
/// under fresh identities, sent on to a stream that never answers. A doubt is kept per
/// far node however often its sessions flap, and a repeated DEAD adds nothing, but each
/// closed session's hop, each restarted identity's doubt, and each lost identity's hold
/// is kept until its own timer: what a node remembers grows with the churn inside one
/// timer, not with the live topology. Once the timers pass, only the live topology is
/// left. A peer can only make a node remember hops on paths it offered (`credits`).
#[test]
fn residual_same_clock_churn_is_remembered_until_its_timer() {
	const FLAPS: u64 = 64;
	let now = Instant::ZERO;
	let mut router = path::Router::<Node, u8>::new(RELAY, 1, Doubt::DEFAULT);
	router.connect(0, 1000, Link::default()).unwrap();
	router.edited(0);
	router.connect(DOWN, 2000, Link::default()).unwrap();
	router.edited(DOWN);
	reask(&mut router, DOWN, &[""]).unwrap();
	router.route(0, 7, p(""), Some(path::Reach::Finite(along(&[(7, 0), (1000, 0)], 1))));
	router.batch(0, 7, p(""), vec![(p("a"), Some(1))], Some(1));
	// The neighbour's session to publisher 7 flaps, and its route moves to the next.
	let flap = |router: &mut path::Router<Node, u8>, session: u64| {
		let moved = vec![path::Moved {
			id: 7,
			prefix: p(""),
			offer: along(&[(7, 0), (1000, session + 1)], 1),
		}];
		let hop = path::Hop { node: 1000, session };
		let dead = path::Dead::Closed {
			hop,
			far: 7,
			moved,
			gone: false,
		};
		router.dead(0, dead, now);
		router.timeout(now);
		router.drain();
	};
	for session in 0..FLAPS {
		flap(&mut router, session);
	}
	let kept = router.retained();
	assert_eq!((kept.doubt, kept.draining), (1, 0), "one doubt per far node");
	assert_eq!(kept.dead, FLAPS as usize, "a dead hop per closed session");
	flap(&mut router, FLAPS - 1);
	assert_eq!(router.retained(), kept, "a repeated DEAD adds nothing");

	// Publishers behind the neighbour restart under fresh identities, each losing its
	// session to the neighbour. Each was sent on to a stream that never answers, so each
	// is held until the hold timer.
	let restarts = 3000..3000 + FLAPS;
	for id in restarts.clone() {
		let hop = path::Hop {
			node: 1000,
			session: id,
		};
		router.route(
			0,
			id,
			p(""),
			Some(path::Reach::Finite(along(&[(id, 0), (1000, id)], 1))),
		);
		router.batch(0, id, p(""), vec![(p("a"), Some(1))], Some(1));
		router.drain();
		let dead = path::Dead::Closed {
			hop,
			far: id,
			moved: Vec::new(),
			gone: false,
		};
		router.dead(0, dead, now);
		router.timeout(now);
		router.drain();
	}
	let kept = router.retained();
	assert_eq!(
		held(&router, restarts.clone()),
		FLAPS as usize,
		"a hold per lost identity"
	);
	assert_eq!(kept.doubt, 1 + FLAPS as usize, "a doubt per restarted identity");
	assert_eq!(kept.dead, 2 * FLAPS as usize);

	// Once the doubt timer fires, only the hops a held route still names are left.
	let later = now.saturating_add(Doubt::DEFAULT.timer);
	router.timeout(later);
	let kept = router.retained();
	assert_eq!((kept.dead, kept.doubt), (FLAPS as usize, 0), "{kept:?}");
	assert!(live(&router, 7), "the moved route is used once the doubt lifts");
	// Once the hold timer fires, this relay lets go; once the neighbour lets go too, only
	// the live topology is left.
	router.timeout(later.saturating_add(path::HOLD));
	assert_eq!(held(&router, restarts.clone()), 0);
	for id in restarts {
		router.route(0, id, p(""), None);
	}
	router.drain();
	let kept = router.retained();
	assert_eq!((kept.routes, kept.dead, kept.doubt), (1, 0, 0), "{kept:?}");
}

/// Whether `router` uses a live route for publisher `id`.
fn live(router: &path::Router<Node, u8>, id: Id) -> bool {
	router.routes(id).any(|(_, route)| route.offer().is_some())
}

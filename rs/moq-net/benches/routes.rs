//! What one change costs a lite-07 route node, at the origin and at a relay hearing it.
//!
//! - watched, unwatched: one change to the origin's own claims, swept over the claims
//!   the origin holds and the streams it answers. Sending a change costs each stream
//!   it reaches its own batch, so the streams axis is expected to grow; the claims
//!   axis is not. In watched every stream watches the root, so each is sent the
//!   change; in unwatched each watches a prefix of its own, which the change misses.
//! - received: a relay hears one batch from the origin and forwards it on, swept over
//!   the claims the origin holds and the claims the batch changes. The changes axis is
//!   expected to grow; the claims axis is not.
//! - lagging: a relay hears the origin on two streams, the second some one-claim
//!   batches behind the first, and each hears its next batch. Swept over the claims
//!   the origin holds and how far the second lags: neither axis is expected to grow.
//! - confirmed: a relay holds back every claim from the streams it answers until it
//!   confirms serving one, all of them at one identity and epoch. One claim restarts at
//!   another epoch, then back at the shared one, each confirmed. Swept over the claims
//!   held and the streams answered: the streams axis is expected to grow, since each is
//!   sent the claim; the claims axis is not.
//! - teardown: a node following one origin on a stream per prefix, as a viewer session
//!   holding many announce prefixes does, closes them all at once. Swept over the
//!   prefixes followed and the claims each holds: both axes are expected to grow, no
//!   faster than the claims held.
//! - refollow: the same node stops asking for one prefix and asks for it again. The claims
//!   axis is expected to grow, since the stream hears its prefix's claims again; the
//!   prefixes axis no faster than the prefixes held.
//!
//! - drain, repath, follow, reopen, swap, remark: a relay hearing every forwarding node
//!   behind it from its upstream neighbours, telling one downstream neighbour every route
//!   and one watcher every claim, and serving what it hears into a real origin as a
//!   session does. Swept over the forwarding nodes whose routes the event changes and the
//!   broadcasts behind each.
//! - drain: the cheapest upstream sends GOAWAY, so every route moves to the second.
//! - repath: the only upstream moves every route onto another path at the same cost.
//!   Nothing anyone is told or served changes, so only the routes axis may grow.
//! - follow: the first upstream connects to a relay holding nothing and offers every
//!   route with its claims.
//! - reopen/spare: the cheapest upstream closes its session and opens another, twice in
//!   a row, as a relay restarting every link does, while a second upstream offers every
//!   route dearer. Each route's cost changes four times, and each change rewrites every
//!   claim behind it into the origin and tells the watcher each again.
//! - reopen/sole: the same with no second upstream, so each route ends and returns:
//!   the claims stay shown, unavailable, and come back sent whole.
//! - swap: the watcher swaps the whole tree for the first forwarding node's prefix and
//!   back. Each edit tells again the claims both prefixes cover.
//! - remark: the relay's own claim moves between its two identities, then back, as an
//!   endpoint's does when it reconnects over a marked link. Swept over the claims the relay
//!   holds and the downstream neighbours, which each hear a revision under both.
//!
//! The route layer is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench routes`.

use std::fmt;

use moq_net::fuzz::{self, RouteMesh, RouteShape, RouteTally};

/// Two swept counts, named `<a><unit>_<b><unit>` such as `10c_1000s`.
#[derive(Clone, Copy)]
struct Shape {
	a: usize,
	b: usize,
	units: [&'static str; 2],
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}{}_{}{}", self.a, self.units[0], self.b, self.units[1])
	}
}

fn shapes(pairs: &[(usize, usize)], units: [&'static str; 2]) -> Vec<Shape> {
	pairs.iter().map(|&(a, b)| Shape { a, b, units }).collect()
}

/// `(claims, streams)` shapes.
fn claims_streams() -> Vec<Shape> {
	shapes(&[(10, 10), (10_000, 10), (10, 1_000), (10_000, 1_000)], ["c", "s"])
}

/// `(claims, changes)` shapes for a relay hearing a batch.
fn received_shapes() -> Vec<Shape> {
	shapes(&[(100, 1), (10_000, 1), (100, 100), (10_000, 100)], ["c", "d"])
}

/// `(claims, streams)` shapes for a relay holding back claims.
fn held_shapes() -> Vec<Shape> {
	shapes(&[(10, 10), (10_000, 10), (10, 100), (10_000, 100)], ["c", "s"])
}

/// `(prefixes, claims)` shapes for a node following one origin per prefix.
fn followed_shapes() -> Vec<Shape> {
	shapes(&[(16, 1), (1_024, 1), (16, 16), (1_024, 16)], ["p", "c"])
}

/// `(claims, lag)` shapes for a relay hearing a stream that lags another.
fn lagging_shapes() -> Vec<Shape> {
	shapes(&[(100, 1), (10_000, 1), (100, 100), (10_000, 10_000)], ["c", "l"])
}

/// `(relays, broadcasts)` shapes for a relay between routing neighbours: the forwarding
/// nodes whose routes one event changes, and the broadcasts behind each.
fn routed_shapes() -> Vec<Shape> {
	shapes(
		&[(1, 16), (1, 4_096), (16, 16), (16, 4_096), (64, 16), (64, 1_024)],
		["r", "b"],
	)
}

/// `(broadcasts, downstream)` shapes for the relay's own claim moving identity.
fn remarked_shapes() -> Vec<Shape> {
	shapes(&[(16, 1), (4_096, 1), (16, 16), (4_096, 16)], ["b", "d"])
}

fn node(shape: Shape, interest: fn(usize) -> String) -> fuzz::RouteNode {
	let interests: Vec<String> = (0..shape.b).map(interest).collect();
	let interests: Vec<&str> = interests.iter().map(String::as_str).collect();
	fuzz::RouteNode::new(shape.a, &interests)
}

/// One change to the origin's own claims, which `sent` batches reach per stream.
fn own(node: &fuzz::RouteNode, streams: usize, sent: usize) {
	node.own("live/extra", true);
	assert_eq!(node.drain(), sent * streams);
	node.own("live/extra", false);
	assert_eq!(node.drain(), sent * streams);
}

/// A relay shaped `(relays, broadcasts)` with `upstream` upstream neighbours.
fn mesh(relays: usize, broadcasts: usize, upstream: usize) -> RouteShape {
	RouteShape {
		relays,
		broadcasts,
		upstream,
		downstream: 1,
		watchers: 1,
	}
}

/// A mesh settled by one run of `event` and `restore`.
fn settled(
	shape: Shape,
	upstream: usize,
	event: fn(&mut RouteMesh) -> RouteTally,
	restore: fn(&mut RouteMesh),
) -> RouteMesh {
	let mut mesh = RouteMesh::new(mesh(shape.a, shape.b, upstream));
	event(&mut mesh);
	restore(&mut mesh);
	mesh
}

/// Close and open the first upstream twice.
fn twice(mesh: &mut RouteMesh) -> RouteTally {
	let mut tally = mesh.reopen(0);
	tally += mesh.reopen(0);
	tally
}

/// Swap the watcher's tree to the first forwarding node's prefix and back.
fn swap(mesh: &mut RouteMesh) -> RouteTally {
	let mut tally = mesh.swap(true);
	tally += mesh.swap(false);
	tally
}

fn drain(mesh: &mut RouteMesh) -> RouteTally {
	mesh.goaway(0)
}

/// Bring the drained upstream back.
fn undrain(mesh: &mut RouteMesh) {
	_ = mesh.reopen(0);
}

fn repath(mesh: &mut RouteMesh) -> RouteTally {
	mesh.repath(0)
}

fn unchanged(_: &mut RouteMesh) {}

mod routes {
	use moq_net::fuzz::{self, RouteMesh, RouteShape, RouteTally};
	use swole::bench;

	use super::{
		Shape, claims_streams, followed_shapes, held_shapes, lagging_shapes, mesh, node, own, received_shapes,
		remarked_shapes, routed_shapes, settled,
	};

	/// A node whose streams carry how many batches each change sends them.
	struct Node {
		node: fuzz::RouteNode,
		streams: usize,
	}

	fn watching(shape: Shape) -> Node {
		Node {
			node: node(shape, |_| String::new()),
			streams: shape.b,
		}
	}

	fn elsewhere(shape: Shape) -> Node {
		Node {
			node: node(shape, |i| format!("other/{i}")),
			streams: shape.b,
		}
	}

	/// Each stream reads the start, then the withdrawal, as a batch.
	#[bench(args = claims_streams(), setup = watching)]
	fn watched(input: &Node) {
		own(&input.node, input.streams, 1)
	}

	#[bench(args = claims_streams(), setup = elsewhere)]
	fn unwatched(input: &Node) {
		own(&input.node, input.streams, 0)
	}

	fn heard(shape: Shape) -> (fuzz::RouteRelay, usize) {
		(fuzz::RouteRelay::heard(shape.a), shape.b)
	}

	/// Each claim changed is served into the origin and forwarded on its own.
	#[bench(args = received_shapes(), setup = heard)]
	fn received((relay, changes): &mut (fuzz::RouteRelay, usize)) {
		assert_eq!(relay.change(*changes), 2 * *changes);
	}

	fn lagging_relay(shape: Shape) -> fuzz::RouteRelay {
		fuzz::RouteRelay::lagging(shape.a, shape.b)
	}

	/// The first stream's change is served through both streams and forwarded; the second's
	/// older one queues nothing, though its copy now holds the claim, so the relay works out
	/// that claim's ways again.
	#[bench(args = lagging_shapes(), setup = lagging_relay)]
	fn lagging(relay: &mut fuzz::RouteRelay) {
		assert_eq!(relay.step(), 3);
	}

	fn held(shape: Shape) -> (fuzz::RouteRelay, usize) {
		(fuzz::RouteRelay::held(shape.a, shape.b), shape.b)
	}

	/// Each change's serving change, then one batch per stream answered.
	#[bench(args = held_shapes(), setup = held)]
	fn confirmed((relay, streams): &mut (fuzz::RouteRelay, usize)) {
		assert_eq!(relay.restart(), 2 * (1 + *streams));
	}

	fn follower(shape: Shape) -> fuzz::RouteFollower {
		fuzz::RouteFollower::new(shape.a, shape.b)
	}

	#[bench(args = followed_shapes(), setup = follower)]
	fn teardown(follower: fuzz::RouteFollower) {
		follower.teardown()
	}

	fn following(shape: Shape) -> (fuzz::RouteFollower, usize) {
		(fuzz::RouteFollower::new(shape.a, shape.b), shape.b)
	}

	/// Both edits go to the peer, and the prefix's claims leave the origin and return.
	#[bench(args = followed_shapes(), setup = following)]
	fn refollow((follower, claims): &mut (fuzz::RouteFollower, usize)) {
		assert_eq!(follower.refollow(), 2 + 2 * *claims);
	}

	fn draining(shape: Shape) -> RouteMesh {
		settled(shape, 2, super::drain, super::undrain)
	}

	#[bench(args = routed_shapes(), setup = draining, after = super::undrain)]
	fn drain(mesh: &mut RouteMesh) -> RouteTally {
		super::drain(mesh)
	}

	fn repathing(shape: Shape) -> RouteMesh {
		settled(shape, 1, super::repath, super::unchanged)
	}

	#[bench(args = routed_shapes(), setup = repathing)]
	fn repath(mesh: &mut RouteMesh) -> RouteTally {
		super::repath(mesh)
	}

	fn unfollowed(shape: Shape) -> RouteMesh {
		RouteMesh::new(mesh(shape.a, shape.b, 0))
	}

	/// Returns the mesh, so tearing it down stays unmeasured.
	#[bench(args = routed_shapes(), setup = unfollowed)]
	fn follow(mut mesh: RouteMesh) -> (RouteMesh, RouteTally) {
		let tally = mesh.open(0);
		(mesh, tally)
	}

	fn spare(shape: Shape) -> RouteMesh {
		settled(shape, 2, super::twice, super::unchanged)
	}

	#[bench(args = routed_shapes(), setup = spare)]
	fn reopen_spare(mesh: &mut RouteMesh) -> RouteTally {
		super::twice(mesh)
	}

	fn sole(shape: Shape) -> RouteMesh {
		settled(shape, 1, super::twice, super::unchanged)
	}

	#[bench(args = routed_shapes(), setup = sole)]
	fn reopen_sole(mesh: &mut RouteMesh) -> RouteTally {
		super::twice(mesh)
	}

	fn swapping(shape: Shape) -> RouteMesh {
		settled(shape, 1, super::swap, super::unchanged)
	}

	#[bench(args = routed_shapes(), setup = swapping)]
	fn swap(mesh: &mut RouteMesh) -> RouteTally {
		super::swap(mesh)
	}

	/// The relay owning `broadcasts` claims, its identity moved there and back twice to settle.
	fn remarking(shape: Shape) -> RouteMesh {
		let mut mesh = RouteMesh::new(RouteShape {
			downstream: shape.b,
			..mesh(0, 0, 0)
		});
		mesh.own(shape.a);
		for _ in 0..4 {
			mesh.remark();
		}
		mesh
	}

	#[bench(args = remarked_shapes(), setup = remarking)]
	fn remark(mesh: &mut RouteMesh) -> RouteTally {
		mesh.remark();
		mesh.remark()
	}
}

fn main() {
	swole::main();
}

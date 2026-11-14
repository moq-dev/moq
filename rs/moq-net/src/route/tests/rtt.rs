//! The RTT a route carries: each node adds its link's min RTT to the sum it received,
//! and nothing ranks or selects a route by it.

use std::time::Duration;

use super::*;

fn originate(sim: &mut Sim, node: Node, id: Id) {
	sim.originate(node, id, 0);
	sim.announce(node, "", 0, None);
}

fn ms(ms: u64) -> Option<Duration> {
	Some(Duration::from_millis(ms))
}

/// The RTT `node` advertises to `peer` for `id`.
fn advertised(sim: &Sim, node: Node, peer: Node, id: Id) -> Option<Duration> {
	match sim.sent(node, peer, id) {
		Some(Reach::Finite(advert)) => advert.rtt,
		other => panic!("{node} sends {peer} {other:?}"),
	}
}

/// A chain origin, a, b, c: each relay measures its link toward the origin.
fn chain() -> Sim {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, a, 1);
	sim.join(a, b, 1);
	sim.join(b, c, 1);
	sim.rtt(a, origin, Some(10));
	sim.rtt(b, a, Some(20));
	sim.rtt(c, b, Some(30));
	originate(&mut sim, origin, 7);
	assert!(sim.run());
	sim
}

#[test]
fn each_relay_adds_its_link_to_the_sum() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let sim = chain();
	assert_eq!(advertised(&sim, origin, a, 7), ms(0), "the origin's own route");
	assert_eq!(advertised(&sim, a, b, 7), ms(10));
	assert_eq!(advertised(&sim, b, c, 7), ms(30));
	let routes = sim.nodes[c].routes(7);
	assert_eq!(routes, [(b, 3, ms(60))]);
}

/// A shift on one link sends one ROUTE_UPDATE on each stream carrying the route from
/// there on, and the same value again sends nothing.
#[test]
fn a_shift_updates_each_stream_downstream_once() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	let mut sim = chain();
	let downstream: usize = [a, b, c].iter().map(|&node| sim.sending(node, 7)).sum();
	let before = sim.stats.route_msgs;
	sim.rtt(a, origin, Some(40));
	assert!(sim.run());
	assert_eq!(sim.stats.route_msgs - before, downstream);
	assert_eq!(advertised(&sim, b, c, 7), ms(60));

	let before = sim.stats.route_msgs;
	sim.rtt(a, origin, Some(40));
	assert!(sim.run());
	assert_eq!(sim.stats.route_msgs, before);
}

/// A link with no measurement leaves the sum unknown from there on, while the nodes
/// before it still carry theirs.
#[test]
fn an_unknown_link_ends_the_sum() {
	let (a, b, c) = (1, 2, 3);
	let mut sim = chain();
	sim.rtt(b, a, None);
	assert!(sim.run());
	assert_eq!(advertised(&sim, a, b, 7), ms(10));
	assert_eq!(advertised(&sim, b, c, 7), None);
	assert_eq!(sim.nodes[c].routes(7)[0].2, None);
}

/// The metric selects; a far lower RTT through a costlier link changes nothing.
#[test]
fn rtt_never_selects_a_route() {
	let (origin, cheap, fast, edge) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.join(origin, cheap, 1);
	sim.join(origin, fast, 5);
	sim.join(cheap, edge, 1);
	sim.join(fast, edge, 1);
	sim.rtt(cheap, origin, Some(200));
	sim.rtt(fast, origin, Some(1));
	originate(&mut sim, origin, 7);
	assert!(sim.run());
	assert_eq!(sim.route(edge, 7).and_then(|route| route.next), Some(cheap));
	assert_eq!(advertised(&sim, cheap, edge, 7), ms(200));
}

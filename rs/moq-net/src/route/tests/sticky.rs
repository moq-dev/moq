//! The route rule as the prototype runs it, with the next hop in use ahead of the
//! path length and the neighbour in [`order`](super::multi::order).
//!
//! The route proof (`rs/moq-net/verify/route-algebra`) breaks ties between routes
//! of equal draining, priority, and cost by a fixed order, so it has one fixed
//! point. Staying on the next hop in use makes the choice depend on what came
//! first, which the proof does not cover. `rs/moq-net/verify/README.md` argues on
//! paper that the rule still settles; these tests check it, and show that which
//! of two equal routes a node sits on can change what a neighbour is offered.

use super::algebra::Rng;
use super::compare::Arm;
use super::multi::{Best, MultiSim, Tie, Tune};
use super::*;

/// The rule the epic picked: floor, epoch, batch, drain, and claimed.
const KEPT: Tune = match Arm::KEPT {
	Arm::Multi(tune) => tune,
	Arm::Path(_) => panic!("the picked rule is the prototype's"),
};

/// More deliveries than any network here needs to settle.
const LIMIT: usize = 200_000;

/// A random network: relays first, then publishers, each relay linked to an
/// earlier one and a few more links besides, costs of 1 or 2 so equal ranks are
/// common, and one link in four marked upstream.
struct Net {
	relays: usize,
	publishers: usize,
	links: Vec<(Node, Node, Metric, Tie)>,
}

impl Net {
	fn draw(rng: &mut Rng) -> Self {
		let relays = 4 + rng.below(5) as usize;
		let publishers = 1 + rng.below(2) as usize;
		let mut links = Vec::new();
		let joined = |a: Node, b: Node, links: &mut Vec<(Node, Node, Metric, Tie)>, rng: &mut Rng| {
			if a == b || links.iter().any(|&(x, y, _, _)| (x, y) == (a, b) || (x, y) == (b, a)) {
				return;
			}
			let tie = match rng.below(8) {
				0 | 1 => Tie::Up,
				2 => Tie::Scoped,
				_ => Tie::Peer,
			};
			links.push((a, b, 1 + rng.below(2), tie));
		};
		for relay in 1..relays {
			let earlier = rng.below(relay as u64) as Node;
			joined(relay, earlier, &mut links, rng);
		}
		for _ in 0..relays / 2 {
			let (a, b) = (rng.below(relays as u64) as Node, rng.below(relays as u64) as Node);
			joined(a, b, &mut links, rng);
		}
		for publisher in relays..relays + publishers {
			for _ in 0..1 + rng.below(2) {
				let relay = rng.below(relays as u64) as Node;
				if !links.iter().any(|&(x, y, _, _)| (x, y) == (publisher, relay)) {
					links.push((publisher, relay, 1, Tie::Peer));
				}
			}
		}
		Self {
			relays,
			publishers,
			links,
		}
	}

	fn build(&self) -> MultiSim {
		let mut net = MultiSim::new(KEPT, self.relays + self.publishers);
		for publisher in self.relays..self.relays + self.publishers {
			net.client(publisher);
			net.publish(publisher, 100 + publisher as Id);
		}
		for &(a, b, cost, tie) in &self.links {
			net.link(a, b, cost, tie);
		}
		for publisher in self.relays..self.relays + self.publishers {
			net.claim(publisher, "live/0", true);
		}
		net
	}
}

/// Deliver one message at a time from a random queue, reading a burst before
/// answering as `batch` does, until nothing is left. False if it never settles.
fn settle(net: &mut MultiSim, rng: &mut Rng) -> bool {
	for _ in 0..LIMIT {
		let ready = net.ready();
		if ready.is_empty() {
			net.flush();
			if net.ready().is_empty() {
				return true;
			}
			continue;
		}
		let ((from, to), _) = rng.pick(&ready);
		net.deliver(from, to);
		if rng.one_in(3) {
			net.flush();
		}
	}
	false
}

/// Links close and reopen, relays restart or send GOAWAY, all while messages are
/// in flight; then every closed link reopens.
fn churn(net: &mut MultiSim, plan: &Net, rng: &mut Rng) {
	let mut down = Vec::new();
	for _ in 0..rng.below(6) {
		for _ in 0..rng.below(40) {
			let ready = net.ready();
			if ready.is_empty() {
				break;
			}
			let ((from, to), _) = rng.pick(&ready);
			net.deliver(from, to);
		}
		net.flush();
		let relay = rng.below(plan.relays as u64) as Node;
		match rng.below(5) {
			0 => {
				let links: Vec<_> = (plan.links.iter())
					.filter(|&&(a, b, _, _)| (a == relay || b == relay) && net.is_up(a, b))
					.map(|&(a, b, _, _)| (a, b))
					.collect();
				net.reset(relay);
				down.extend(links);
			}
			1 => net.goaway(relay),
			_ => {
				let &(a, b, _, _) = &plan.links[rng.below(plan.links.len() as u64) as usize];
				if net.is_up(a, b) {
					net.close(a, b);
					down.push((a, b));
				}
			}
		}
	}
	for (a, b) in down {
		if !net.is_up(a, b) {
			net.open(a, b);
		}
	}
}

/// Every relay sits on a route of the best draining, priority, and cost among those
/// its neighbours last offered it, and the routes in use form trees: each one's
/// path is its next hop's route with that hop added.
fn check(net: &MultiSim, plan: &Net) -> Result<(), String> {
	for node in 0..plan.relays {
		let router = net.router(node).expect("a relay");
		for origin in router.origins() {
			let candidates = router.candidates(origin);
			let best = router.best(origin);
			let lowest = candidates.iter().map(Best::rank).min();
			let Some(best) = best else {
				if lowest.is_some() {
					return Err(format!("{node} uses no route to {origin} of {candidates:?}"));
				}
				continue;
			};
			if !candidates.contains(best) || Some(best.rank()) != lowest {
				return Err(format!("{node} uses {best:?} to {origin} of {candidates:?}"));
			}
			let path = &best.offer.path;
			if path.len() > 1 {
				let next = net.router(best.via).expect("a relay");
				let theirs = next.best(origin).map(|best| &best.offer.path[..]);
				if theirs != Some(&path[..path.len() - 1]) {
					return Err(format!("{node}'s route to {origin} is {path:?}; {} uses {theirs:?}", best.via));
				}
			}
		}
	}
	Ok(())
}

/// The rule as it runs settles under any delivery order, through churn, on a
/// route of the best rank it was offered at every node. Which of two equal routes
/// it settles on varies with the order: printed, for how often that reaches past
/// the node.
#[test]
fn the_next_hop_in_use_settles_on_a_best_rank() {
	let (mut settled, mut moved, mut ranked, mut reached) = (0, 0, 0, 0);
	for seed in 1..=150 {
		let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed);
		let plan = Net::draw(&mut rng);

		let mut net = plan.build();
		churn(&mut net, &plan, &mut rng);
		assert!(settle(&mut net, &mut rng), "seed {seed} never settles");
		if let Err(err) = check(&net, &plan) {
			panic!("seed {seed} after churn: {err}");
		}

		// The same network twice from cold, in two delivery orders.
		let mut runs = Vec::new();
		for _ in 0..2 {
			let mut net = plan.build();
			assert!(settle(&mut net, &mut rng), "seed {seed} never settles");
			if let Err(err) = check(&net, &plan) {
				panic!("seed {seed}: {err}");
			}
			runs.push(net);
		}
		for node in 0..plan.relays {
			let [a, b] = [&runs[0], &runs[1]].map(|net| net.router(node).expect("a relay"));
			for origin in a.origins().union(&b.origins()) {
				settled += 1;
				let [x, y] = [a.best(*origin), b.best(*origin)];
				moved += usize::from(x.map(|x| x.via) != y.map(|y| y.via));
				ranked += usize::from(x.map(Best::rank) != y.map(Best::rank));
				reached += usize::from(x.is_some() != y.is_some());
			}
		}
	}
	println!(
		"of {settled} routes settled twice: {moved} on another next hop, {ranked} at another rank, {reached} reachable once"
	);
}

/// A route learned over an upstream link is marked, and a marked route is never
/// sent over another upstream link. Relay 3 hears two routes of equal rank to
/// relay 0: through 2, over a link it marked upstream, and through 1, over a peer
/// link. The fixed order picks 1, the lower neighbour, and 3 offers it to 4, its
/// own upstream. Staying on the next hop in use keeps whichever came first: if 2's
/// came first, 4 is offered nothing and has no route to 0.
#[test]
fn an_equal_route_in_use_can_leave_a_neighbour_without_one() {
	let build = || {
		let mut net = MultiSim::new(KEPT, 6);
		net.client(5);
		net.publish(5, 100);
		net.link(5, 0, 1, Tie::Peer);
		net.link(0, 1, 1, Tie::Peer);
		net.link(0, 2, 1, Tie::Peer);
		net.link(3, 2, 1, Tie::Up);
		net.link(3, 1, 1, Tie::Peer);
		net.link(3, 4, 1, Tie::Up);
		net
	};
	let origin = hop(0, 0);
	let mut rng = Rng(7);
	let mut settled = |first: Node, second: Node| {
		let mut net = build();
		net.hold(second, 3);
		net.claim(5, "live/0", true);
		assert!(settle(&mut net, &mut rng));
		assert_eq!(net.router(3).and_then(|r| r.best(origin)).map(|best| best.via), Some(first));
		net.release(second, 3);
		assert!(settle(&mut net, &mut rng));
		let via = net.router(3).and_then(|r| r.best(origin)).map(|best| best.via);
		let reached = net.router(4).and_then(|r| r.best(origin)).is_some();
		(via, reached)
	};
	assert_eq!(settled(1, 2), (Some(1), true), "the fixed order's choice");
	assert_eq!(settled(2, 1), (Some(2), false), "the first heard, kept");
}

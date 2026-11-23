//! The route rule as [`relay::Router`] runs it, with the next hop in use ahead of the
//! path length and the neighbour in [`relay::order`].
//!
//! The route proof (`rs/moq-net/verify/route-algebra`) breaks ties between routes of
//! equal draining, priority, cost, and mark by a fixed order, so it has one fixed point.
//! Staying on the next hop in use makes the choice depend on what came first, which the
//! proof does not cover. `rs/moq-net/verify/route-algebra/README.md` argues on paper that
//! the rule still settles, at every node on the rank and mark the fixed order would give
//! it; these tests check both.

use crate::route::relay::{self, Best, NodeId};

use super::algebra::Rng;
use super::multi::Tie;
use super::multicast::{RelaySim, Router};
use super::*;

/// More deliveries than any network here needs to settle.
const LIMIT: usize = 200_000;

/// A random network: relays first, then publishers, each relay linked to an earlier one
/// and a few more links besides, costs of 1 or 2 so equal ranks are common, and one link
/// in four marked upstream.
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

	fn build(&self) -> RelaySim {
		let mut net = RelaySim::new(self.relays + self.publishers, relay::debounce(HOP));
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

fn is_up(net: &RelaySim, a: Node, b: Node) -> bool {
	net.up().contains(&(a.min(b), a.max(b)))
}

/// Deliver one message at a time from a random queue, reading a burst before answering,
/// until nothing is left. False if it never settles.
fn settle(net: &mut RelaySim, rng: &mut Rng) -> bool {
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

/// Links close and reopen, relays restart or send GOAWAY, all while messages are in
/// flight; then every closed link reopens.
fn churn(net: &mut RelaySim, plan: &Net, rng: &mut Rng) {
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
					.filter(|&&(a, b, _, _)| (a == relay || b == relay) && is_up(net, a, b))
					.map(|&(a, b, _, _)| (a, b))
					.collect();
				net.reset(relay);
				down.extend(links);
			}
			1 => net.goaway(relay),
			_ => {
				let &(a, b, _, _) = &plan.links[rng.below(plan.links.len() as u64) as usize];
				if is_up(net, a, b) {
					net.close(a, b);
					down.push((a, b));
				}
			}
		}
	}
	for (a, b) in down {
		if !is_up(net, a, b) {
			net.open(a, b);
		}
	}
}

/// Whether `origin` is one of `router`'s own identities.
fn is_self(router: &Router, origin: NodeId) -> bool {
	origin == router.me() || origin == router.me_marked()
}

/// Every relay sits on a route of the best draining, priority, and cost among those its
/// neighbours last offered it, and the routes in use form trees: each one's path is its
/// next hop's route with that hop added.
fn check(net: &RelaySim, plan: &Net) -> Result<(), String> {
	for node in 0..plan.relays {
		let router = net.router(node).expect("a relay");
		for origin in router.heard() {
			let candidates = router.usable(origin);
			let lowest = candidates.iter().map(|best| best.offer.rank()).min();
			let Some((via, offer)) = router.best(origin) else {
				if lowest.is_some() {
					return Err(format!("{node} uses no route to {origin:?} of {candidates:?}"));
				}
				continue;
			};
			let best = Best {
				via,
				offer: offer.clone(),
			};
			if !candidates.contains(&best) || Some(offer.rank()) != lowest {
				return Err(format!("{node} uses {best:?} to {origin:?} of {candidates:?}"));
			}
			let next = net.router(via).expect("a relay");
			if is_self(next, origin) {
				continue;
			}
			let path = &offer.path;
			let theirs = next.best(origin).map(|(_, offer)| &offer.path[..]);
			if theirs != Some(&path[..path.len() - 1]) {
				return Err(format!(
					"{node}'s route to {origin:?} is {path:?}; {via} uses {theirs:?}"
				));
			}
		}
	}
	Ok(())
}

/// The identities a relay announces a claim under, as of now.
fn announcing(router: &Router) -> Vec<NodeId> {
	[router.me(), router.me_marked()]
		.into_iter()
		.filter(|id| router.own_as(*id).0 > 0)
		.collect()
}

/// The rule as it runs settles under any delivery order, through churn, on a route of
/// the best rank it was offered at every node. Which of two equal routes it settles on
/// varies with the order, but never its rank, its mark, or whether it has one: only the
/// next hop, which no neighbour's offer depends on. What can vary is which identities
/// announce at all (see [`whether_a_relay_announces_hangs_on_arrival_order`]), so routes
/// are compared only to identities that announced in both orders.
#[test]
fn the_next_hop_in_use_settles_on_a_best_rank() {
	let (mut settled, mut moved, mut ranked, mut marked, mut reached, mut announced) = (0, 0, 0, 0, 0, 0);
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
		let mut both: Vec<NodeId> = Vec::new();
		for relay in 0..plan.relays {
			let [a, b] = [&runs[0], &runs[1]].map(|net| announcing(net.router(relay).expect("a relay")));
			announced += a.iter().filter(|id| !b.contains(id)).count() + b.iter().filter(|id| !a.contains(id)).count();
			both.extend(a.iter().filter(|id| b.contains(id)));
		}
		for node in 0..plan.relays {
			let [a, b] = [&runs[0], &runs[1]].map(|net| net.router(node).expect("a relay"));
			for origin in both.iter().filter(|&&origin| !is_self(a, origin)) {
				settled += 1;
				let [x, y] = [a.best(*origin), b.best(*origin)];
				moved += usize::from(x.map(|x| x.0) != y.map(|y| y.0));
				ranked += usize::from(x.map(|x| x.1.rank()) != y.map(|y| y.1.rank()));
				marked += usize::from(x.map(|x| x.1.marked) != y.map(|y| y.1.marked));
				reached += usize::from(x.is_some() != y.is_some());
			}
		}
	}
	println!(
		"of {settled} routes settled twice: {moved} on another next hop, {ranked} at another rank, {marked} marked once, {reached} reachable once; {announced} identities announced in one order only"
	);
	assert_eq!((ranked, marked, reached), (0, 0, 0));
	assert!(moved > 0, "no two orders settled on different next hops");
}

/// A relay offers a route to itself from its first claim on, for good. Relay 2 has no
/// endpoint: it announces only a claim it adopts across its scoped grant to relay 1, and 1
/// tells 2 its closest claim unless that is the one 2 told it. 2 tells 1 publisher 3's
/// claim, which 2 reaches over its cheap route to 0, and 1 adopts it, which beats 1's
/// own dear route to 0. If 2's grant reaches 1 first, 1's closest claim is the one 2
/// gave it, so 1 tells 2 nothing and 2 never announces. If 1 hears 0's claim first, it
/// tells it to 2, which announces it; 1 then adopts 2's cheaper grant and withdraws what
/// it told, and 2 keeps a route to itself with no claim behind it.
#[test]
fn whether_a_relay_announces_hangs_on_arrival_order() {
	let build = || {
		let mut net = RelaySim::new(4, relay::debounce(HOP));
		net.client(3);
		net.publish(3, 100);
		net.link(3, 0, 1, Tie::Peer);
		net.link(0, 2, 1, Tie::Peer);
		net.link(0, 1, 3, Tie::Peer);
		net.link(1, 2, 1, Tie::Scoped);
		net
	};
	let mut rng = Rng(11);
	let mut settled = |late: (Node, Node)| {
		let mut net = build();
		net.hold(late.0, late.1);
		net.claim(3, "live/0", true);
		assert!(settle(&mut net, &mut rng));
		net.release(late.0, late.1);
		assert!(settle(&mut net, &mut rng));
		while net.tick() {}
		assert!(settle(&mut net, &mut rng));
		let id = net.id(2);
		let relay = net.router(2).expect("a relay");
		let claimed = relay.own().1.contains_key(&(p("live/0"), 100));
		let routed = net.router(0).expect("a relay").best(id).is_some();
		(relay.announced(), claimed, routed)
	};
	assert_eq!(settled((0, 1)), (false, false, false), "2's grant first");
	assert_eq!(settled((2, 1)), (true, false, true), "0's claim first at 1");
}

/// A route learned over an upstream link is marked, and a marked route is never sent over
/// another upstream link. Relay 3 hears two routes of equal rank to relay 0: through 2,
/// over a link it marked upstream, and through 1, over a peer link. Were the mark below
/// the next hop in use, 3 would keep whichever came first, and with 2's first 4 would be
/// offered nothing. Unmarked first, 3 moves to 1's route whichever came first, and offers
/// it to 4, its own upstream.
#[test]
fn an_equal_route_in_use_never_leaves_a_neighbour_without_one() {
	let build = || {
		let mut net = RelaySim::new(6, relay::debounce(HOP));
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
	let mut rng = Rng(7);
	let mut settled = |first: Node, second: Node| {
		let mut net = build();
		let origin = net.id(0);
		net.hold(second, 3);
		net.claim(5, "live/0", true);
		assert!(settle(&mut net, &mut rng));
		assert_eq!(
			net.router(3).and_then(|r| r.best(origin)).map(|(via, _)| via),
			Some(first)
		);
		net.release(second, 3);
		assert!(settle(&mut net, &mut rng));
		let via = net.router(3).and_then(|r| r.best(origin)).map(|(via, _)| via);
		let reached = net.router(4).and_then(|r| r.best(origin)).is_some();
		(via, reached)
	};
	assert_eq!(settled(1, 2), (Some(1), true), "the unmarked route first");
	assert_eq!(settled(2, 1), (Some(1), true), "the marked route first");
}

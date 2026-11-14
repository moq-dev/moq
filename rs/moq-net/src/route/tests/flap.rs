//! A publisher's own sessions closing: doubt keeps relays off routes through the
//! far end, but not off a route that begins there.

use super::*;

/// The neighbour `node` routes 7 through, following next hops until the
/// publisher: `Ok(true)` at it, `Ok(false)` stopped short, `Err` round a loop.
fn walk(sim: &Sim, node: Node) -> Result<bool, Vec<Node>> {
	let mut seen = vec![node];
	let mut at = node;
	loop {
		match sim.toward(at, 7) {
			None => return Ok(false),
			Some(None) => return Ok(true),
			Some(Some(next)) if seen.contains(&next) => {
				seen.push(next);
				return Err(seen);
			}
			Some(Some(next)) => {
				seen.push(next);
				at = next;
			}
		}
	}
}

/// The publisher on relays 1 and 2, and relay 3 behind both, routing through 1.
fn dual_homed(rule: Rule) -> Sim {
	let (origin, r1, r2, r3) = (0, 1, 2, 3);
	let mut sim = Sim::with(rule, 4);
	sim.leaf(origin);
	sim.join(origin, r1, 1);
	sim.join(origin, r2, 2);
	sim.join(r1, r2, 1);
	sim.join(r1, r3, 1);
	sim.join(r2, r3, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	assert_eq!(sim.next_hop(r3, 7), Some(r1));
	sim
}

/// The publisher's link to relay 1 closes and reopens every half doubt timer.
/// Every relay keeps a route that reaches the publisher through relay 2 the
/// whole time, and shows the claim available.
#[test]
fn a_flapping_link_keeps_a_dual_homed_publisher_routed() {
	let (origin, r1, r2, r3) = (0, 1, 2, 3);
	let half = Doubt::DEFAULT.timer / 2;
	let mut sim = dual_homed(Rule::PATH);
	for flap in 0..8 {
		match flap % 2 {
			0 => sim.disconnect(origin, r1),
			_ => sim.connect(origin, r1),
		}
		// Each step, before any message moves and after each round of them.
		loop {
			for relay in [r1, r2, r3] {
				assert_eq!(walk(&sim, relay), Ok(true), "flap {flap}: relay {relay}");
				assert_eq!(sim.shows(relay, 7, "live/x"), Some(true), "flap {flap}: relay {relay}");
			}
			if sim.busy().is_empty() {
				break;
			}
			// One round; whether it went quiet is the loop's test.
			sim.settle(1);
		}
		sim.wait(half);
	}
	assert!(sim.run());
	assert_eq!(sim.next_hop(r3, 7), Some(r1), "back on the cheaper link");
}

/// Under the replaced rule, the relays that used the closed link have no route
/// while the link flaps, though the publisher never left.
#[test]
fn a_flapping_link_darkened_the_publisher_under_the_replaced_rule() {
	let (origin, r1, r3) = (0, 1, 3);
	let half = Doubt::DEFAULT.timer / 2;
	let mut sim = dual_homed(Rule::ORIGIN);
	for flap in 0..8 {
		match flap % 2 {
			0 => sim.disconnect(origin, r1),
			_ => sim.connect(origin, r1),
		}
		assert!(sim.flow());
		assert_eq!(walk(&sim, r3), Ok(false), "flap {flap}");
		sim.wait(half);
	}
}

/// The publisher on each of three relays in a ring that is cheap one way round,
/// whose sessions all close before any relay hears another's news.
fn ring_of_homes(rule: Rule) -> Sim {
	let (origin, r1, r2, r3) = (0, 1, 2, 3);
	let mut sim = Sim::with(rule, 4);
	sim.leaf(origin);
	for relay in [r1, r2, r3] {
		sim.join(origin, relay, 1);
	}
	for (a, b) in [(r1, r2), (r2, r3), (r3, r1)] {
		sim.link(a, b, SimLink::cost(1), SimLink::cost(3));
	}
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	for relay in [r1, r2, r3] {
		assert_eq!(sim.toward(relay, 7), Some(Some(origin)), "relay {relay} direct");
	}
	sim
}

/// The publisher leaves all three relays at once. Under the path rule each moves
/// onto its cheaper neighbour's direct route, which that neighbour's own close
/// made stale, so requests go round the ring until the news arrives: the cost of
/// keeping a publisher's other links usable. The replaced rule held every relay
/// instead. Either way every relay lets go once the news has spread.
#[test]
fn a_publisher_leaving_every_relay_at_once_loops_until_the_news_arrives() {
	let (origin, r1, r2, r3) = (0, 1, 2, 3);
	for (rule, loops) in [(Rule::ORIGIN, 0), (Rule::PATH, 3)] {
		let mut sim = ring_of_homes(rule);
		for relay in [r1, r2, r3] {
			sim.disconnect(origin, relay);
		}
		let looped = [r1, r2, r3]
			.into_iter()
			.filter(|&relay| walk(&sim, relay).is_err())
			.count();
		assert_eq!(looped, loops, "{}", rule.as_str());
		// One hop of delay: each relay hears its neighbours' news.
		sim.settle(1);
		for relay in [r1, r2, r3] {
			assert!(
				walk(&sim, relay).is_ok(),
				"{}: relay {relay} after one hop",
				rule.as_str()
			);
		}
		assert!(sim.run());
		for relay in [r1, r2, r3] {
			assert_eq!(sim.toward(relay, 7), None, "{}: relay {relay}", rule.as_str());
		}
	}
}

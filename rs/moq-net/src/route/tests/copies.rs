//! The cached-copy case of cache-aware routing, on the prototype in `paths.rs`.
//!
//! A relay with a reader for a broadcast carries a copy, and offers its neighbours
//! a route to that copy alone: the path the copy took from the publisher, and no
//! cost of its own. A relay pulls the broadcast from whichever is cheaper, the
//! publisher's route or a copy whose path does not name it, and passes a copy's
//! route on for as long as it beats the publisher's.

use super::paths::{PathSim, Rules, Source};
use super::*;

const HOME: Node = 0;
const NEAR: Node = 1;
const FAR: Node = 2;
const TAIL: Node = 3;
const ORIGIN: Node = 4;

/// Two relays with an uplink each to the publisher's relay, `NEAR`'s the cheaper,
/// and a cheap link between them. `TAIL` hangs off `FAR`.
fn uplinks(rules: Rules) -> PathSim {
	let mut sim = PathSim::new(5, rules);
	sim.leaf(ORIGIN);
	sim.join(HOME, NEAR, 10);
	sim.join(HOME, FAR, 12);
	sim.join(NEAR, FAR, 3);
	sim.join(FAR, TAIL, 1);
	sim.join(ORIGIN, HOME, 1);
	sim.originate(ORIGIN, 7);
	sim.announce(ORIGIN, "live");
	assert!(sim.run());
	sim
}

fn pulls(sim: &PathSim, node: Node) -> Option<(Node, Metric, bool)> {
	let source: Source = sim.source(node, 7, "live")?;
	Some((source.next, source.cost, source.warm))
}

/// How following each relay's source for the broadcast fares: relays whose sources
/// lead round a loop, and relays whose sources lead nowhere.
fn astray(sim: &PathSim) -> (usize, usize) {
	let (mut loops, mut lost) = (0, 0);
	for relay in [HOME, NEAR, FAR, TAIL] {
		let mut at = relay;
		let mut hops = 0;
		loop {
			match sim.source(at, 7, "live") {
				None if at == relay => break,
				None => {
					lost += 1;
					break;
				}
				Some(source) if source.next == ORIGIN => break,
				Some(source) if !sim.connected(at, source.next) => {
					lost += 1;
					break;
				}
				Some(source) => at = source.next,
			}
			hops += 1;
			if hops > 5 {
				loops += 1;
				break;
			}
		}
	}
	(loops, lost)
}

/// Deliver one message at a time, and sum what was astray after each.
fn settle(sim: &mut PathSim) -> (usize, usize) {
	let (mut loops, mut lost) = astray(sim);
	for _ in 0..10_000 {
		let Some(&(from, to)) = sim.busy().first() else {
			return (loops, lost);
		};
		sim.deliver(from, to);
		let (a, b) = astray(sim);
		loops += a;
		lost += b;
	}
	panic!("never settles");
}

/// `FAR` carries the broadcast and is farther from the publisher than `NEAR`.
/// `NEAR` joins that copy over the cheap link rather than pull its own over its
/// uplink. Babel's distance cannot allow this: `FAR`'s cost to the publisher is
/// not below what `NEAR` advertised, which is the check cache-aware routing's
/// eligibility rule makes (moq-dev/moq#8457).
#[test]
fn a_relay_joins_a_copy_no_closer_to_the_publisher() {
	for rules in [Rules::BARE, Rules::CAUSES, Rules::EPOCHS] {
		let mut sim = uplinks(rules);
		assert_eq!(pulls(&sim, NEAR), Some((HOME, 11, false)));
		assert_eq!(pulls(&sim, FAR), Some((HOME, 13, false)));

		let before = sim.stats;
		sim.carry(FAR, 7, "live");
		assert!(sim.run());
		assert_eq!(pulls(&sim, FAR), Some((HOME, 13, false)), "{rules:?}");
		assert_eq!(pulls(&sim, NEAR), Some((FAR, 3, true)), "{rules:?}");
		assert_eq!(pulls(&sim, TAIL), Some((FAR, 1, true)), "{rules:?}");
		// One message to each neighbour the copy's path does not name, and none onward:
		// every further neighbour is on that path.
		assert_eq!(sim.stats.warm_msgs - before.warm_msgs, 2, "{rules:?}");
		assert_eq!(sim.stats.route_msgs - before.route_msgs, 2, "{rules:?}");

		sim.uncarry(FAR, 7, "live");
		assert!(sim.run());
		assert_eq!(pulls(&sim, NEAR), Some((HOME, 11, false)), "{rules:?}");
		assert_eq!(pulls(&sim, TAIL), Some((FAR, 14, false)), "{rules:?}");
		assert_eq!(sim.stats.warm_msgs - before.warm_msgs, 4, "{rules:?}");
	}

	// The same mesh on today's router: the neighbour with the copy is the farther one.
	let mut sim = Sim::new(5);
	sim.join(HOME, NEAR, 10);
	sim.join(HOME, FAR, 12);
	sim.join(NEAR, FAR, 3);
	sim.join(FAR, TAIL, 1);
	sim.join(ORIGIN, HOME, 1);
	sim.originate(ORIGIN, 7, 0);
	sim.announce(ORIGIN, "live", 0, None);
	assert!(sim.run());
	let (near, far) = (sim.route(NEAR, 7).unwrap(), sim.route(FAR, 7).unwrap());
	assert_eq!((near.metric, far.metric), (11, 13));
	assert!(far.metric >= near.metric, "so the distance refuses the join");
}

/// Both relays carry a copy, `NEAR`'s pulled from `FAR`'s. `FAR` never takes
/// `NEAR`'s in return, since that copy came through `FAR`. When `FAR`'s uplink
/// closes the two swap, each still fed from the publisher. The swap loops for as
/// long as the news takes to cross their link, which a cause shortens.
#[test]
fn two_copies_never_feed_each_other() {
	for rules in [Rules::BARE, Rules::CAUSES, Rules::EPOCHS] {
		let mut sim = uplinks(rules);
		sim.carry(FAR, 7, "live");
		assert_eq!(settle(&mut sim), (0, 0), "{rules:?}");
		sim.carry(NEAR, 7, "live");
		assert_eq!(settle(&mut sim), (0, 0), "{rules:?}");
		assert_eq!(pulls(&sim, FAR), Some((HOME, 13, false)), "{rules:?}");
		assert_eq!(pulls(&sim, NEAR), Some((FAR, 3, true)), "{rules:?}");

		// Until `NEAR` hears, it still pulls from `FAR`, which now pulls from it.
		sim.disconnect(HOME, FAR);
		let (loops, _) = settle(&mut sim);
		let limit = match rules.causes {
			true => 3,
			false => 6,
		};
		assert!(
			loops <= limit,
			"{rules:?}: {loops} relays astray, summed over deliveries"
		);
		assert_eq!(pulls(&sim, NEAR), Some((HOME, 11, false)), "{rules:?}");
		assert_eq!(pulls(&sim, FAR), Some((NEAR, 3, true)), "{rules:?}");
		assert_eq!(pulls(&sim, TAIL), Some((FAR, 1, true)), "{rules:?}");
		assert_eq!(astray(&sim), (0, 0), "{rules:?}");
	}
}

/// Readers arrive at both relays in the same instant, so each pulls over its own
/// uplink and then hears of the other's copy. By their paths alone each would move
/// onto the other's, see its own copy's path come back, move off again, and so on
/// for good. Ranking the two lets exactly one move.
#[test]
fn two_copies_started_together_settle_on_one_uplink() {
	for rules in [Rules::BARE, Rules::CAUSES, Rules::EPOCHS] {
		let mut sim = uplinks(rules);
		sim.carry(FAR, 7, "live");
		sim.carry(NEAR, 7, "live");
		assert_eq!(settle(&mut sim).0, 0, "{rules:?}");
		let (near, far) = (pulls(&sim, NEAR).unwrap(), pulls(&sim, FAR).unwrap());
		assert!(
			[
				((HOME, 11, false), (NEAR, 3, true)),
				((FAR, 3, true), (HOME, 13, false))
			]
			.contains(&(near, far)),
			"{rules:?}: {near:?} {far:?}"
		);
		assert_eq!(astray(&sim), (0, 0), "{rules:?}");

		let unranked = Rules { ranked: false, ..rules };
		let mut sim = uplinks(unranked);
		sim.carry(FAR, 7, "live");
		sim.carry(NEAR, 7, "live");
		assert!(!sim.run(), "{rules:?}: the two trade copies for good");
	}
}

/// The publisher leaves while both relays carry a copy. Every route to either copy
/// goes with it and neither relay adopts the other's. A cause ends them without a
/// loop; a bare path leaves each relay to try the other's copy first.
#[test]
fn a_copy_that_goes_takes_its_routes_along() {
	for rules in [Rules::BARE, Rules::CAUSES, Rules::EPOCHS] {
		let mut sim = uplinks(rules);
		sim.carry(FAR, 7, "live");
		assert_eq!(settle(&mut sim), (0, 0), "{rules:?}");
		sim.carry(NEAR, 7, "live");
		assert_eq!(settle(&mut sim), (0, 0), "{rules:?}");

		sim.disconnect(ORIGIN, HOME);
		let (loops, _) = settle(&mut sim);
		for relay in [HOME, NEAR, FAR, TAIL] {
			assert_eq!(pulls(&sim, relay), None, "{rules:?}: relay {relay}");
			assert_eq!(sim.nodes[relay].retained(), (0, 0), "{rules:?}: relay {relay}");
		}
		assert_eq!(loops > 0, !rules.causes, "{rules:?}: {loops}");
	}
}

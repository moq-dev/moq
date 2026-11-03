//! Next-hop handoff: complete and fresh announcements across route changes.
//!
//! Each origin versions its own claims with a revision. A node sends each
//! neighbor its view of an origin's claims within that neighbor's interest,
//! closed by floors certifying the view complete as of a revision for the
//! region they cover. Receivers merge every neighbor's view claim by claim,
//! keeping whichever knowledge is newer, and keep showing an origin's claims
//! while any route to it is usable or recovering.

use std::collections::BTreeSet;

use super::*;

/// An origin reaching a node through two relays, the first preferred and the
/// second a feasible successor.
fn two_relays() -> Sim {
	let (origin, a, b, node) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(origin);
	sim.join(origin, a, 1);
	sim.join(origin, b, 1);
	sim.join(node, a, 1);
	sim.join(node, b, 2);
	sim.originate(origin, 7, 0);
	assert!(sim.run());
	sim
}

/// A withdrawal reaches the node through its next hop while the alternative's
/// copy is still in flight. When the next hop goes, the alternative's complete
/// snapshot is older than what the node already knows, and is refused.
#[test]
fn an_older_complete_snapshot_revives_no_known_withdrawal() {
	let (origin, a, b, node) = (0, 1, 2, 3);
	let mut sim = two_relays();
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	assert_eq!(sim.shows(node, 7, "live/x"), Some(true));

	sim.hold(origin, b);
	sim.unannounce(origin, "live/x");
	assert!(sim.run());
	assert_eq!(sim.shows(node, 7, "live/x"), None);
	// Forgetting another origin excuses no replay of this one.
	sim.collect(node, 99);

	sim.disconnect(node, a);
	assert!(sim.run());
	assert_eq!(sim.route(node, 7).and_then(|route| route.next), Some(b));
	assert_eq!(sim.shows(node, 7, "live/x"), None);

	sim.release(origin, b);
	assert!(sim.run());
	assert_eq!(sim.shows(node, 7, "live/x"), None);
	let events = at(&sim, node);
	assert_eq!(revivals(&events), 0);
	assert_eq!(kinds(&events), [Kind::Start, Kind::End]);
	sim.check().unwrap();
}

/// A new next hop's ROUTE arrives before its announcements. Merged views keep
/// what the node knows, and the reader sees nothing change.
#[test]
fn trailing_announcements_report_no_false_absence() {
	let (origin, a, b, node) = (0, 1, 2, 3);
	let mut sim = two_relays();
	sim.disconnect(node, b);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	let before = sim.events.len();

	sim.hold(b, node);
	sim.connect(node, b);
	assert!(sim.run());
	assert!(sim.queued(b, node) >= 2, "a ROUTE then its announcements");
	assert!(sim.deliver(b, node), "only the ROUTE");
	sim.disconnect(node, a);
	assert!(sim.run());
	assert_eq!(sim.route(node, 7).and_then(|route| route.next), Some(b));
	assert_eq!(sim.shows(node, 7, "live/x"), Some(true));

	sim.release(b, node);
	assert!(sim.run());
	assert_eq!(sim.shows(node, 7, "live/x"), Some(true));
	let events: Vec<&SimEvent> = sim.events[before..].iter().filter(|e| e.node == node).collect();
	assert!(events.is_empty(), "{events:?}");
}

/// The partitioned local mesh, with a client behind the starved relay. The
/// CDN detour is infeasible until the origin advances its seqno, and through
/// that window the relay and its client keep the claim unavailable rather
/// than ending it. The relay's seqno request recovers it through the CDN.
#[test]
fn a_starved_relay_keeps_presence_unavailable_until_its_request_recovers_it() {
	let (c1, c2, l1, l2, origin, client) = (0, 1, 2, 3, 4, 5);
	let mut sim = Sim::new(6);
	sim.join(c1, c2, 10);
	sim.join(l1, l2, 1);
	sim.link(l1, c1, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(l2, c2, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, l2, 1);
	sim.join(client, l1, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "local/x", 0, None);
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "local/x"), Some(true));

	// Hold the request so the starved window is observable.
	let before = sim.events.len();
	sim.hold(l1, c1);
	sim.disconnect(l1, l2);
	assert!(sim.run());
	assert!(sim.starved(l1, 7));
	assert!(sim.grounded(client, 7));
	for node in [l1, client] {
		assert_eq!(sim.shows(node, 7, "local/x"), Some(false), "node {node}");
	}
	assert_eq!(false_ends(&sim.events), 0);

	sim.release(l1, c1);
	assert!(sim.run());
	assert_eq!(sim.seqno(origin), 1, "the request reached the origin");
	assert_eq!(sim.route(l1, 7).and_then(|route| route.next), Some(c1));
	let events: Vec<SimEvent> = sim.events[before..]
		.iter()
		.filter(|e| e.node == client)
		.cloned()
		.collect();
	assert_eq!(kinds(&events), [Kind::Unavailable, Kind::Available]);
}

/// A client of two relays, one lagging behind its origin.
fn lagging(watch: &str) -> Sim {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, r1, 1);
	sim.join(origin, r2, 1);
	sim.join(client, r1, 1);
	sim.join(client, r2, 1);
	sim.watch(client, watch);
	sim.originate(origin, 7, 0);
	sim
}

/// A client watching `live/a` knows `live/a/x` ended, then widens to
/// everything. The lagging relay's wider snapshot must not revive `live/a/x`,
/// yet is the first to report `live/b/y`: floors scoped to the region they
/// cover do both.
#[test]
fn widening_interest_needs_floors_scoped_to_what_they_cover() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = lagging("live/a/");
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/b/y", 0, None);
	assert!(sim.run());
	sim.hold(origin, r2);
	sim.unannounce(origin, "live/a/x");
	assert!(sim.run());
	assert!(sim.shown(client).is_empty());

	sim.hold(r1, client);
	sim.watch(client, "");
	assert!(sim.run());
	sim.release(r1, client);
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "live/a/x"), None);
	assert_eq!(sim.shows(client, 7, "live/b/y"), Some(true));
	assert_eq!(revivals(&sim.events), 0);
	sim.check().unwrap();
}

/// Narrowing retires what a client knew outside the new interest. On rejoin a
/// lagging relay's snapshot replays a withdrawal the client saw before it
/// narrowed, as after a collection, and only until the newer snapshot lands.
#[test]
fn cancelling_interest_retires_its_region() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = lagging("");
	sim.announce(origin, "live/a/x", 0, None);
	assert!(sim.run());
	sim.hold(origin, r2);
	sim.unannounce(origin, "live/a/x");
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "live/a/x"), None);

	sim.watch(client, "live/b/");
	assert!(sim.run());
	assert_eq!(sim.floors(client, 7), 1, "only the watched region");
	sim.hold(r1, client);
	sim.watch(client, "");
	assert!(sim.run());
	assert_eq!(
		sim.shows(client, 7, "live/a/x"),
		Some(true),
		"the lagging relay replays"
	);
	sim.release(r1, client);
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "live/a/x"), None);
	sim.release(origin, r2);
	assert!(sim.run());
	assert_eq!(
		life(&sim, client, "live/a/x"),
		[Kind::Start, Kind::End, Kind::Start, Kind::End]
	);
	assert_eq!(revivals(&sim.events), 0, "a retired region, like a collection");
	assert_eq!(sim.floors(client, 7), 1);
	sim.check().unwrap();
}

/// A claim outside a narrowed interest ends unseen. On widening, what was
/// kept there stays hidden until a floor covering it arrives, rather than
/// showing a stale Start the next snapshot ends.
#[test]
fn widening_hides_what_was_frozen_until_a_covering_floor_arrives() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = lagging("");
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/b/y", 0, None);
	assert!(sim.run());
	sim.watch(client, "live/b/");
	assert!(sim.run());
	sim.unannounce(origin, "live/a/x");
	assert!(sim.run());

	sim.hold(r1, client);
	sim.hold(r2, client);
	sim.watch(client, "");
	assert_eq!(sim.shows(client, 7, "live/a/x"), None, "frozen while unwatched");
	assert_eq!(sim.shows(client, 7, "live/b/y"), Some(true), "still current");

	sim.release(r1, client);
	sim.release(r2, client);
	assert!(sim.run());
	assert_eq!(life(&sim, client, "live/a/x"), [Kind::Start, Kind::Unwatched]);
	sim.check().unwrap();
}

/// A snapshot sent before the client narrowed still waits when it widens
/// again. It answers the old interest, so it must not lift the guard on what
/// stayed frozen.
#[test]
fn a_snapshot_queued_before_widening_keeps_the_frozen_rest_hidden() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = lagging("");
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/b/y", 0, None);
	assert!(sim.run());
	sim.hold(r2, client);
	sim.announce(origin, "live/b/z", 0, None);
	assert!(sim.run());
	assert!(sim.queued(r2, client) > 0, "a root snapshot waits");
	sim.watch(client, "live/b/");
	assert!(sim.run());
	sim.unannounce(origin, "live/a/x");
	assert!(sim.run());

	sim.hold(r1, client);
	sim.watch(client, "");
	assert!(sim.deliver(r2, client), "the root snapshot from before narrowing");
	assert_eq!(sim.shows(client, 7, "live/a/x"), None, "it answers the old interest");

	sim.release(r1, client);
	sim.release(r2, client);
	assert!(sim.run());
	assert_eq!(life(&sim, client, "live/a/x"), [Kind::Start, Kind::Unwatched]);
	sim.check().unwrap();
}

/// A client behind a scoped grant widens its interest. Every batch it gets is
/// cut to the grant's scope, so no floor covers the widened interest, yet the
/// batch answering it shows the newly covered claims, and claim churn keeps
/// the certified regions bounded.
#[test]
fn widening_behind_a_scoped_grant_shows_the_rest_of_the_scope() {
	let (origin, relay, client) = (0, 1, 2);
	let mut sim = Sim::new(3);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, relay, 1);
	let grant = Grant {
		subject: "local",
		scope: "live/a/",
	};
	sim.link(client, relay, SimLink::cost(1).grant(grant), SimLink::cost(1));
	sim.watch(client, "live/a/x/");
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/a/x/1", 0, None);
	sim.announce(origin, "live/a/y", 0, None);
	assert!(sim.run());
	let id = qualified("local", 7);
	assert_eq!(sim.shows(client, id, "live/a/x/1"), Some(true));
	assert_eq!(sim.shows(client, id, "live/a/y"), None);

	sim.watch(client, "");
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/a/y"), Some(true));
	let certified = sim.retained().certified;
	for _ in 0..8 {
		sim.announce(origin, "live/a/z", 0, None);
		sim.unannounce(origin, "live/a/z");
		assert!(sim.run());
	}
	assert_eq!(sim.retained().certified, certified, "claim churn adds no state");
	assert!(
		sim.check().unwrap().contains(&(client, id)),
		"the qualified view is checked"
	);
}

/// Two scoped grants qualify one origin under one subject. After widening, a
/// batch through the first grant certifies only its own scope: a claim in the
/// second scope that ended while unwatched stays hidden until the second
/// grant answers too.
#[test]
fn widening_behind_two_grants_waits_for_each_scope() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = scoped_pair(["live/a/", "live/b/"]);
	sim.announce(origin, "live/a/x/1", 0, None);
	sim.announce(origin, "live/b/z", 0, None);
	assert!(sim.run());
	let id = qualified("local", 7);
	assert_eq!(sim.shows(client, id, "live/b/z"), Some(true));

	sim.hold(r1, client);
	sim.hold(r2, client);
	sim.watch(client, "live/a/x/");
	sim.unannounce(origin, "live/b/z");
	assert!(sim.run());
	sim.watch(client, "");
	while sim.deliver(r1, client) {}
	assert_eq!(sim.shows(client, id, "live/a/x/1"), Some(true));
	assert_eq!(sim.shows(client, id, "live/b/z"), None, "only the first scope answered");

	sim.release(r1, client);
	sim.release(r2, client);
	assert!(sim.run());
	assert_eq!(life(&sim, client, "live/b/z"), [Kind::Start, Kind::Unwatched]);
	sim.check().unwrap();
}

/// An origin reaching a client through two relays, each admitting the client
/// under a grant scoped to one of `scopes` with one subject.
fn scoped_pair(scopes: [&'static str; 2]) -> Sim {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = Sim::new(4);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, r1, 1);
	sim.join(origin, r2, 1);
	for (relay, scope) in [(r1, scopes[0]), (r2, scopes[1])] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		sim.link(client, relay, SimLink::cost(1).grant(grant), SimLink::cost(1));
	}
	sim.originate(origin, 7, 0);
	sim
}

/// One key arrives through two grants. When one grant's session closes,
/// nothing certifies its scope, so its claims go offline rather than staying
/// shown through the other grant, which could never deliver their withdrawal.
#[test]
fn a_closed_grant_lets_its_scope_go() {
	let (origin, r1, client) = (0, 1, 3);
	let mut sim = scoped_pair(["live/a/", "live/b/"]);
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/b/z", 0, None);
	assert!(sim.run());
	let id = qualified("local", 7);
	assert_eq!(sim.shows(client, id, "live/a/x"), Some(true));

	sim.disconnect(client, r1);
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/a/x"), None);
	assert_eq!(sim.shows(client, id, "live/b/z"), Some(true));
	sim.unannounce(origin, "live/a/x");
	sim.announce(origin, "live/a/y", 0, None);
	assert!(sim.run());
	sim.connect(client, r1);
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/a/x"), None);
	assert_eq!(sim.shows(client, id, "live/a/y"), Some(true));
	assert_eq!(life(&sim, client, "live/a/x"), [Kind::Start, Kind::Offline]);
	assert_eq!(false_ends(&sim.events), 0);
	sim.check().unwrap();
}

/// Two relays each admit an uplink scoped to a different prefix and forward
/// the key to a third, whose view carries one floor per scope.
fn two_scoped_uplinks() -> Sim {
	let (origin, local, x, y, z) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	sim.leaf(origin);
	sim.join(origin, local, 1);
	for (relay, scope) in [(x, "live/a/"), (y, "live/b/")] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		sim.link(relay, local, SimLink::cost(1).grant(grant), SimLink::cost(1));
		sim.join(relay, z, 1);
	}
	sim.originate(origin, 7, 0);
	sim
}

/// A relay forwards a key whose floors are all grant scopes, and its peer
/// moves to an interest no scope meets. The relay still answers the new
/// stream, certifying nothing, so the peer lets go of a claim it kept from the
/// scoped view instead of waiting on an answer that never comes.
#[test]
fn a_disjoint_interest_is_answered_with_nothing() {
	let (origin, z) = (0, 4);
	let mut sim = two_scoped_uplinks();
	sim.announce(origin, "live/", 0, None);
	assert!(sim.run());
	let id = qualified("local", 7);
	assert_eq!(sim.shows(z, id, "live/"), Some(true));

	sim.watch(z, "live/c");
	assert!(sim.run());
	assert_eq!(sim.shows(z, id, "live/"), None);
	assert_eq!(life(&sim, z, "live/"), [Kind::Start, Kind::Offline]);
	sim.check().unwrap();
}

/// An interest matches whole path segments: watching `live/a` sees what lies
/// below it, never a sibling `live/ab` that merely shares its bytes.
#[test]
fn an_interest_matches_whole_segments() {
	let (origin, client) = (0, 1);
	let mut sim = Sim::new(2);
	sim.leaf(origin);
	sim.join(origin, client, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/ab", 0, None);
	sim.watch(client, "live/a");
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "live/a/x"), Some(true));
	assert_eq!(sim.shows(client, 7, "live/ab"), None);
	sim.check().unwrap();
}

/// One key arrives through a wide grant and a narrow one. The narrow session
/// lags behind a withdrawal the wide one delivered; when the wide session
/// closes, only what lies outside the narrow scope goes offline, and the newer
/// knowledge inside it stays rather than reviving the withdrawn claim.
#[test]
fn a_lost_scope_keeps_newer_knowledge_where_still_certified() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = scoped_pair(["live/", "live/b/"]);
	sim.announce(origin, "live/a/x", 0, None);
	sim.announce(origin, "live/b/z", 0, None);
	assert!(sim.run());
	let id = qualified("local", 7);
	assert_eq!(sim.shows(client, id, "live/b/z"), Some(true));

	sim.hold(r2, client);
	sim.unannounce(origin, "live/b/z");
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/b/z"), None);
	sim.disconnect(client, r1);
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/a/x"), None, "outside the scope left");
	assert_eq!(
		sim.shows(client, id, "live/b/z"),
		None,
		"the lagging scope revives nothing"
	);
	sim.check().unwrap();

	sim.release(r2, client);
	assert!(sim.run());
	assert_eq!(sim.shows(client, id, "live/b/z"), None);
	assert_eq!(revivals(&at(&sim, client)), 0);
	sim.check().unwrap();
}

/// When one scoped uplink closes, its relay is starved and holds, so the third
/// relay shows that scope unavailable; once a seqno request recovers a route,
/// nothing certifies the lost scope and the third lets it go. Certifiers are
/// feasible neighbors only, so the two never keep the scope alive through each
/// other.
#[test]
fn a_relay_lets_go_of_a_scope_its_uplinks_lost() {
	let (origin, local, x, z) = (0, 1, 2, 4);
	for request in [false, true] {
		let mut sim = two_scoped_uplinks();
		sim.announce(origin, "live/a/x", 0, None);
		sim.announce(origin, "live/b/z", 0, None);
		assert!(sim.run());
		let id = qualified("local", 7);
		assert_eq!(sim.shows(z, id, "live/a/x"), Some(true));
		assert_eq!(sim.shows(z, id, "live/b/z"), Some(true));

		if !request {
			// Hold every request at the starved relay.
			for peer in [z, local] {
				sim.hold(x, peer);
			}
		}
		sim.disconnect(x, local);
		sim.unannounce(origin, "live/a/x");
		assert!(sim.run());
		let lost = if request { None } else { Some(false) };
		if !request {
			// Only the ROUTE update reaches z, not the requests.
			while sim.deliver(x, z) {}
			assert!(sim.run());
		}
		assert_eq!(sim.shows(z, id, "live/a/x"), lost, "request {request}");
		assert_eq!(sim.shows(z, id, "live/b/z"), Some(true), "request {request}");
		assert_eq!(false_ends(&sim.events), 0, "request {request}");
		sim.check().unwrap();
	}
}

/// Every lite-07 claim covers the paths below it, so a claim above an
/// interest is inside it.
#[test]
fn a_claim_above_an_interest_overlaps_it() {
	let (origin, client) = (0, 3);
	let mut sim = lagging("live/a/x");
	sim.announce(origin, "live/a", 0, None);
	sim.announce(origin, "live/b", 0, None);
	assert!(sim.run());
	assert_eq!(sim.shows(client, 7, "live/a"), Some(true));
	assert_eq!(sim.shows(client, 7, "live/b"), None);
	sim.check().unwrap();
}

/// Two origins hold the same wildcard capability for one epoch, and a client
/// watches a region below it. Withdrawing one origin's wildcard ends only that
/// origin's claim, and a lagging relay's older copy of it is refused after the
/// client's next hop moves there.
#[test]
fn wildcard_claims_end_per_origin_and_are_certified_below() {
	let (o1, o2, r1, r2, client) = (0, 1, 2, 3, 4);
	let mut sim = Sim::new(5);
	for origin in [o1, o2] {
		sim.leaf(origin);
		sim.join(origin, r1, 1);
		sim.join(origin, r2, 1);
	}
	sim.leaf(client);
	sim.join(client, r1, 1);
	sim.join(client, r2, 1);
	sim.watch(client, "live/a/");
	sim.originate(o1, 10, 0);
	sim.originate(o2, 11, 0);
	sim.announce(o1, "live/", 0, Some(1));
	sim.announce(o2, "live/", 0, Some(1));
	assert!(sim.run());
	assert_eq!(sim.shown(client).len(), 2);

	sim.hold(o1, r2);
	sim.unannounce(o1, "live/");
	assert!(sim.run());
	assert_eq!(sim.shows(client, 10, "live/"), None);
	assert_eq!(
		sim.shows(client, 11, "live/"),
		Some(true),
		"the other origin's claim stays"
	);

	sim.disconnect(client, r1);
	assert!(sim.run());
	assert_eq!(sim.shows(client, 10, "live/"), None);
	assert_eq!(sim.shows(client, 11, "live/"), Some(true));
}

/// An origin process restarts with a fresh identity: its claims start a new
/// revision history and the old identity's go offline once nothing can
/// recover them. Reusing the identity would restart its revision below the
/// floors other nodes hold: they keep the dead process's claims and refuse
/// the new ones.
#[test]
fn origin_restart_takes_a_fresh_identity_for_its_claims() {
	let (origin, a, b) = (0, 1, 2);
	for fresh in [false, true] {
		let mut sim = Sim::new(3);
		sim.leaf(origin);
		sim.join(origin, a, 1);
		sim.join(a, b, 1);
		sim.originate(origin, 7, 0);
		sim.announce(origin, "live/x", 0, None);
		sim.unannounce(origin, "live/x");
		sim.announce(origin, "live/y", 0, None);
		assert!(sim.run());

		sim.kill(origin);
		assert!(sim.run());
		for node in [a, b] {
			assert!(sim.shown(node).is_empty());
			assert!(!sim.holds(node, 7), "nothing can recover the dead identity");
		}
		let offline = sim.events.iter().filter(|e| e.kind == Kind::Offline).count();
		assert_eq!(offline, 2, "offline, not ended by the origin");
		assert_eq!(false_ends(&sim.events), 0);

		let id = if fresh { 8 } else { 7 };
		sim.originate(origin, id, 10);
		sim.reconnect(origin);
		sim.announce(origin, "live/x", 0, None);
		assert!(sim.run());
		for node in [a, b] {
			if fresh {
				assert_eq!(sim.shows(node, 8, "live/x"), Some(true));
				assert_eq!(sim.shown(node).len(), 1);
			} else {
				assert_eq!(sim.shows(node, 7, "live/y"), Some(true), "the dead process's claim");
				assert_eq!(sim.shows(node, 7, "live/x"), None);
			}
		}
	}
}

/// Node b learned that `live/x` ended, then lost every route and collected the
/// origin. A session still delivering older messages brings the claim back,
/// but only until the withdrawal queued behind it lands. Without collection,
/// b's retained floor refuses the replay outright.
#[test]
fn stale_replay_after_collection_lasts_only_until_the_withdrawal_behind_it() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	for collect in [false, true] {
		let mut sim = Sim::new(4);
		sim.leaf(origin);
		sim.join(origin, a, 1);
		sim.join(a, b, 1);
		sim.join(a, c, 1);
		sim.join(c, b, 1);
		sim.hold(a, b);
		sim.originate(origin, 7, 0);
		sim.announce(origin, "live/x", 0, None);
		assert!(sim.run());
		sim.unannounce(origin, "live/x");
		assert!(sim.run());
		assert_eq!(kinds(&at(&sim, b)), [Kind::Start, Kind::End]);

		sim.kill(origin);
		assert!(sim.run());
		assert!(!sim.holds(b, 7));
		if collect {
			sim.collect(b, 7);
		}
		// The held ROUTE, an empty snapshot, then the one carrying `live/x`.
		for _ in 0..3 {
			assert!(sim.deliver(a, b));
		}
		assert_eq!(sim.shows(b, 7, "live/x").is_some(), collect);
		assert!(sim.deliver(a, b), "the withdrawal");
		assert_eq!(sim.shows(b, 7, "live/x"), None);
		sim.release(a, b);
		assert!(sim.run());
		let replayed: &[Kind] = if collect {
			&[Kind::Start, Kind::End, Kind::Start, Kind::End]
		} else {
			&[Kind::Start, Kind::End]
		};
		assert_eq!(kinds(&at(&sim, b)), replayed);
		assert!(!sim.holds(b, 7));
	}
}

/// Holds follow feasible advertisements only, so they cannot prop each other
/// up. When the origin dies, every node in a ring goes offline and keeps no
/// route state, with no timer, and none reports an END the origin did not send.
#[test]
fn holds_retire_once_nothing_can_recover() {
	let origin = 4;
	let mut sim = Sim::new(5);
	for relay in 0..4 {
		sim.join(relay, (relay + 1) % 4, 1);
	}
	sim.leaf(origin);
	sim.join(origin, 0, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());

	sim.kill(origin);
	assert!(sim.run());
	for relay in 0..4 {
		assert!(sim.shown(relay).is_empty());
		assert!(!sim.holds(relay, 7), "relay {relay}");
		assert!(!sim.remembers(relay, 7), "relay {relay} drained its distance");
	}
	assert_eq!(false_ends(&sim.events), 0);
	assert_eq!(sim.retained().pending, 0);
}

/// Seqnos and revisions are varints. An origin never advertises the last
/// value: a bump, a claim change, or a request that would reach it rotates the
/// identity instead, so every node lets the old one go and sees the claims
/// again under the new one. Nothing wraps.
#[test]
fn exhausted_counters_rotate_the_identity() {
	let (origin, a, b, c, client) = (0, 1, 2, 3, 4);
	let rotated = |sim: &Sim| {
		let fresh = sim.proven(origin);
		assert_ne!(fresh, 7, "a fresh identity");
		assert_eq!(sim.shows(client, 7, "live/x"), None);
		assert_eq!(sim.shows(client, fresh, "live/x"), Some(true));
		assert!(sim.route(a, 7).is_none());
		sim.check().unwrap();
	};
	let build = |seqno| {
		let mut sim = Sim::new(5);
		sim.leaf(origin);
		sim.leaf(client);
		sim.join(origin, a, 1);
		sim.join(a, b, 1);
		sim.join(b, c, 1);
		sim.join(origin, c, 1);
		sim.join(client, a, 1);
		sim.disconnect(origin, c);
		sim.originate(origin, 7, seqno);
		sim.announce(origin, "live/x", 0, None);
		assert!(sim.run());
		sim
	};

	let mut sim = build(MAX - 2);
	sim.bump(origin);
	assert!(sim.run());
	assert_eq!(sim.proven(origin), 7);
	sim.bump(origin);
	assert!(sim.run());
	rotated(&sim);

	let mut sim = build(0);
	sim.age(origin, MAX - 2);
	sim.announce(origin, "live/y", 0, None);
	assert!(sim.run());
	assert_eq!(sim.proven(origin), 7);
	sim.unannounce(origin, "live/y");
	assert!(sim.run());
	rotated(&sim);

	// The origin moves to a farther attachment, and the starved node asks for
	// the last seqno.
	let mut sim = build(MAX - 1);
	sim.disconnect(origin, a);
	sim.connect(origin, c);
	assert!(sim.run());
	rotated(&sim);
	assert!(sim.route(a, sim.proven(origin)).is_some());
}

/// Node b forwards a request along its route through x, whose own route just
/// failed but whose retraction to b is still in flight, so x drops it. When
/// the retraction lands, b moves to an equal-metric route through y and
/// advertises nothing new. Keeping the request at every hop that forwarded
/// it, and resending it when that hop's next hop changes, reaches the origin
/// with no timer.
#[test]
fn a_quiet_origin_is_reached_by_a_kept_request() {
	let (origin, w, x, y2, y, b, n, z, client) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
	let mut sim = Sim::new(9);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, w, 1);
	sim.join(w, x, 1);
	sim.join(origin, y2, 1);
	sim.join(y2, y, 1);
	sim.link(b, x, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.link(b, y, SimLink::cost(1).upstream(), SimLink::cost(1));
	sim.join(b, n, 1);
	sim.join(n, z, 1);
	sim.join(z, origin, 1);
	sim.join(n, client, 1);
	sim.originate(origin, 7, 0);
	sim.announce(origin, "live/x", 0, None);
	assert!(sim.run());
	assert_eq!(sim.route(n, 7).and_then(|route| route.next), Some(z));
	assert_eq!(sim.route(b, 7).and_then(|route| route.next), Some(x));

	sim.disconnect(n, z);
	sim.hold(x, b);
	sim.disconnect(w, x);
	assert!(sim.run());
	assert!(sim.starved(n, 7));
	sim.release(x, b);
	assert!(sim.run());
	assert_eq!(sim.route(b, 7).and_then(|route| route.next), Some(y));
	assert_eq!(sim.shows(client, 7, "live/x"), Some(true));
	assert_eq!(sim.seqno(origin), 1);
	assert!(sim.route(n, 7).is_some());
}

/// Random meshes churn sessions and hold and deliver messages out of step,
/// then release every hold. Every relay that can still reach a quiet origin
/// ends with a route to it, which only a seqno request can bring.
#[test]
fn sweep_requests_reach_a_quiet_origin() {
	const RELAYS: usize = 5;
	let mut stranded = 0;
	for seed in 0..300u64 {
		let mut rng = XorShift(0x9e37_79b9 ^ (seed * 0x1234_5678_9abc));
		let nodes = RELAYS + 2;
		let mut sim = Sim::new(nodes);
		let mut links = Vec::new();
		for a in 0..RELAYS {
			for b in a + 1..RELAYS {
				if b == a + 1 || rng.chance(40) {
					sim.join(a, b, 1 + rng.next(3));
					links.push((a, b));
				}
			}
		}
		for origin in RELAYS..nodes {
			sim.leaf(origin);
			for _ in 0..2 {
				let relay = rng.next(RELAYS as u64) as usize;
				if !links.contains(&(relay, origin)) {
					sim.join(origin, relay, 1 + rng.next(3));
					links.push((relay, origin));
				}
			}
			sim.originate(origin, origin as u64, 0);
		}
		assert!(sim.run());
		for _ in 0..30 {
			match rng.next(6) {
				0 | 1 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					if sim.connected(a, b) {
						sim.disconnect(a, b);
					} else {
						sim.connect(a, b);
					}
				}
				2 | 3 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					if rng.chance(60) {
						sim.hold(from, to);
					} else {
						sim.release(from, to);
					}
				}
				_ => {
					let busy = sim.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						sim.deliver(from, to);
					}
				}
			}
			sim.settle(1);
		}
		for &(a, b) in &links {
			sim.release(a, b);
			sim.release(b, a);
		}
		assert!(sim.run());
		for origin in RELAYS..nodes {
			// The relays an up session path still joins to the origin.
			let mut reach = BTreeSet::from([origin]);
			let mut frontier = vec![origin];
			while let Some(at) = frontier.pop() {
				for &(a, b) in &links {
					let next = match at {
						_ if at == a => b,
						_ if at == b => a,
						_ => continue,
					};
					if next < RELAYS && sim.connected(a, b) && reach.insert(next) {
						frontier.push(next);
					}
				}
			}
			stranded += reach
				.iter()
				.filter(|&&node| node < RELAYS && sim.route(node, origin as Id).is_none())
				.count();
		}
	}
	assert_eq!(stranded, 0, "relays left without a route to a reachable origin");
}

/// One change to a claim costs one batch per session carrying its origin's
/// route and one event per node watching it; retained state follows sessions,
/// live claims, and current interest, and neither withdrawal history nor
/// interest history leaves anything behind.
#[test]
fn announcement_cost_follows_sessions_and_live_claims() {
	const PREFIXES: [&str; 4] = ["live/a", "live/b", "live/c", "live/d"];
	for (relays, claims) in [(4, 2), (8, 2), (4, 4)] {
		let mut sim = Sim::new(relays + 2);
		for relay in 0..relays {
			sim.join(relay, (relay + 1) % relays, 1);
		}
		let origin = relays;
		sim.leaf(origin);
		sim.join(origin, 0, 1);
		// A client watching a region with no claims yet.
		let client = relays + 1;
		sim.leaf(client);
		sim.join(client, relays / 2, 1);
		sim.watch(client, "none/");
		sim.originate(origin, 7, 0);
		for prefix in &PREFIXES[..claims] {
			sim.announce(origin, prefix, 0, None);
		}
		assert!(sim.run());

		// Each relay sends to both ring neighbors, and the origin and its relay to
		// each other: without split horizon the relay echoes the origin's claims.
		let sessions = 2 * relays + 2;
		let retained = sim.retained();
		assert_eq!(retained.items, relays * claims);
		assert_eq!(retained.floors, relays + 1);
		assert_eq!(retained.heard, sessions * claims);
		assert_eq!(retained.sent, retained.heard, "both ends cache the same view");

		let (stats, events) = (sim.stats, sim.events.len());
		sim.unannounce(origin, PREFIXES[0]);
		assert!(sim.run());
		// The client's session carries only the advanced floor.
		assert_eq!(sim.stats.claim_msgs - stats.claim_msgs, sessions + 1);
		assert_eq!(sim.stats.claim_items - stats.claim_items, sessions);
		assert_eq!(sim.events.len() - events, relays + 1, "once per node");

		for _ in 0..50 {
			sim.announce(origin, PREFIXES[0], 0, None);
			sim.unannounce(origin, PREFIXES[0]);
		}
		assert!(sim.run());
		let after = sim.retained();
		assert_eq!(after.items, relays * (claims - 1));
		assert_eq!(after.floors, retained.floors, "no tombstones");
		assert_eq!(after.echoes, 0, "every ROUTE_END echo drained");
		sim.check().unwrap();

		// The client cycles through disjoint regions, one with a live claim.
		for cycles in [1, 16] {
			for room in 0..cycles {
				sim.watch(client, &format!("room/{room}"));
				assert!(sim.run());
				sim.watch(client, PREFIXES[1]);
				assert!(sim.run());
				assert_eq!(sim.shows(client, 7, PREFIXES[1]), Some(true));
			}
			sim.watch(client, "none/");
			assert!(sim.run());
			assert_eq!(sim.retained(), after, "interest history after {cycles} cycles");
		}
		sim.check().unwrap();
	}
}

/// A client reaches one origin through two relays, each session scoped by a
/// grant under one subject, so both feed one qualified view. Under churn every
/// view stays some revision of the origin's truth, the widening guard stays
/// bounded, no claim ends while the origin still has it, and once churn stops
/// the client shows exactly the claims its interest and the scopes admit.
#[test]
fn sweep_scoped_handoff() {
	const CLAIMS: [&str; 5] = ["live/a/x", "live/a/y/", "live/b/z", "live/c", "live/"];
	const WATCHES: [&str; 5] = ["", "live/", "live/a/", "live/b/z", "live/c"];
	const SCOPES: [&str; 2] = ["live/a/", "live/b/"];
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut rng = XorShift(0x5c0b_ed00);
	let mut live_starts = 0;
	for scenario in 0..150 {
		let mut sim = Sim::new(4);
		sim.leaf(origin);
		sim.leaf(client);
		sim.join(origin, r1, 1);
		sim.join(origin, r2, 1);
		sim.join(r1, r2, 1);
		for (relay, scope) in [(r1, SCOPES[0]), (r2, SCOPES[1])] {
			let grant = Grant {
				subject: "local",
				scope,
			};
			sim.link(client, relay, SimLink::cost(1).grant(grant), SimLink::cost(1));
		}
		let links = [(origin, r1), (origin, r2), (r1, r2), (client, r1), (client, r2)];
		sim.originate(origin, 7, 0);
		assert!(sim.run());
		for _ in 0..40 {
			match rng.next(8) {
				0..=2 => {
					let claim = CLAIMS[rng.next(CLAIMS.len() as u64) as usize];
					if rng.chance(60) {
						sim.announce(origin, claim, rng.next(3), None);
					} else {
						sim.unannounce(origin, claim);
					}
				}
				3 => sim.watch(client, WATCHES[rng.next(WATCHES.len() as u64) as usize]),
				4 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					if sim.connected(a, b) {
						sim.disconnect(a, b);
					} else {
						sim.connect(a, b);
					}
				}
				5 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					if rng.chance(60) {
						sim.hold(from, to);
					} else {
						sim.release(from, to);
					}
				}
				_ => {
					let busy = sim.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						sim.deliver(from, to);
					}
				}
			}
			sim.settle(1);
			sim.check().unwrap_or_else(|err| panic!("scenario {scenario}: {err}"));
			assert!(
				sim.retained().certified <= SCOPES.len() + 1,
				"scenario {scenario}: the widening guard grows"
			);
		}
		for (a, b) in links {
			sim.release(a, b);
			sim.release(b, a);
			if !sim.connected(a, b) {
				sim.connect(a, b);
			}
		}
		assert!(sim.run());
		sim.check().unwrap_or_else(|err| panic!("scenario {scenario}: {err}"));
		let watch = sim.watching(client).clone();
		let rev = sim.revision(7);
		let expected: BTreeSet<Path> = CLAIMS
			.into_iter()
			.filter(|claim| sim.truth(7, claim, rev).is_some())
			.map(p)
			.filter(|claim| {
				SCOPES.iter().map(|scope| p(scope)).any(|scope| {
					let region = if within(&scope, &watch) {
						scope
					} else if within(&watch, &scope) {
						watch.clone()
					} else {
						return false;
					};
					overlaps(claim, &region)
				})
			})
			.collect();
		let id = qualified("local", 7);
		let shown: BTreeSet<Path> = sim
			.shown(client)
			.iter()
			.filter(|((shown, _), view)| *shown == id && view.available)
			.map(|((_, claim), _)| claim.clone())
			.collect();
		assert_eq!(shown, expected, "scenario {scenario} watching {watch:?}");
		assert_eq!(false_ends(&sim.events), 0, "scenario {scenario}");
		live_starts += sim
			.events
			.iter()
			.filter(|event| event.id == id && event.kind == Kind::Start && !event.withdrawn)
			.count();
	}
	assert!(live_starts > 0);
}

const RELAYS: usize = 5;
const CLAIMS: [&str; 4] = ["live/", "live/a/", "live/a/x", "live/b"];

/// Random meshes with two origins and a client under churn: claims change,
/// sessions drop and return, messages are held and replayed out of step, the
/// client changes interest, nodes collect route-less identities, and an
/// origin restarts with a fresh identity. Selected routes never loop, every
/// view is always some revision of the origin's truth, nothing revives and
/// nothing ends falsely, and once churn stops every node shows every live
/// origin's current claims and holds nothing for a dead one.
#[test]
fn sweep_handoff() {
	let mut rng = XorShift(0x4a4d_0ff5);
	for scenario in 0..150 {
		let mut sim = churn(&mut rng);
		let label = format!("scenario {scenario}");
		assert_eq!(revivals(&sim.events), 0, "{label}");
		assert_eq!(false_ends(&sim.events), 0, "{label}");
		settled(&mut sim, &label);
	}
}

/// Origins sit at `RELAYS` and `RELAYS + 1`, the client at `RELAYS + 2`.
fn churn(rng: &mut XorShift) -> Sim {
	let (first, client) = (RELAYS, RELAYS + 2);
	let mut sim = Sim::new(RELAYS + 3);
	let mut links = Vec::new();
	for relay in 0..RELAYS {
		links.push((relay, (relay + 1) % RELAYS));
	}
	let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
	if a.abs_diff(b) > 1 && a.abs_diff(b) != RELAYS - 1 {
		links.push((a, b));
	}
	for leaf in [first, first + 1, client] {
		sim.leaf(leaf);
		let a = rng.next(RELAYS as u64) as usize;
		links.push((leaf, a));
		let b = (a + 1 + rng.next(RELAYS as u64 - 1) as usize) % RELAYS;
		links.push((leaf, b));
	}
	for &(a, b) in &links {
		sim.join(a, b, 1 + rng.next(3));
	}
	let mut ids = [100, 200];
	sim.originate(first, ids[0], 0);
	sim.originate(first + 1, ids[1], 0);
	assert!(sim.run());

	for _ in 0..40 {
		match rng.next(10) {
			0..=3 => {
				let origin = rng.next(2) as usize;
				let claim = CLAIMS[rng.next(CLAIMS.len() as u64) as usize];
				if rng.chance(50) {
					sim.announce(first + origin, claim, rng.next(3), None);
				} else {
					sim.unannounce(first + origin, claim);
				}
			}
			4 => {
				let (a, b) = links[rng.next(links.len() as u64) as usize];
				if sim.connected(a, b) {
					sim.disconnect(a, b);
				} else {
					sim.connect(a, b);
				}
			}
			5 => {
				let (a, b) = links[rng.next(links.len() as u64) as usize];
				let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
				if rng.chance(70) {
					sim.hold(from, to);
				} else {
					sim.release(from, to);
				}
			}
			6 => {
				sim.watch(client, ["", "live/a/", "live/b"][rng.next(3) as usize]);
			}
			7 if rng.chance(50) => {
				// Collection is allowed once a node holds no route to an identity.
				let node = rng.next(RELAYS as u64 + 3) as usize;
				let id = ids[rng.next(2) as usize];
				if !sim.holds(node, id) && (RELAYS..RELAYS + 2).all(|origin| node != origin) {
					sim.collect(node, id);
				}
			}
			7 if rng.chance(40) => {
				let origin = rng.next(2) as usize;
				sim.kill(first + origin);
				ids[origin] += 1;
				sim.originate(first + origin, ids[origin], 0);
				sim.reconnect(first + origin);
			}
			_ => {
				// Echoed ROUTE_ENDs interleave freely with the echoing node's routes.
				for _ in 0..rng.next(6) {
					let busy: Vec<(bool, usize, usize)> = (sim.busy().into_iter().map(|(from, to)| (false, from, to)))
						.chain(sim.busy_acks().into_iter().map(|(from, to)| (true, from, to)))
						.collect();
					if busy.is_empty() {
						break;
					}
					match busy[rng.next(busy.len() as u64) as usize] {
						(false, from, to) => sim.deliver(from, to),
						(true, from, to) => sim.deliver_ack(from, to),
					};
				}
			}
		}
		sim.check().unwrap();
		for id in (100..=ids[0]).chain(200..=ids[1]) {
			assert_loop_free(&sim, RELAYS + 3, id);
		}
	}

	for &(a, b) in &links {
		sim.release(a, b);
		sim.release(b, a);
		if !sim.connected(a, b) {
			sim.connect(a, b);
		}
	}
	assert!(sim.run());
	sim
}

/// After churn stops with every session up, each node shows exactly the live
/// origins' current claims within its interest, available, and keeps nothing
/// for an identity whose process is gone.
fn settled(sim: &mut Sim, label: &str) {
	sim.check().unwrap();
	let live: Vec<Id> = (RELAYS..RELAYS + 2).map(|origin| sim.proven(origin)).collect();
	for node in 0..RELAYS + 3 {
		let mut expected = BTreeSet::new();
		for &id in &live {
			assert!(
				sim.route(node, id).is_some(),
				"{label}: node {node} has no route to {id}"
			);
			for claim in CLAIMS {
				if overlaps(&p(claim), sim.watching(node)) && sim.truth(id, claim, sim.revision(id)).is_some() {
					expected.insert((id, p(claim)));
				}
			}
		}
		let shown: BTreeSet<(Id, Path)> = sim.shown(node).keys().cloned().collect();
		assert_eq!(shown, expected, "{label}: node {node}");
		for ((id, claim), shown) in sim.shown(node) {
			assert!(shown.available, "{label}: node {node} {id} {claim}");
			assert_eq!(
				sim.truth(*id, claim.as_str(), sim.revision(*id)),
				Some(shown.item),
				"{label}: node {node} {id} {claim}"
			);
		}
		for id in (100..110).chain(200..210).filter(|id| !live.contains(id)) {
			assert!(!sim.holds(node, id), "{label}: node {node} holds dead {id}");
			assert!(
				!sim.remembers(node, id),
				"{label}: node {node} keeps dead {id}'s distance"
			);
		}
	}
	assert_eq!(sim.retained().pending, 0, "{label}");
}

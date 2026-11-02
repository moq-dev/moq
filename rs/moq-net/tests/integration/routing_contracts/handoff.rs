//! Next-hop handoff: complete and fresh announcements across route changes.
//!
//! Each origin versions its own claims with a revision that starts at zero for
//! each routing identity. A node sends each neighbor its view of an origin's
//! claims within that neighbor's interest, closed by floors that certify the
//! view complete as of a revision for the region they cover. Receivers merge
//! every neighbor's view claim by claim, keeping whichever knowledge is newer,
//! and keep showing an origin's claims while any route to it is usable or
//! recovering. See `quest/m1/cluster-routing/routes.md`.

use std::collections::BTreeSet;

use super::XorShift;
use super::model::{
	Config, Event, Freshness, Grant, Id, Kind, Link, Net, Requests, VARINT_MAX, overlaps, qualified, within,
};

const NEXT_HOP: Freshness = Freshness::NextHop;
const BARRIER: Freshness = Freshness::Barrier;
const FLOORS: Freshness = Freshness::Floors { scoped: true };

fn config(freshness: Freshness, requests: Requests) -> Config {
	Config {
		freshness,
		requests,
		..Config::default()
	}
}

fn at(net: &Net, node: usize) -> Vec<Event> {
	net.events.iter().filter(|event| event.node == node).copied().collect()
}

fn kinds(events: &[Event]) -> Vec<Kind> {
	events.iter().map(|event| event.kind).collect()
}

/// Starts of a claim's life that the same node had already shown ending.
fn revivals(events: &[Event]) -> usize {
	let mut ended = BTreeSet::new();
	let mut count = 0;
	for event in events {
		let life = (event.node, event.forgot, event.id, event.claim, event.since);
		match event.kind {
			Kind::End => {
				ended.insert(life);
			}
			Kind::Start if ended.contains(&life) => count += 1,
			_ => {}
		}
	}
	count
}

/// Ends shown while the origin still had that life of the claim.
fn false_ends(events: &[Event]) -> usize {
	events
		.iter()
		.filter(|event| event.kind == Kind::End && !event.withdrawn)
		.count()
}

/// Starts of a claim's life that the same node had already shown going away.
fn reannouncements(events: &[Event]) -> usize {
	let mut gone = BTreeSet::new();
	let mut count = 0;
	for event in events {
		let life = (event.node, event.forgot, event.id, event.claim, event.since);
		match event.kind {
			Kind::End | Kind::Offline => {
				gone.insert(life);
			}
			Kind::Start if gone.contains(&life) => count += 1,
			_ => {}
		}
	}
	count
}

/// An origin reaching a node through two relays, the first preferred and the
/// second a feasible successor.
fn two_relays(freshness: Freshness) -> Net {
	let (origin, a, b, node) = (0, 1, 2, 3);
	let mut net = Net::new(config(freshness, Requests::Off), 4);
	net.leaf(origin);
	net.join(origin, a, 1);
	net.join(origin, b, 1);
	net.join(node, a, 1);
	net.join(node, b, 2);
	net.originate(origin, 7, 0);
	assert!(net.run());
	net
}

/// A withdrawal reaches the node through its next hop while the alternative's
/// copy is still in flight. When the next hop goes, the alternative's complete
/// snapshot is older than what the node already knows: adopting it, by next-hop
/// authority or behind a snapshot barrier, revives the claim until the
/// withdrawal catches up. Floors refuse the older snapshot.
#[test]
fn an_older_complete_snapshot_revives_a_known_withdrawal() {
	let (origin, a, b, node) = (0, 1, 2, 3);
	for freshness in [NEXT_HOP, BARRIER, FLOORS] {
		let mut net = two_relays(freshness);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		assert_eq!(net.shows(node, 7, "live/x"), Some(true));

		net.hold(origin, b);
		net.unannounce(origin, "live/x");
		assert!(net.run());
		assert_eq!(net.shows(node, 7, "live/x"), None, "{freshness:?}");
		// Forgetting another origin excuses no replay of this one.
		net.collect(node, 99);

		net.disconnect(node, a);
		assert!(net.run());
		assert_eq!(net.route(node, 7).and_then(|route| route.next), Some(b));
		let revived = net.shows(node, 7, "live/x").is_some();
		assert_eq!(revived, freshness != FLOORS, "{freshness:?}");

		net.release(origin, b);
		assert!(net.run());
		assert_eq!(net.shows(node, 7, "live/x"), None);
		let events = at(&net, node);
		assert_eq!(revivals(&events), usize::from(freshness != FLOORS), "{freshness:?}");
		if freshness == FLOORS {
			assert_eq!(kinds(&events), [Kind::Start, Kind::End]);
			net.check().unwrap();
		}
	}
}

/// A new next hop's ROUTE arrives before its announcements. Next-hop authority
/// shows the new next hop's empty set, a false END followed by a
/// re-announcement; merged views keep what the node knows and the reader sees
/// nothing change.
#[test]
fn next_hop_authority_reports_false_absence_while_announcements_trail() {
	let (origin, a, b, node) = (0, 1, 2, 3);
	for freshness in [NEXT_HOP, FLOORS] {
		let mut net = two_relays(freshness);
		net.disconnect(node, b);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		let before = net.events.len();

		net.hold(b, node);
		net.connect(node, b);
		assert!(net.run());
		assert!(net.queued(b, node) >= 2, "a ROUTE then its announcements");
		assert!(net.deliver(b, node), "only the ROUTE");
		net.disconnect(node, a);
		assert!(net.run());
		assert_eq!(net.route(node, 7).and_then(|route| route.next), Some(b));
		let trailing = net.shows(node, 7, "live/x");

		net.release(b, node);
		assert!(net.run());
		assert_eq!(net.shows(node, 7, "live/x"), Some(true));
		let events: Vec<Event> = net.events[before..]
			.iter()
			.filter(|e| e.node == node)
			.copied()
			.collect();
		if freshness == NEXT_HOP {
			assert_eq!(trailing, None);
			assert_eq!(false_ends(&events), 1);
			assert_eq!(reannouncements(&events), 1);
		} else {
			assert_eq!(trailing, Some(true));
			assert!(events.is_empty(), "{events:?}");
		}
	}
}

/// The partitioned local mesh from the routing contract, with a client behind
/// the starved relay. The CDN detour is infeasible until the origin advances
/// its seqno, and through that window the relay and its client keep the claim
/// as unavailable rather than ending it. Next-hop authority ends it. With seqno
/// requests the relay recovers through the CDN by itself.
#[test]
fn a_starved_relay_keeps_presence_unavailable_until_its_request_recovers_it() {
	let (c1, c2, l1, l2, origin, client) = (0, 1, 2, 3, 4, 5);
	let build = |freshness, requests| {
		let mut net = Net::new(config(freshness, requests), 6);
		net.join(c1, c2, 10);
		net.join(l1, l2, 1);
		net.link(l1, c1, Link::cost(1).upstream(), Link::cost(1));
		net.link(l2, c2, Link::cost(1).upstream(), Link::cost(1));
		net.leaf(origin);
		net.leaf(client);
		net.join(origin, l2, 1);
		net.join(client, l1, 1);
		net.originate(origin, 7, 0);
		net.announce(origin, "local/x", 0, None);
		assert!(net.run());
		assert_eq!(net.shows(client, 7, "local/x"), Some(true));
		net
	};

	let mut net = build(NEXT_HOP, Requests::Off);
	net.disconnect(l1, l2);
	assert!(net.run());
	assert!(net.starved(l1, 7));
	assert_eq!(false_ends(&net.events), 2, "the relay and its client");

	let mut net = build(FLOORS, Requests::Off);
	net.disconnect(l1, l2);
	assert!(net.run());
	assert!(net.starved(l1, 7));
	assert!(net.grounded(client, 7));
	for node in [l1, client] {
		assert_eq!(net.shows(node, 7, "local/x"), Some(false), "node {node}");
	}
	assert_eq!(false_ends(&net.events), 0);

	let mut net = build(FLOORS, Requests::HopByHop);
	let before = net.events.len();
	net.disconnect(l1, l2);
	assert!(net.run());
	assert_eq!(net.seqno(origin), 1, "the request reached the origin");
	assert_eq!(net.route(l1, 7).and_then(|route| route.next), Some(c1));
	let events: Vec<Event> = net.events[before..]
		.iter()
		.filter(|e| e.node == client)
		.copied()
		.collect();
	assert_eq!(kinds(&events), [Kind::Unavailable, Kind::Available]);
}

/// A client of two relays, one lagging behind its origin.
fn lagging(freshness: Freshness, watch: &'static str) -> Net {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = Net::new(config(freshness, Requests::Off), 4);
	net.leaf(origin);
	net.leaf(client);
	net.join(origin, r1, 1);
	net.join(origin, r2, 1);
	net.join(client, r1, 1);
	net.join(client, r2, 1);
	net.watch(client, watch);
	net.originate(origin, 7, 0);
	net
}

/// A client watching `live/a/` knows `live/a/x` ended at revision 3, then widens
/// to everything. The lagging relay's wider snapshot, at revision 2, must not
/// revive `live/a/x`, yet is the first to report `live/b/y`. Floors scoped to
/// the region they cover do both. One floor per origin either refuses the wide
/// snapshot, leaving `live/b/y` missing until the origin changes something, or
/// would accept it and revive `live/a/x`.
#[test]
fn widening_interest_needs_floors_scoped_to_what_they_cover() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	for scoped in [false, true] {
		let mut net = lagging(Freshness::Floors { scoped }, "live/a/");
		net.announce(origin, "live/a/x", 0, None);
		net.announce(origin, "live/b/y", 0, None);
		assert!(net.run());
		net.hold(origin, r2);
		net.unannounce(origin, "live/a/x");
		assert!(net.run());
		assert!(net.shown(client).is_empty());

		net.hold(r1, client);
		net.watch(client, "");
		assert!(net.run());
		net.release(r1, client);
		assert!(net.run());
		assert_eq!(net.shows(client, 7, "live/a/x"), None);
		assert_eq!(net.shows(client, 7, "live/b/y").is_some(), scoped, "scoped {scoped}");
		assert_eq!(revivals(&net.events), 0);
		if scoped {
			net.check().unwrap();
		}
	}
}

/// A client narrows its interest and later widens it again. Narrowing retires
/// what it knew outside the new interest, so its state follows the current
/// interest rather than every prefix it ever watched. On rejoin a lagging
/// relay's snapshot replays a withdrawal the client saw before it narrowed,
/// as after a collection, and only until the newer snapshot behind it lands.
#[test]
fn cancelling_interest_retires_its_region() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = lagging(FLOORS, "");
	net.announce(origin, "live/a/x", 0, None);
	assert!(net.run());
	net.hold(origin, r2);
	net.unannounce(origin, "live/a/x");
	assert!(net.run());
	assert_eq!(net.shows(client, 7, "live/a/x"), None);

	net.watch(client, "live/b/");
	assert!(net.run());
	assert_eq!(net.floors(client, 7), 1, "only the watched region");
	net.hold(r1, client);
	net.watch(client, "");
	assert!(net.run());
	assert_eq!(
		net.shows(client, 7, "live/a/x"),
		Some(true),
		"the lagging relay replays"
	);
	net.release(r1, client);
	assert!(net.run());
	assert_eq!(net.shows(client, 7, "live/a/x"), None);
	net.release(origin, r2);
	assert!(net.run());
	let life: Vec<Kind> = at(&net, client)
		.iter()
		.filter(|event| event.claim == "live/a/x")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::End, Kind::Start, Kind::End]);
	assert_eq!(revivals(&net.events), 0, "a retired region, like a collection");
	assert_eq!(net.floors(client, 7), 1);
	net.check().unwrap();
}

/// A client narrows its interest, and a claim outside it ends unseen. On
/// widening, what it kept there is frozen at a floor it no longer refreshed,
/// so it shows nothing in the widened region until a floor covering it
/// arrives, rather than a stale Start the next snapshot ends.
#[test]
fn widening_hides_what_was_frozen_until_a_covering_floor_arrives() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = lagging(FLOORS, "");
	net.announce(origin, "live/a/x", 0, None);
	net.announce(origin, "live/b/y", 0, None);
	assert!(net.run());
	net.watch(client, "live/b/");
	assert!(net.run());
	net.unannounce(origin, "live/a/x");
	assert!(net.run());

	net.hold(r1, client);
	net.hold(r2, client);
	net.watch(client, "");
	assert_eq!(net.shows(client, 7, "live/a/x"), None, "frozen while unwatched");
	assert_eq!(net.shows(client, 7, "live/b/y"), Some(true), "still current");

	net.release(r1, client);
	net.release(r2, client);
	assert!(net.run());
	let life: Vec<Kind> = at(&net, client)
		.iter()
		.filter(|event| event.claim == "live/a/x")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::Unwatched]);
	net.check().unwrap();
}

/// A snapshot sent before the client narrowed still waits on one session when
/// it widens again. Its floor covers the widened region but answers the old
/// interest, so it must not lift the guard on what stayed frozen.
#[test]
fn a_snapshot_queued_before_widening_keeps_the_frozen_rest_hidden() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = lagging(FLOORS, "");
	net.announce(origin, "live/a/x", 0, None);
	net.announce(origin, "live/b/y", 0, None);
	assert!(net.run());
	net.hold(r2, client);
	net.announce(origin, "live/b/z", 0, None);
	assert!(net.run());
	assert!(net.queued(r2, client) > 0, "a root snapshot waits");
	net.watch(client, "live/b/");
	assert!(net.run());
	net.unannounce(origin, "live/a/x");
	assert!(net.run());

	net.hold(r1, client);
	net.watch(client, "");
	assert!(net.deliver(r2, client), "the root snapshot from before narrowing");
	assert_eq!(net.shows(client, 7, "live/a/x"), None, "it answers the old interest");

	net.release(r1, client);
	net.release(r2, client);
	assert!(net.run());
	let life: Vec<Kind> = at(&net, client)
		.iter()
		.filter(|event| event.claim == "live/a/x")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::Unwatched]);
	net.check().unwrap();
}

/// A client behind a scoped grant widens its interest. Every batch it gets is
/// cut to the grant's scope, so no floor ever covers the widened interest;
/// the batch answering it is still what shows the newly covered claims. The
/// certified region is kept once however many batches repeat it, and the
/// freshness oracle checks the qualified view against the origin's history.
#[test]
fn widening_behind_a_scoped_grant_shows_the_rest_of_the_scope() {
	let (origin, relay, client) = (0, 1, 2);
	let mut net = Net::new(config(FLOORS, Requests::Off), 3);
	net.leaf(origin);
	net.leaf(client);
	net.join(origin, relay, 1);
	let grant = Grant {
		subject: "local",
		scope: "live/a/",
	};
	net.link(client, relay, Link::cost(1).grant(grant), Link::cost(1));
	net.watch(client, "live/a/x/");
	net.originate(origin, 7, 0);
	net.announce(origin, "live/a/x/1", 0, None);
	net.announce(origin, "live/a/y", 0, None);
	assert!(net.run());
	let id = qualified("local", 7);
	assert_eq!(net.shows(client, id, "live/a/x/1"), Some(true));
	assert_eq!(net.shows(client, id, "live/a/y"), None);

	net.watch(client, "");
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/a/y"), Some(true));
	let certified = net.retained().certified;
	for _ in 0..8 {
		net.announce(origin, "live/a/z", 0, None);
		net.unannounce(origin, "live/a/z");
		assert!(net.run());
	}
	assert_eq!(net.retained().certified, certified, "claim churn adds no state");
	assert!(
		net.check().unwrap().contains(&(client, id)),
		"the qualified view is checked"
	);
}

/// Two scoped grants qualify one origin under the same subject, so their
/// batches merge into one view. After widening, a batch through the first
/// grant certifies only its own scope: a claim in the second scope that ended
/// while unwatched stays hidden until the second grant answers too.
#[test]
fn widening_behind_two_grants_waits_for_each_scope() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = Net::new(config(FLOORS, Requests::Off), 4);
	net.leaf(origin);
	net.leaf(client);
	net.join(origin, r1, 1);
	net.join(origin, r2, 1);
	for (relay, scope) in [(r1, "live/a/"), (r2, "live/b/")] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		net.link(client, relay, Link::cost(1).grant(grant), Link::cost(1));
	}
	net.originate(origin, 7, 0);
	net.announce(origin, "live/a/x/1", 0, None);
	net.announce(origin, "live/b/z", 0, None);
	assert!(net.run());
	let id = qualified("local", 7);
	assert_eq!(net.shows(client, id, "live/b/z"), Some(true));

	net.hold(r1, client);
	net.hold(r2, client);
	net.watch(client, "live/a/x/");
	net.unannounce(origin, "live/b/z");
	assert!(net.run());
	net.watch(client, "");
	while net.deliver(r1, client) {}
	assert_eq!(net.shows(client, id, "live/a/x/1"), Some(true));
	assert_eq!(net.shows(client, id, "live/b/z"), None, "only the first scope answered");

	net.release(r1, client);
	net.release(r2, client);
	assert!(net.run());
	let life: Vec<Kind> = at(&net, client)
		.iter()
		.filter(|event| event.claim == "live/b/z")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::Unwatched]);
	net.check().unwrap();
}

/// One key arrives through two grants under one subject. When the session
/// for one scope closes, nothing certifies that scope any more, so its claims
/// go offline rather than staying shown through the other grant, which could
/// never deliver their withdrawal. The other scope stays available.
#[test]
fn a_closed_grant_lets_its_scope_go() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = Net::new(config(FLOORS, Requests::Off), 4);
	net.leaf(origin);
	net.leaf(client);
	net.join(origin, r1, 1);
	net.join(origin, r2, 1);
	for (relay, scope) in [(r1, "live/a/"), (r2, "live/b/")] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		net.link(client, relay, Link::cost(1).grant(grant), Link::cost(1));
	}
	net.originate(origin, 7, 0);
	net.announce(origin, "live/a/x", 0, None);
	net.announce(origin, "live/b/z", 0, None);
	assert!(net.run());
	let id = qualified("local", 7);
	assert_eq!(net.shows(client, id, "live/a/x"), Some(true));

	net.disconnect(client, r1);
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/a/x"), None);
	assert_eq!(net.shows(client, id, "live/b/z"), Some(true));
	net.unannounce(origin, "live/a/x");
	net.announce(origin, "live/a/y", 0, None);
	assert!(net.run());
	net.connect(client, r1);
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/a/x"), None);
	assert_eq!(net.shows(client, id, "live/a/y"), Some(true));
	let life: Vec<Kind> = at(&net, client)
		.iter()
		.filter(|event| event.claim == "live/a/x")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::Offline]);
	assert_eq!(false_ends(&net.events), 0);
	net.check().unwrap();
}

/// A relay forwards a key whose floors are all grant scopes, and its peer
/// moves to an interest no scope meets. The relay still answers the new
/// stream, certifying nothing, so the peer lets go of a wildcard it kept from
/// the scoped view instead of waiting on an answer that never comes.
#[test]
fn a_disjoint_interest_is_answered_with_nothing() {
	let (origin, local, x, y, z) = (0, 1, 2, 3, 4);
	let mut net = Net::new(config(FLOORS, Requests::Off), 5);
	net.leaf(origin);
	net.join(origin, local, 1);
	for (relay, scope) in [(x, "live/a/"), (y, "live/b/")] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		net.link(relay, local, Link::cost(1).grant(grant), Link::cost(1));
		net.join(relay, z, 1);
	}
	net.originate(origin, 7, 0);
	net.announce(origin, "live/", 0, None);
	assert!(net.run());
	let id = qualified("local", 7);
	assert_eq!(net.shows(z, id, "live/"), Some(true));

	net.watch(z, "live/c");
	assert!(net.run());
	assert_eq!(net.shows(z, id, "live/"), None);
	let life: Vec<Kind> = at(&net, z)
		.iter()
		.filter(|event| event.id == id && event.claim == "live/")
		.map(|event| event.kind)
		.collect();
	assert_eq!(life, [Kind::Start, Kind::Offline]);
	net.check().unwrap();
}

/// An interest matches whole path segments: watching `live/a` sees what lies
/// below it, never a sibling `live/ab` that merely shares its bytes.
#[test]
fn an_interest_matches_whole_segments() {
	let (origin, client) = (0, 1);
	let mut net = Net::new(config(FLOORS, Requests::Off), 2);
	net.leaf(origin);
	net.join(origin, client, 1);
	net.originate(origin, 7, 0);
	net.announce(origin, "live/a/x", 0, None);
	net.announce(origin, "live/ab", 0, None);
	net.watch(client, "live/a");
	assert!(net.run());
	assert_eq!(net.shows(client, 7, "live/a/x"), Some(true));
	assert_eq!(net.shows(client, 7, "live/ab"), None);
	net.check().unwrap();
}

/// One key arrives through a wide grant and a narrow one under one subject.
/// The narrow session lags behind a withdrawal the wide one delivered; when
/// the wide session closes, only what lies outside the narrow scope goes
/// offline, and the newer knowledge inside it stays rather than reviving the
/// withdrawn claim from the lagging session.
#[test]
fn a_lost_scope_keeps_newer_knowledge_where_still_certified() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut net = Net::new(config(FLOORS, Requests::Off), 4);
	net.leaf(origin);
	net.leaf(client);
	net.join(origin, r1, 1);
	net.join(origin, r2, 1);
	for (relay, scope) in [(r1, "live/"), (r2, "live/b/")] {
		let grant = Grant {
			subject: "local",
			scope,
		};
		net.link(client, relay, Link::cost(1).grant(grant), Link::cost(1));
	}
	net.originate(origin, 7, 0);
	net.announce(origin, "live/a/x", 0, None);
	net.announce(origin, "live/b/z", 0, None);
	assert!(net.run());
	let id = qualified("local", 7);
	assert_eq!(net.shows(client, id, "live/b/z"), Some(true));

	net.hold(r2, client);
	net.unannounce(origin, "live/b/z");
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/b/z"), None);
	net.disconnect(client, r1);
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/a/x"), None, "outside the scope left");
	assert_eq!(
		net.shows(client, id, "live/b/z"),
		None,
		"the lagging scope revives nothing"
	);
	net.check().unwrap();

	net.release(r2, client);
	assert!(net.run());
	assert_eq!(net.shows(client, id, "live/b/z"), None);
	assert_eq!(revivals(&at(&net, client)), 0);
	net.check().unwrap();
}

/// Two relays each admit an uplink scoped to a different prefix under one
/// subject and forward the key to a third, whose view then carries one floor
/// per scope. When one uplink closes, that relay is starved and holds, so the
/// third shows that scope unavailable while the other stays available; once
/// a seqno request recovers a route, nothing certifies the lost scope and the
/// third lets it go. Certifiers are feasible neighbors only, so the starved
/// relay and the third never keep the scope alive through each other.
#[test]
fn a_relay_lets_go_of_a_scope_its_uplinks_lost() {
	for (requests, lost) in [(Requests::Off, Some(false)), (Requests::HopByHop, None)] {
		let (origin, local, x, y, z) = (0, 1, 2, 3, 4);
		let mut net = Net::new(config(FLOORS, requests), 5);
		net.leaf(origin);
		net.join(origin, local, 1);
		for (relay, scope) in [(x, "live/a/"), (y, "live/b/")] {
			let grant = Grant {
				subject: "local",
				scope,
			};
			net.link(relay, local, Link::cost(1).grant(grant), Link::cost(1));
			net.join(relay, z, 1);
		}
		net.originate(origin, 7, 0);
		net.announce(origin, "live/a/x", 0, None);
		net.announce(origin, "live/b/z", 0, None);
		assert!(net.run());
		let id = qualified("local", 7);
		assert_eq!(net.shows(z, id, "live/a/x"), Some(true));
		assert_eq!(net.shows(z, id, "live/b/z"), Some(true));

		net.disconnect(x, local);
		net.unannounce(origin, "live/a/x");
		assert!(net.run());
		assert_eq!(net.shows(z, id, "live/a/x"), lost, "{requests:?}");
		assert_eq!(net.shows(z, id, "live/b/z"), Some(true), "{requests:?}");
		assert_eq!(false_ends(&net.events), 0, "{requests:?}");
		net.check().unwrap();
	}
}

/// An exact claim covers only its own path, so it is outside an interest
/// below it, and only a wildcard ancestor is inside.
#[test]
fn only_a_wildcard_claim_overlaps_an_interest_below_it() {
	assert!(!overlaps("live/a", "live/a/x"));
	assert!(overlaps("live/a/", "live/a/x"));
	assert!(overlaps("live/a/x", "live/a/"));
	assert!(overlaps("live/a", "live/a"));

	let (origin, client) = (0, 3);
	let mut net = lagging(FLOORS, "live/a/x");
	net.announce(origin, "live/a", 0, None);
	net.announce(origin, "live/a/", 0, None);
	assert!(net.run());
	assert_eq!(net.shows(client, 7, "live/a"), None);
	assert_eq!(net.shows(client, 7, "live/a/"), Some(true));
	net.check().unwrap();
}

/// Two origins hold the same wildcard capability for one epoch, and a client
/// watches a region below it. Withdrawing one origin's wildcard ends only that
/// origin's claim. The client's floor for `live/a/` certifies the wildcard above
/// it, so a lagging relay's older copy of the withdrawn wildcard is refused
/// after the client's next hop moves there.
#[test]
fn wildcard_claims_end_per_origin_and_are_certified_below() {
	let (o1, o2, r1, r2, client) = (0, 1, 2, 3, 4);
	for freshness in [BARRIER, FLOORS] {
		let mut net = Net::new(config(freshness, Requests::Off), 5);
		for origin in [o1, o2] {
			net.leaf(origin);
			net.join(origin, r1, 1);
			net.join(origin, r2, 1);
		}
		net.leaf(client);
		net.join(client, r1, 1);
		net.join(client, r2, 1);
		net.watch(client, "live/a/");
		net.originate(o1, 10, 0);
		net.originate(o2, 11, 0);
		net.announce(o1, "live/", 0, Some(1));
		net.announce(o2, "live/", 0, Some(1));
		assert!(net.run());
		assert_eq!(net.shown(client).len(), 2);

		net.hold(o1, r2);
		net.unannounce(o1, "live/");
		assert!(net.run());
		assert_eq!(net.shows(client, 10, "live/"), None);
		assert_eq!(
			net.shows(client, 11, "live/"),
			Some(true),
			"the other origin's claim stays"
		);

		net.disconnect(client, r1);
		assert!(net.run());
		assert_eq!(net.shows(client, 10, "live/").is_some(), freshness == BARRIER);
		assert_eq!(net.shows(client, 11, "live/"), Some(true));
	}
}

/// An origin process restarts. With a fresh identity its claims start a new
/// revision history and the old identity's go offline once nothing can recover
/// them. A restart that kept its identity, even with a persisted seqno, would
/// restart its revision below the floors other nodes hold: they keep showing the
/// dead process's claims and refuse the new ones.
#[test]
fn origin_restart_takes_a_fresh_identity_for_its_claims() {
	let (origin, a, b) = (0, 1, 2);
	for fresh in [false, true] {
		let mut net = Net::new(Config::default(), 3);
		net.leaf(origin);
		net.join(origin, a, 1);
		net.join(a, b, 1);
		net.originate(origin, 7, 0);
		net.announce(origin, "live/x", 0, None);
		net.unannounce(origin, "live/x");
		net.announce(origin, "live/y", 0, None);
		assert!(net.run());

		net.kill(origin);
		assert!(net.run());
		for node in [a, b] {
			assert!(net.shown(node).is_empty());
			assert!(!net.holds(node, 7), "nothing can recover the dead identity");
		}
		let offline = net.events.iter().filter(|e| e.kind == Kind::Offline).count();
		assert_eq!(offline, 2, "offline, not ended by the origin");
		assert_eq!(false_ends(&net.events), 0);

		let id = if fresh { 8 } else { 7 };
		net.originate(origin, id, 10);
		net.reconnect(origin);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		for node in [a, b] {
			if fresh {
				assert_eq!(net.shows(node, 8, "live/x"), Some(true));
				assert_eq!(net.shown(node).len(), 1);
			} else {
				assert_eq!(net.shows(node, 7, "live/y"), Some(true), "the dead process's claim");
				assert_eq!(net.shows(node, 7, "live/x"), None);
			}
		}
	}
}

/// The route contract's collected incarnation, with announcements. Node b
/// learned that `live/x` ended, then lost every route and collected the origin.
/// A session still delivering older messages brings the claim back, but only
/// until the withdrawal queued behind it on that same session lands. Without
/// collection, b's retained floor refuses the replay outright.
#[test]
fn stale_replay_after_collection_lasts_only_until_the_withdrawal_behind_it() {
	let (origin, a, b, c) = (0, 1, 2, 3);
	for collect in [false, true] {
		let mut net = Net::new(Config::default(), 4);
		net.leaf(origin);
		net.join(origin, a, 1);
		net.join(a, b, 1);
		net.join(a, c, 1);
		net.join(c, b, 1);
		net.hold(a, b);
		net.originate(origin, 7, 0);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		net.unannounce(origin, "live/x");
		assert!(net.run());
		assert_eq!(kinds(&at(&net, b)), [Kind::Start, Kind::End]);

		net.kill(origin);
		assert!(net.run());
		assert!(!net.holds(b, 7));
		if collect {
			net.collect(b, 7);
		}
		// The held ROUTE, an empty snapshot, then the one carrying `live/x`.
		for _ in 0..3 {
			assert!(net.deliver(a, b));
		}
		assert_eq!(net.shows(b, 7, "live/x").is_some(), collect);
		assert!(net.deliver(a, b), "the withdrawal");
		assert_eq!(net.shows(b, 7, "live/x"), None);
		net.release(a, b);
		assert!(net.run());
		let replayed: &[Kind] = if collect {
			&[Kind::Start, Kind::End, Kind::Start, Kind::End]
		} else {
			&[Kind::Start, Kind::End]
		};
		assert_eq!(kinds(&at(&net, b)), replayed);
		assert!(!net.holds(b, 7));
	}
}

/// A relay whose own route went away holds its routes, and its downstream
/// nodes hold through it, only along feasible advertisements, so holds cannot
/// prop each other up. When the origin dies, every node in a ring goes
/// offline and keeps no route state, with no timer, and none of them reports
/// an END the origin did not send.
#[test]
fn holds_retire_once_nothing_can_recover() {
	let origin = 4;
	for requests in [Requests::Off, Requests::HopByHop] {
		let mut net = Net::new(config(FLOORS, requests), 5);
		for relay in 0..4 {
			net.join(relay, (relay + 1) % 4, 1);
		}
		net.leaf(origin);
		net.join(origin, 0, 1);
		net.originate(origin, 7, 0);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());

		net.kill(origin);
		assert!(net.run());
		for relay in 0..4 {
			assert!(net.shown(relay).is_empty());
			assert!(!net.holds(relay, 7), "relay {relay} under {requests:?}");
		}
		assert_eq!(false_ends(&net.events), 0);
		assert_eq!(net.retained().pending, 0);
	}
}

/// Seqnos and revisions are varints. An origin never advertises the last
/// value: a bump, a claim change, or a request that would reach it rotates the
/// identity instead, so every node lets the old one go and sees the claims
/// again under the new one. Nothing wraps. A session proves one key once, so
/// rotating closes and reopens every session the origin had up; proving a
/// second key on a live session fails.
#[test]
fn exhausted_counters_rotate_the_identity() {
	let (origin, a, b, c, client) = (0, 1, 2, 3, 4);
	let rotated = |net: &Net| {
		let fresh = net.proven(origin);
		assert_ne!(fresh, 7, "a fresh identity");
		assert_eq!(net.shows(client, 7, "live/x"), None);
		assert_eq!(net.shows(client, fresh, "live/x"), Some(true));
		assert!(net.route(a, 7).is_none());
		net.check().unwrap();
	};
	let build = |seqno| {
		let mut net = Net::new(config(FLOORS, Requests::HopByHop), 5);
		net.leaf(origin);
		net.leaf(client);
		net.join(origin, a, 1);
		net.join(a, b, 1);
		net.join(b, c, 1);
		net.join(origin, c, 1);
		net.join(client, a, 1);
		net.disconnect(origin, c);
		net.originate(origin, 7, seqno);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		net
	};

	let mut net = build(VARINT_MAX - 2);
	net.bump(origin);
	assert!(net.run());
	assert_eq!(net.proven(origin), 7);
	net.bump(origin);
	assert!(net.run());
	rotated(&net);

	let mut net = build(0);
	net.age(origin, VARINT_MAX - 2);
	net.announce(origin, "live/y", 0, None);
	assert!(net.run());
	assert_eq!(net.proven(origin), 7);
	net.unannounce(origin, "live/y");
	assert!(net.run());
	rotated(&net);

	// The origin moves to a farther attachment, and the starved node asks for
	// the last seqno.
	let mut net = build(VARINT_MAX - 1);
	net.disconnect(origin, a);
	net.connect(origin, c);
	assert!(net.run());
	rotated(&net);
	assert!(net.route(a, net.proven(origin)).is_some());
}

/// The seqno request sketch: a starved node resends its request a few times
/// and then gives up until an infeasible update arrives. Node b forwards along
/// its route through x, whose own route just failed but whose retraction to b
/// is still in flight, so x drops every attempt. When the retraction lands, b
/// moves to an equal-metric route through y and advertises nothing new, so n
/// stays starved behind a quiet origin. Keeping the request at every hop that
/// forwarded it, and resending it when that hop's next hop changes, reaches the
/// origin with no timer.
#[test]
fn a_quiet_origin_stays_starved_after_bounded_retries() {
	let (origin, w, x, y2, y, b, n, z, client) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
	for requests in [Requests::Retries(2), Requests::HopByHop] {
		let mut net = Net::new(config(FLOORS, requests), 9);
		net.leaf(origin);
		net.leaf(client);
		net.join(origin, w, 1);
		net.join(w, x, 1);
		net.join(origin, y2, 1);
		net.join(y2, y, 1);
		net.link(b, x, Link::cost(1).upstream(), Link::cost(1));
		net.link(b, y, Link::cost(1).upstream(), Link::cost(1));
		net.join(b, n, 1);
		net.join(n, z, 1);
		net.join(z, origin, 1);
		net.join(n, client, 1);
		net.originate(origin, 7, 0);
		net.announce(origin, "live/x", 0, None);
		assert!(net.run());
		assert_eq!(net.route(n, 7).and_then(|route| route.next), Some(z));
		assert_eq!(net.route(b, 7).and_then(|route| route.next), Some(x));

		net.disconnect(n, z);
		net.hold(x, b);
		net.disconnect(w, x);
		assert!(net.run());
		assert!(net.starved(n, 7));
		for _ in 0..3 {
			net.tick();
			assert!(net.run());
		}
		net.release(x, b);
		assert!(net.run());
		assert_eq!(net.route(b, 7).and_then(|route| route.next), Some(y));
		assert_eq!(net.shows(client, 7, "live/x"), Some(requests == Requests::HopByHop));
		if requests == Requests::HopByHop {
			assert_eq!(net.seqno(origin), 1);
			assert!(net.route(n, 7).is_some());
		} else {
			assert_eq!(net.seqno(origin), 0);
			assert!(net.starved(n, 7), "nothing will ever ask the origin again");
		}
	}
}

/// Random meshes churn sessions and hold and deliver messages out of step, then
/// release every hold without reconnecting anything. Every relay that can
/// still reach a quiet origin must end with a route to it, which only a seqno
/// request can bring. A forwarder that skipped its requesters stranded some:
/// its only next hop was a requester that had long stopped waiting, and no
/// message ever cancels a request.
#[test]
fn sweep_requests_reach_a_quiet_origin() {
	const RELAYS: usize = 5;
	let mut stranded = 0;
	for seed in 0..1000u64 {
		let mut rng = XorShift(0x9e37_79b9 ^ (seed * 0x1234_5678_9abc));
		let nodes = RELAYS + 2;
		let mut net = Net::new(config(FLOORS, Requests::HopByHop), nodes);
		let mut links = Vec::new();
		for a in 0..RELAYS {
			for b in a + 1..RELAYS {
				if b == a + 1 || rng.chance(40) {
					net.join(a, b, 1 + rng.next(3));
					links.push((a, b));
				}
			}
		}
		for origin in RELAYS..nodes {
			net.leaf(origin);
			for _ in 0..2 {
				let relay = rng.next(RELAYS as u64) as usize;
				if !links.contains(&(relay, origin)) {
					net.join(origin, relay, 1 + rng.next(3));
					links.push((relay, origin));
				}
			}
			net.originate(origin, origin as u64, 0);
		}
		assert!(net.run());
		for _ in 0..30 {
			match rng.next(6) {
				0 | 1 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					if net.connected(a, b) {
						net.disconnect(a, b);
					} else {
						net.connect(a, b);
					}
				}
				2 | 3 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					if rng.chance(60) {
						net.hold(from, to);
					} else {
						net.release(from, to);
					}
				}
				_ => {
					let busy = net.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						net.deliver(from, to);
					}
				}
			}
			net.settle(1);
		}
		for &(a, b) in &links {
			net.release(a, b);
			net.release(b, a);
		}
		assert!(net.run());
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
					if next < RELAYS && net.connected(a, b) && reach.insert(next) {
						frontier.push(next);
					}
				}
			}
			stranded += reach
				.iter()
				.filter(|&&node| node < RELAYS && net.route(node, origin as Id).is_none())
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
	let build = |relays: usize, claims: usize| {
		let mut net = Net::new(Config::default(), relays + 2);
		for relay in 0..relays {
			net.join(relay, (relay + 1) % relays, 1);
		}
		let origin = relays;
		net.leaf(origin);
		net.join(origin, 0, 1);
		// A client watching a region with no claims yet.
		let client = relays + 1;
		net.leaf(client);
		net.join(client, relays / 2, 1);
		net.watch(client, "none/");
		net.originate(origin, 7, 0);
		for prefix in &PREFIXES[..claims] {
			net.announce(origin, prefix, 0, None);
		}
		assert!(net.run());
		net
	};
	for (relays, claims) in [(4, 2), (8, 2), (4, 4)] {
		let mut net = build(relays, claims);
		let origin = relays;
		// Each relay sends to both ring neighbors, and the origin and its relay to
		// each other: without split horizon the relay echoes the origin's own claims.
		let sessions = 2 * relays + 2;
		let retained = net.retained();
		assert_eq!(retained.items, relays * claims);
		assert_eq!(retained.floors, relays + 1);
		assert_eq!(retained.heard, sessions * claims);
		// Both ends of every session cache the same view.
		assert_eq!(
			(retained.sent, retained.sent_floors),
			(retained.heard, retained.heard_floors)
		);

		let (stats, events) = (net.stats, net.events.len());
		net.unannounce(origin, PREFIXES[0]);
		assert!(net.run());
		// The client's session carries only the advanced floor.
		assert_eq!(net.stats.claim_msgs - stats.claim_msgs, sessions + 1);
		assert_eq!(net.stats.claim_items - stats.claim_items, sessions);
		assert_eq!(net.events.len() - events, relays + 1, "once per node");

		for _ in 0..50 {
			net.announce(origin, PREFIXES[0], 0, None);
			net.unannounce(origin, PREFIXES[0]);
		}
		assert!(net.run());
		let after = net.retained();
		assert_eq!(after.items, relays * (claims - 1));
		assert_eq!(after.floors, retained.floors, "no tombstones");
		assert_eq!(after.echoes, 0, "every ROUTE_END echo drained");
		net.check().unwrap();

		// The client cycles through disjoint regions, one with a live claim.
		let client = relays + 1;
		for cycles in [1, 16] {
			for room in 0..cycles {
				let region: &'static str = Box::leak(format!("room/{room}/").into_boxed_str());
				net.watch(client, region);
				assert!(net.run());
				net.watch(client, PREFIXES[1]);
				assert!(net.run());
				assert_eq!(net.shows(client, 7, PREFIXES[1]), Some(true));
			}
			net.watch(client, "none/");
			assert!(net.run());
			assert_eq!(net.retained(), after, "interest history after {cycles} cycles");
		}
		net.check().unwrap();
	}
}

/// A client reaches one origin through two relays, each session scoped by a
/// grant under the same subject, so both feed one qualified view. Claims
/// change in and around both scopes, the client's interest moves, sessions
/// drop and return, and messages are held and delivered out of step. Every
/// view stays some revision of the origin's truth after every step, the
/// widening guard stays bounded, no claim ends while the origin still has it,
/// and once churn stops the client shows exactly the claims its interest and
/// the scopes admit.
#[test]
fn sweep_scoped_handoff() {
	const CLAIMS: [&str; 5] = ["live/a/x", "live/a/y/", "live/b/z", "live/c", "live/"];
	const WATCHES: [&str; 5] = ["", "live/", "live/a/", "live/b/z", "live/c"];
	const SCOPES: [&str; 2] = ["live/a/", "live/b/"];
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut rng = XorShift(0x5c0b_ed00);
	let mut live_starts = 0;
	for scenario in 0..150 {
		let mut net = Net::new(config(FLOORS, Requests::HopByHop), 4);
		net.leaf(origin);
		net.leaf(client);
		net.join(origin, r1, 1);
		net.join(origin, r2, 1);
		net.join(r1, r2, 1);
		for (relay, scope) in [(r1, SCOPES[0]), (r2, SCOPES[1])] {
			let grant = Grant {
				subject: "local",
				scope,
			};
			net.link(client, relay, Link::cost(1).grant(grant), Link::cost(1));
		}
		let links = [(origin, r1), (origin, r2), (r1, r2), (client, r1), (client, r2)];
		net.originate(origin, 7, 0);
		assert!(net.run());
		for _ in 0..40 {
			match rng.next(8) {
				0..=2 => {
					let claim = CLAIMS[rng.next(CLAIMS.len() as u64) as usize];
					if rng.chance(60) {
						net.announce(origin, claim, rng.next(3), None);
					} else {
						net.unannounce(origin, claim);
					}
				}
				3 => net.watch(client, WATCHES[rng.next(WATCHES.len() as u64) as usize]),
				4 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					if net.connected(a, b) {
						net.disconnect(a, b);
					} else {
						net.connect(a, b);
					}
				}
				5 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					if rng.chance(60) {
						net.hold(from, to);
					} else {
						net.release(from, to);
					}
				}
				_ => {
					let busy = net.busy();
					if !busy.is_empty() {
						let (from, to) = busy[rng.next(busy.len() as u64) as usize];
						net.deliver(from, to);
					}
				}
			}
			net.settle(1);
			net.check().unwrap_or_else(|err| panic!("scenario {scenario}: {err}"));
			assert!(
				net.retained().certified <= SCOPES.len() + 1,
				"scenario {scenario}: the widening guard grows"
			);
		}
		for (a, b) in links {
			net.release(a, b);
			net.release(b, a);
			if !net.connected(a, b) {
				net.connect(a, b);
			}
		}
		assert!(net.run());
		net.check().unwrap_or_else(|err| panic!("scenario {scenario}: {err}"));
		let watch = net.watching(client);
		let rev = net.revision(7);
		let expected: BTreeSet<&str> = CLAIMS
			.into_iter()
			.filter(|claim| net.truth(7, claim, rev).is_some())
			.filter(|claim| {
				SCOPES.iter().any(|scope| {
					let region = match () {
						_ if within(scope, watch) => *scope,
						_ if within(watch, scope) => watch,
						_ => return false,
					};
					overlaps(claim, region)
				})
			})
			.collect();
		let id = qualified("local", 7);
		let shown: BTreeSet<&str> = net
			.shown(client)
			.iter()
			.filter(|((shown, _), view)| *shown == id && view.available)
			.map(|((_, claim), _)| *claim)
			.collect();
		assert_eq!(shown, expected, "scenario {scenario} watching {watch:?}");
		// A scope its last session left, or an interest no scope meets, goes
		// offline rather than ending.
		assert_eq!(false_ends(&net.events), 0, "scenario {scenario}");
		live_starts += net
			.events
			.iter()
			.filter(|event| event.id == id && event.kind == Kind::Start && !event.withdrawn)
			.count();
	}
	// Qualified events are judged against the origin's history, so a live
	// claim's Start is not mistaken for a withdrawn one.
	assert!(live_starts > 0);
}

/// Random meshes with two origins and a client under random churn: claims
/// change, sessions drop and return, messages are held and replayed out of
/// step, the client changes interest, and an origin restarts with a fresh
/// identity. Under every rule, selected routes never loop, even right after a
/// drained feasibility distance is dropped. Under merged floors, every view is always some revision of the
/// origin's truth, nothing revives and nothing ends falsely, and once churn
/// stops every node shows every live origin's current claims and holds
/// nothing for a dead one. A node that collected an origin with no route left,
/// or restarted, may see a stale view return until the withdrawal behind it
/// lands; nothing else revives. Next-hop authority and a snapshot barrier both
/// revive withdrawals and report false ENDs on the same scenarios.
#[test]
fn sweep_handoff() {
	let mut totals = Vec::new();
	for freshness in [NEXT_HOP, BARRIER, FLOORS] {
		let mut rng = XorShift(0x4a4d_0ff5);
		let mut total = Totals::default();
		for scenario in 0..150 {
			let mut net = churn(&mut rng, freshness);
			let label = format!("scenario {scenario} under {freshness:?}");
			total.events += net.events.len();
			total.revivals += revivals(&net.events);
			total.false_ends += false_ends(&net.events);
			total.reannouncements += reannouncements(&net.events);
			total.claim_bytes += net.stats.claim_bytes;
			total.route_msgs += net.stats.route_msgs;
			total.requests += net.stats.requests;
			if freshness != FLOORS {
				continue;
			}
			assert_eq!(revivals(&net.events), 0, "{label}");
			assert_eq!(false_ends(&net.events), 0, "{label}");
			settled(&mut net, &label);
		}
		totals.push((freshness, total));
	}
	for (freshness, total) in &totals {
		eprintln!("{freshness:?}: {total:?}");
	}
	let [(_, next_hop), (_, barrier), _] = &totals[..] else {
		unreachable!()
	};
	assert!(next_hop.revivals > 0 && next_hop.false_ends > 0);
	assert!(barrier.revivals > 0);
}

#[derive(Debug, Default)]
struct Totals {
	events: usize,
	revivals: usize,
	false_ends: usize,
	reannouncements: usize,
	claim_bytes: usize,
	route_msgs: usize,
	requests: usize,
}

const RELAYS: usize = 5;
const CLAIMS: [&str; 4] = ["live/", "live/a/", "live/a/x", "live/b"];

/// Origins sit at `RELAYS` and `RELAYS + 1`, the client at `RELAYS + 2`.
fn churn(rng: &mut XorShift, freshness: Freshness) -> Net {
	let (first, client) = (RELAYS, RELAYS + 2);
	let mut net = Net::new(config(freshness, Requests::HopByHop), RELAYS + 3);
	let mut links = Vec::new();
	for relay in 0..RELAYS {
		links.push((relay, (relay + 1) % RELAYS));
	}
	let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
	if a.abs_diff(b) > 1 && a.abs_diff(b) != RELAYS - 1 {
		links.push((a, b));
	}
	for leaf in [first, first + 1, client] {
		net.leaf(leaf);
		let a = rng.next(RELAYS as u64) as usize;
		links.push((leaf, a));
		let b = (a + 1 + rng.next(RELAYS as u64 - 1) as usize) % RELAYS;
		links.push((leaf, b));
	}
	for &(a, b) in &links {
		net.join(a, b, 1 + rng.next(3));
	}
	let mut ids = [100, 200];
	net.originate(first, ids[0], 0);
	net.originate(first + 1, ids[1], 0);
	assert!(net.run());

	for _ in 0..40 {
		match rng.next(10) {
			0..=3 => {
				let origin = rng.next(2) as usize;
				let claim = CLAIMS[rng.next(CLAIMS.len() as u64) as usize];
				if rng.chance(50) {
					net.announce(first + origin, claim, rng.next(3), None);
				} else {
					net.unannounce(first + origin, claim);
				}
			}
			4 => {
				let (a, b) = links[rng.next(links.len() as u64) as usize];
				if net.connected(a, b) {
					net.disconnect(a, b);
				} else {
					net.connect(a, b);
				}
			}
			5 => {
				let (a, b) = links[rng.next(links.len() as u64) as usize];
				let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
				if rng.chance(70) {
					net.hold(from, to);
				} else {
					net.release(from, to);
				}
			}
			6 => {
				net.watch(client, ["", "live/a/", "live/b"][rng.next(3) as usize]);
			}
			7 if rng.chance(50) => {
				// Collection of routes and announcements is allowed once a node holds no route to an identity.
				let node = rng.next(RELAYS as u64 + 3) as usize;
				let id = ids[rng.next(2) as usize];
				if !net.holds(node, id) && (RELAYS..RELAYS + 2).all(|origin| node != origin) {
					net.collect(node, id);
				}
			}
			7 if rng.chance(40) => {
				let origin = rng.next(2) as usize;
				net.kill(first + origin);
				ids[origin] += 1;
				net.originate(first + origin, ids[origin], 0);
				net.reconnect(first + origin);
			}
			_ => {
				// Echoed ROUTE_ENDs ride the route's own stream, so they interleave
				// freely with the echoing node's routes.
				for _ in 0..rng.next(6) {
					let busy: Vec<(bool, usize, usize)> = (net.busy().into_iter().map(|(from, to)| (false, from, to)))
						.chain(net.busy_acks().into_iter().map(|(from, to)| (true, from, to)))
						.collect();
					if busy.is_empty() {
						break;
					}
					match busy[rng.next(busy.len() as u64) as usize] {
						(false, from, to) => net.deliver(from, to),
						(true, from, to) => net.deliver_ack(from, to),
					};
				}
			}
		}
		if freshness == FLOORS {
			net.check().unwrap();
		}
		loop_free(&net);
	}

	for &(a, b) in &links {
		net.release(a, b);
		net.release(b, a);
		if !net.connected(a, b) {
			net.connect(a, b);
		}
	}
	assert!(net.run());
	net
}

/// A node drops a feasibility distance only once every neighbor echoed its
/// ROUTE_END, so an older advertisement of its own never returns as a loop.
fn loop_free(net: &Net) {
	for id in (100..110).chain(200..210) {
		for node in 0..RELAYS + 3 {
			let mut at = node;
			for hop in 0.. {
				let Some(next) = net.route(at, id).and_then(|selected| selected.next) else {
					break;
				};
				assert!(hop <= RELAYS + 3, "{id} loops from node {node}");
				at = next;
			}
		}
	}
}

/// After churn stops with every session up, each node shows exactly the live
/// origins' current claims within its interest, available, and keeps nothing
/// for an identity whose process is gone.
fn settled(net: &mut Net, label: &str) {
	net.check().unwrap();
	let live: Vec<Id> = (RELAYS..RELAYS + 2).map(|origin| net.proven(origin)).collect();
	for node in 0..RELAYS + 3 {
		let mut expected = BTreeSet::new();
		for &id in &live {
			assert!(
				net.route(node, id).is_some(),
				"{label}: node {node} has no route to {id}"
			);
			for claim in CLAIMS {
				if overlaps(claim, net.watching(node)) && net.truth(id, claim, net.revision(id)).is_some() {
					expected.insert((id, claim));
				}
			}
		}
		let shown: BTreeSet<(Id, &str)> = net.shown(node).keys().copied().collect();
		assert_eq!(shown, expected, "{label}: node {node}");
		for (&(id, claim), shown) in net.shown(node) {
			assert!(shown.available, "{label}: node {node} {id} {claim}");
			assert_eq!(
				net.truth(id, claim, net.revision(id)),
				Some(shown.item),
				"{label}: node {node} {id} {claim}"
			);
		}
		for id in (100..110).chain(200..210).filter(|id| !live.contains(id)) {
			assert!(!net.holds(node, id), "{label}: node {node} holds dead {id}");
			assert!(
				!net.remembers(node, id),
				"{label}: node {node} keeps dead {id}'s distance"
			);
		}
	}
	assert_eq!(net.retained().pending, 0, "{label}");
}

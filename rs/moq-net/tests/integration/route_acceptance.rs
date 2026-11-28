#![cfg(feature = "async")]

//! Acceptance of lite-07's route rule on real sessions with time mocked: the topology
//! fixtures of the routes epic's acceptance quest
//! and the three from routing prior art (the epic's Plan item 30).
//!
//! The rule's own fixtures live in `path_routes.rs`, whose helpers these share.

use crate::path_routes::{DEBOUNCE, HOLD, Seen, cut, land, lite07, publish, quiesce, record, withdrawn};
use crate::route_change::{link, produce_origin};
use crate::support;

use std::time::Duration;

use futures::FutureExt;
use moq_net::{
	Version, origin,
	stats::{Message, Registry, Tier, Totals},
};
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer, peer_with_latency};

/// How a [`join`] is configured beyond [`peer`]'s defaults.
#[derive(Default)]
struct Join {
	/// What the link costs both ways, when not the default of 1.
	cost: Option<u64>,
	/// How each side ranks the routes it receives over the link: the first node's, then
	/// the second's.
	preference: [i8; 2],
	/// Where the second node's session counts its control messages.
	stats: moq_net::stats::Session,
}

/// [`peer`] `a` and `b`, configured by `join`.
async fn join(version: Version, a: &origin::Producer, b: &origin::Producer, join: Join) -> MockPair {
	let [a, b] = [(a, join.preference[0]), (b, join.preference[1])].map(|(node, preference)| {
		node.clone()
			.with_link(origin::Link::Peer)
			.with_priority(origin::Priority::new(preference))
	});
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(a.consume().with_hidden(true));
	options.client_subscribe = Some(a);
	options.server_publish = Some(b.consume().with_hidden(true));
	options.server_subscribe = Some(b);
	options.cost = join.cost;
	options.server_stats = join.stats;
	connect_mock(options).await
}

/// Whether a session has closed, without waiting for it to.
fn closed(pair: &MockPair) -> bool {
	pair.server.closed().now_or_never().is_some() || pair.client.closed().now_or_never().is_some()
}

/// The announce stream messages counted under `tier`, both ways.
fn announces(registry: &Registry, tier: &Tier) -> u64 {
	(registry.snapshot().control().into_iter())
		.filter(|(row, class, _)| row == tier && *class == Message::Announce)
		.map(
			|(
				_,
				_,
				Totals {
					tx_messages,
					rx_messages,
					..
				},
			)| tx_messages + rx_messages,
		)
		.sum()
}

/// What a link flap cost, in announce stream messages on the sessions beside the link.
#[derive(Debug, PartialEq, Eq)]
struct Flap {
	/// While the link is down: on `B`-`C`, then on `C`-`D`.
	down: [u64; 2],
	/// Once it reopens: on `B`-`C`, then on `C`-`D`.
	up: [u64; 2],
}

/// `O` publishes `broadcasts` broadcasts and peers with `A`, which peers with `B` and `C`,
/// which peer with each other. `D` reads from `C`. The link from `A` to `C` closes and
/// opens again. Returns what that cost beside the link, after checking that `D` never saw
/// a broadcast end, restart, or start again.
async fn flap(broadcasts: usize) -> Flap {
	let version = lite07();
	let [o, a, b, c, d] = [1, 2, 3, 4, 5].map(produce_origin);
	let _live: Vec<_> = (0..broadcasts).map(|i| publish(&o, &format!("live/{i}"))).collect();
	let registry = Registry::new(Default::default());
	let tiers = ["b-c", "c-d"].map(|label| Tier::new(moq_net::Path::from_static(label)));
	let counted = |tier: &Tier| {
		registry
			.tier(tier.clone())
			.session(moq_net::stats::SessionInfo::new(moq_net::Path::from_static("")))
	};
	let _o_a = peer(version, &o, &a).await;
	let _a_b = peer(version, &a, &b).await;
	let a_c = peer(version, &a, &c).await;
	let _b_c = join(
		version,
		&b,
		&c,
		Join {
			stats: counted(&tiers[0]),
			..Default::default()
		},
	)
	.await;
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(c.consume());
	options.client_subscribe = Some(d.clone());
	options.server_stats = counted(&tiers[1]);
	let _c_d = connect_mock(options).await;
	quiesce().await;

	let mut events = support::watch(d.consume().routes());
	land().await;
	let shown = support::drain(&mut events);
	assert_eq!(shown.len(), broadcasts, "D shows every broadcast");

	let count = || tiers.each_ref().map(|tier| announces(&registry, tier));
	let before = count();
	cut(&a_c);
	quiesce().await;
	let after = count();
	let _a_c = peer(version, &a, &c).await;
	quiesce().await;
	let reopened = count();

	let churn: Vec<_> = (support::drain(&mut events).into_iter())
		.filter(|(_, kinds)| kinds.iter().any(|kind| *kind != support::Kind::Update))
		.collect();
	assert!(churn.is_empty(), "D keeps every broadcast: {churn:?}");
	Flap {
		down: [0, 1].map(|i| after[i] - before[i]),
		up: [0, 1].map(|i| reopened[i] - after[i]),
	}
}

/// A link flap with many broadcasts behind one origin ends none of them, and going down
/// costs the routing session beside it two messages whatever the broadcasts: `C` loses its
/// link to `A`, names the closed session, and ends the route it offered `B`, whose own it
/// now follows. `D` never forwards, so it is told claims, each with what it costs to
/// reach: one update per broadcast each way. Coming back, `C` offers `B` its route again
/// and tells it the origin's claims with it, one message each and their floor.
#[kio::test]
async fn a_link_flap_ends_no_broadcast() {
	for broadcasts in [1, 64] {
		let n = broadcasts as u64;
		let cost = Flap {
			down: [2, n],
			up: [n + 2, n],
		};
		assert_eq!(flap(broadcasts).await, cost, "{broadcasts} broadcasts");
	}
}

/// `R1` publishes in a ring of three, where the link from `R2` to `R3` lags by [`STALE`].
/// The broadcast ends while `R3`'s route and claim are still on their way to `R2`, and
/// `R2`'s to `R3`: each arrives after the withdrawal it predates, and neither relay shows
/// the broadcast again. A claim's view is versioned by its publisher's revision, so an
/// older one never overwrites a newer withdrawal, whichever neighbour sends it.
#[kio::test]
async fn a_stale_neighbour_cannot_revive_an_ended_broadcast() {
	const STALE: Duration = Duration::from_secs(2);
	let version = lite07();
	let [r1, r2, r3] = [1, 2, 3].map(produce_origin);
	let _r1_r2 = peer(version, &r1, &r2).await;
	let _r1_r3 = peer(version, &r1, &r3).await;
	let _r2_r3 = peer_with_latency(version, &r2, &r3, STALE).await;
	let mut watched = Vec::new();
	for relay in [&r2, &r3] {
		watched.push(record(relay, "live").await);
	}

	let start = kio::clock::now();
	let live = publish(&r1, "live");
	kio::clock::sleep(STALE / 2).await;
	drop(live);
	quiesce().await;
	for (i, watch) in watched.iter_mut().enumerate() {
		let seen = watch.seen(start);
		let ended = seen.iter().position(withdrawn).expect("ended");
		assert_eq!(ended, seen.len() - 1, "R{}: nothing after the end: {seen:?}", i + 2);
		assert!(
			seen[ended].at < STALE,
			"R{}: before the stale route lands: {seen:?}",
			i + 2
		);
	}
}

/// A ring of five relays, `R0` to `R4`, each link of cost 1, with `P` publishing behind
/// `R0`. Returns `P`'s route and the ring's sessions, `Ri` to `Ri+1` at index `i`.
async fn ring() -> (
	origin::Producer,
	origin::Route,
	Vec<origin::Producer>,
	Vec<MockPair>,
	MockPair,
) {
	let version = lite07();
	let p = produce_origin(10);
	let live = publish(&p, "live");
	let relays: Vec<_> = (1..=5).map(produce_origin).collect();
	let p_r0 = link(version, &p, &relays[0]).await;
	let mut links = Vec::new();
	for i in 0..5 {
		links.push(peer(version, &relays[i], &relays[(i + 1) % 5]).await);
	}
	quiesce().await;
	(p, live, relays, links, p_r0)
}

/// [`ring`]: the link from `R1` to `R2` closes. `R2` preferred the path through `R1`, of
/// cost 3, and its backup runs the other way round the ring, of cost 4: it moves there at
/// once, the broadcast available throughout. Every other session stays up.
#[kio::test]
async fn a_ring_moves_at_once_to_a_backup_avoiding_the_far_end() {
	let (_p, _live, relays, links, _p_r0) = ring().await;
	let mut watch = record(&relays[2], "live").await;
	let start = kio::clock::now();
	cut(&links[1]);
	kio::clock::sleep(DEBOUNCE / 2).await;
	let seen = watch.seen(start);
	assert!(seen.iter().all(|seen| seen.available && !withdrawn(seen)), "{seen:?}");
	assert_eq!(seen.last().map(|seen| seen.cost), Some(4), "on the backup: {seen:?}");
	quiesce().await;
	assert_eq!(watch.seen(start), [], "and stays there");
	for (i, pair) in links.iter().enumerate().filter(|(i, _)| *i != 1) {
		assert!(!closed(pair), "R{i}-R{} stays up", (i + 1) % 5);
	}
}

/// [`ring`]: the link from `R0` to `R1` closes. Every backup runs the other way round the
/// ring, through `R0`, the closed link's far end, on sessions the DEAD does not name: `R2`
/// moves onto its backup, of cost 4, available throughout, and `R1` onto `R2`'s, of cost
/// 5, both at once. `R2` routed through `R1`, so it offered `R1` nothing until it moved:
/// `R1` shows the claim unavailable until `R2`'s route lands, here in the same instant.
/// Every session of the backup stays up.
#[kio::test]
async fn a_ring_moves_at_once_to_a_backup_through_the_far_end() {
	let (_p, _live, relays, links, _p_r0) = ring().await;
	let mut far = record(&relays[1], "live").await;
	let mut near = record(&relays[2], "live").await;
	let start = kio::clock::now();
	cut(&links[0]);
	quiesce().await;

	for (name, watch, cost, waits) in [("R2", &mut near, 4, false), ("R1", &mut far, 5, true)] {
		let seen = watch.seen(start);
		assert!(!seen.iter().any(withdrawn), "{name} stays shown: {seen:?}");
		assert!(
			seen.iter().all(|seen| seen.available || (waits && seen.at.is_zero())),
			"{name} is unavailable only while its backup is on its way: {seen:?}"
		);
		let moved = seen.last().expect("moved");
		assert!(moved.available, "{name} ends available: {seen:?}");
		assert!(moved.cost == cost, "{name} ends on its backup: {seen:?}");
		assert!(moved.at < DEBOUNCE, "{name} moves at once: {seen:?}");
	}
	for (i, pair) in links.iter().enumerate().skip(1) {
		assert!(!closed(pair), "R{i}-R{} stays up", (i + 1) % 5);
	}
}

/// `A` and `B` each reach `P` directly and peer with each other, over a link lagging by
/// [`LAG`]. `P` dies: each relay's only route left is the other's direct route, which the
/// other's own close made stale. Each moves onto it at once, as the route rule must while
/// nothing says it is stale, until the other's DEAD ends it one lag after the cut. Both
/// then show the claim unavailable and let go a debounce after that DEAD, once.
#[kio::test]
async fn two_relays_routing_through_each_other_both_let_go() {
	const LAG: Duration = Duration::from_millis(200);
	let version = lite07();
	let [p, a, b] = [1, 2, 3].map(produce_origin);
	let _live = publish(&p, "live");
	let dying = [peer(version, &p, &a).await, peer(version, &p, &b).await];
	let _a_b = peer_with_latency(version, &a, &b, LAG).await;
	quiesce().await;

	let mut watched = [record(&a, "live").await, record(&b, "live").await];
	let start = kio::clock::now();
	dying.iter().for_each(cut);
	quiesce().await;
	for (name, watch) in ["A", "B"].into_iter().zip(&mut watched) {
		let seen = watch.seen(start);
		assert!(
			seen.iter().all(|seen| !seen.available || seen.at.is_zero()),
			"{name}: on the stale route only until its DEAD lands: {seen:?}"
		);
		let unavailable = seen.iter().find(|seen| !seen.available).expect("unavailable");
		assert_eq!(unavailable.at, LAG, "{name}: unavailable once the DEAD lands: {seen:?}");
		assert_eq!(
			seen.iter().filter(|seen| withdrawn(seen)).count(),
			1,
			"{name}: {seen:?}"
		);
		let ended = seen.last().unwrap();
		assert!(withdrawn(ended), "{name}: {seen:?}");
		assert_eq!(ended.at, LAG + DEBOUNCE, "{name}: {seen:?}");
	}
}

/// `P` feeds `A`, which neighbours `B`. `P`'s link to `A` closes: no route reaches the claim,
/// so `A` lets it go a debounce later. Just after, `P` links to `B`, and `A` shows the claim
/// again the moment `B`'s route lands, at its higher cost. A lite-07 claim's own withdrawal is
/// final for the routes it had, so nothing holds a later one back the way a hop chain's
/// retraction quarantine would.
#[kio::test]
async fn a_claim_let_go_returns_at_once_on_a_worse_route() {
	let version = lite07();
	let [p, a, b] = [1, 2, 3].map(produce_origin);
	let _live = publish(&p, "live");
	let p_a = peer(version, &p, &a).await;
	let _a_b = peer(version, &a, &b).await;
	quiesce().await;

	let mut watch = record(&a, "live").await;
	let start = kio::clock::now();
	cut(&p_a);
	kio::clock::sleep(DEBOUNCE).await;
	land().await;
	let _p_b = peer(version, &p, &b).await;
	let linked = kio::clock::now().saturating_duration_since(start);
	land().await;
	let seen = watch.seen(start);
	let [lost, gone, back] = seen.as_slice() else {
		panic!("let go, then back: {seen:?}");
	};
	assert!(!lost.available && lost.at.is_zero(), "{seen:?}");
	assert!(withdrawn(gone) && gone.at == DEBOUNCE, "{seen:?}");
	assert!(
		back.kind == support::Kind::Start && back.available && back.cost == 2 && back.at == linked,
		"back at once, through B: {seen:?}"
	);
}

/// How far apart [`a_vanished_relay_is_noticed_a_keep_alive_apart`]'s neighbours notice.
const KEEP_ALIVE: Duration = Duration::from_secs(3);

/// Prior art (the epic's Plan item 30): `X` is `P`'s only way out and neighbours `R1`,
/// `R2` and `R3`, which mesh with each other. `X` vanishes: it sends nothing more, and its
/// neighbours notice one at a time, `R1` at once, `R2` half a [`KEEP_ALIVE`] later, `R3` a
/// whole one later, as keep-alives spaced that far apart would.
///
/// Measured: nothing keeps a relay off a route through `X` that a neighbour still offers,
/// so `R1` moves onto `R2`'s at once and onto `R3`'s when `R2` notices, as `R2` does. `R3` notices at 3 s,
/// and all three show the claim unavailable then and go offline a debounce later, once
/// each: the broadcast is never withdrawn early, but readers on `R1` and `R2` see it
/// available while nothing can serve it, and a request made then follows a route into
/// the dead relay. Everyone lets go only when the last neighbour notices.
///
/// So a dead relay reported one neighbour at a time stays routed for the spread of its
/// neighbours' detection: relay keep-alives bound it. This pins the hunt.
#[kio::test]
async fn a_vanished_relay_is_noticed_a_keep_alive_apart() {
	let version = lite07();
	let [p, x, r1, r2, r3] = [1, 2, 3, 4, 5].map(produce_origin);
	let _live = publish(&p, "live");
	let p_x = peer(version, &p, &x).await;
	let mut noticed = Vec::new();
	for r in [&r1, &r2, &r3] {
		noticed.push(peer(version, &x, r).await);
	}
	let _mesh = [
		peer(version, &r1, &r2).await,
		peer(version, &r2, &r3).await,
		peer(version, &r1, &r3).await,
	];
	quiesce().await;

	let mut watched = Vec::new();
	for r in [&r1, &r2, &r3] {
		watched.push(record(r, "live").await);
	}
	let start = kio::clock::now();
	// `X` is the client of each of its sessions but `P`'s: nothing it writes leaves it.
	p_x.server_transport.hold_writes();
	for pair in &noticed {
		pair.client_transport.hold_writes();
	}
	for (i, pair) in noticed.iter().enumerate() {
		kio::clock::sleep_until(start.saturating_add(KEEP_ALIVE * i as u32 / 2)).await;
		cut(pair);
	}
	quiesce().await;

	let seen: Vec<Vec<Seen>> = watched.iter_mut().map(|watch| watch.seen(start)).collect();
	for (i, seen) in seen.iter().enumerate() {
		let notice = KEEP_ALIVE * i as u32 / 2;
		assert!(
			seen.iter().all(|seen| seen.at >= notice),
			"R{}: nothing before it notices: {seen:?}",
			i + 1
		);
		assert_eq!(
			seen.iter().filter(|seen| withdrawn(seen)).count(),
			1,
			"R{}: {seen:?}",
			i + 1
		);
		let ended = seen.last().unwrap();
		assert!(
			withdrawn(ended) && ended.at == KEEP_ALIVE + DEBOUNCE,
			"R{}: let go a debounce after R3 notices: {seen:?}",
			i + 1
		);
	}
	// Pinned: each move is onto a route through the dead `X`, in milliseconds after `X`
	// vanished.
	let moves = |seen: &[Seen]| -> Vec<u128> {
		(seen.iter())
			.filter(|seen| seen.available && !withdrawn(seen))
			.map(|seen| seen.at.as_millis())
			.collect()
	};
	assert_eq!(moves(&seen[0]), [0, 1_500], "R1: {:?}", seen[0]);
	assert_eq!(moves(&seen[1]), [1_500], "R2: {:?}", seen[1]);
	assert_eq!(moves(&seen[2]), [], "R3: {:?}", seen[2]);
}

/// Prior art (the epic's Plan item 30, RFC 4264's dispute wheel): `R1`, `R2` and `R3`
/// each reach `P` directly and form a ring, and each demotes its own link to `P` and
/// promotes the ring link to the next relay, preferring a path through its neighbour to
/// its own. Under BGP's local preference each would take the next relay's direct route
/// and flip back as that relay does the same, forever. A lite-07 path ranks by the lowest
/// preference on it, so every path to `P` carries some relay's demoted link and ranks the
/// same: cost decides, and each settles direct, then stays.
#[kio::test]
async fn a_dispute_wheel_settles() {
	let version = lite07();
	let p = produce_origin(10);
	let _live = publish(&p, "live");
	let relays: Vec<_> = (1..=3).map(produce_origin).collect();
	let mut watched = Vec::new();
	for relay in &relays {
		watched.push(record(relay, "live").await);
	}
	let start = kio::clock::now();
	let mut links = Vec::new();
	for (i, relay) in relays.iter().enumerate() {
		let preference = [0, -1];
		links.push(
			join(
				version,
				&p,
				relay,
				Join {
					preference,
					..Default::default()
				},
			)
			.await,
		);
		let next = &relays[(i + 1) % 3];
		let preference = [0, 1];
		links.push(
			join(
				version,
				next,
				relay,
				Join {
					preference,
					..Default::default()
				},
			)
			.await,
		);
	}
	quiesce().await;
	for (i, watch) in watched.iter_mut().enumerate() {
		let seen = watch.seen(start);
		let last = seen.last().expect("reached");
		assert!(last.available && last.cost == 1, "R{} settles direct: {seen:?}", i + 1);
	}
	quiesce().await;
	for (i, watch) in watched.iter_mut().enumerate() {
		assert_eq!(watch.seen(start), [], "R{} stays", i + 1);
	}
}

/// Prior art (the epic's Plan item 30): `P` reaches `A` over a primary link of cost 1 and
/// `B` over a backup of cost 4, and `A`, `B` and `C` mesh. The primary fails and returns,
/// either within the debounce or long after: every relay ends back on the primary, at the
/// cost it had before.
///
/// `B` and `C` move onto the backup as soon as the cut's news arrives, staying available,
/// and back as soon as the primary returns. `A` is left with the backup through `B`, which
/// routed through `A` and so offered it nothing until it moved: `A` shows the claim
/// unavailable until then, here in the same instant.
#[kio::test]
async fn a_primary_that_fails_and_returns_takes_every_relay_back() {
	for back in [DEBOUNCE / 2, HOLD + DEBOUNCE] {
		let version = lite07();
		let [p, a, b, c] = [1, 2, 3, 4].map(produce_origin);
		let _live = publish(&p, "live");
		let primary = || Join {
			cost: Some(1),
			..Default::default()
		};
		let p_a = join(version, &p, &a, primary()).await;
		let backup = Join {
			cost: Some(4),
			..Default::default()
		};
		let _p_b = join(version, &p, &b, backup).await;
		let _mesh = [
			peer(version, &a, &b).await,
			peer(version, &a, &c).await,
			peer(version, &b, &c).await,
		];
		quiesce().await;

		let mut watched = Vec::new();
		for relay in [&a, &b, &c] {
			watched.push(record(relay, "live").await);
		}
		let start = kio::clock::now();
		cut(&p_a);
		kio::clock::sleep(back).await;
		let _p_a = join(version, &p, &a, primary()).await;
		quiesce().await;
		let seen: Vec<Vec<Seen>> = watched.iter_mut().map(|watch| watch.seen(start)).collect();
		for ((name, cost), seen) in [("A", 1), ("B", 2), ("C", 2)].into_iter().zip(&seen) {
			let last = seen.last().expect("moved");
			assert!(
				last.available && !withdrawn(last) && last.cost == cost,
				"{name} after {back:?}: back on the primary: {seen:?}"
			);
		}
		for ((name, backup), seen) in [("B", 4), ("C", 5)].into_iter().zip(&seen[1..]) {
			assert!(
				seen.iter().all(|seen| seen.available && !withdrawn(seen)),
				"{name} after {back:?}: {seen:?}"
			);
			assert!(
				seen[0].cost == backup && seen[0].at < DEBOUNCE / 2,
				"{name} after {back:?}: onto the backup at once: {seen:?}"
			);
			let returned = seen.iter().position(|seen| seen.cost == 2).expect("returned");
			assert!(
				seen[returned].at >= back,
				"{name} after {back:?}: not back before the primary: {seen:?}"
			);
		}
		assert!(
			seen[0]
				.iter()
				.all(|seen| !withdrawn(seen) && (seen.available || seen.at.is_zero())),
			"A after {back:?}: {:?}",
			seen[0]
		);
		let backup = seen[0].iter().find(|seen| seen.available).expect("moved");
		assert!(
			backup.cost == 5 && backup.at.is_zero(),
			"A after {back:?}: onto the backup at once: {:?}",
			seen[0]
		);
	}
}

/// First-hop selection across a pool: three members each claim `transcode` through two
/// hubs, and two edges hang off one hub each, so every edge reaches every member at the
/// same cost and the tie decides. The tie hashes the path and the member's node rather
/// than anything local to the edge, so both edges pick the same member for every path,
/// and the picks spread over the pool.
#[kio::test]
async fn pool_ties_agree_on_every_edge() {
	let version = lite07();
	let [edge_a, edge_b, hub_a, hub_b] = [1, 2, 3, 4].map(produce_origin);
	let members = [5, 6, 7].map(produce_origin);
	let mut pairs = vec![
		peer(version, &edge_a, &hub_a).await,
		peer(version, &edge_b, &hub_b).await,
		peer(version, &hub_a, &hub_b).await,
	];
	let mut claims = Vec::new();
	for member in &members {
		claims.push(member.claim("transcode", origin::RouteOptions::default()).unwrap());
		pairs.push(peer(version, member, &hub_a).await);
		pairs.push(peer(version, member, &hub_b).await);
	}
	quiesce().await;

	let identities: Vec<_> = claims.iter().map(|claim| Some(claim.identity())).collect();
	let mut picked = std::collections::BTreeSet::new();
	for i in 0..32 {
		let path = format!("transcode/{i}");
		let a = edge_a
			.consume()
			.request(path.as_str(), None)
			.await
			.unwrap()
			.info()
			.identity
			.clone();
		let b = edge_b
			.consume()
			.request(path.as_str(), None)
			.await
			.unwrap()
			.info()
			.identity
			.clone();
		assert_eq!(a, b, "{path}");
		picked.insert(identities.iter().position(|identity| *identity == a).expect("a member"));
	}
	assert!(picked.len() > 1, "every path picked one member: {picked:?}");
	drop(pairs);
}

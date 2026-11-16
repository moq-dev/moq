#![cfg(feature = "async")]

//! Lite-07 routes by their path, on real sessions with time paused.
//!
//! A route names every relay it crossed and the session each learned it on. A relay that
//! loses a session names that hop in one DEAD, every relay drops the routes through it,
//! and the node at the closed session's far end is doubted for a second, so nobody moves
//! onto a stale route through it. A relay left without a route holds its claims,
//! unavailable, until the neighbours that learned the route from it answer, or for
//! [`HOLD`] at most.

use crate::route_change::{link, next, payload, produce_origin, read, timed};
use crate::support;
use support::AtStart;

use std::time::Duration;

use futures::channel::mpsc;
use moq_net::{Error, Pattern, Patterns, Version, auth::Grant, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer, peer_with_latency};

/// How long a relay holds a route no neighbour answers for, and lets an announce
/// stream stall before closing its session.
pub(super) const HOLD: Duration = Duration::from_secs(10);

/// How long a closed session's far end is doubted.
pub(super) const DOUBT: Duration = Duration::from_secs(1);

/// The wip version the path rule runs on.
pub(super) fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// One change a node showed at a path: how long after the cut, its kind, and whether a
/// usable route reached the publisher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen {
	pub at: Duration,
	pub kind: support::Kind,
	pub available: bool,
	/// The cost of the route shown, which tells its path apart.
	pub cost: u64,
}

/// The changes a node shows at a path.
pub(super) struct Watch {
	rx: mpsc::UnboundedReceiver<(kio::clock::Instant, support::Kind, bool, u64)>,
}

impl Watch {
	/// What the node showed since the last call, timed from `start`.
	pub(super) fn seen(&mut self, start: kio::clock::Instant) -> Vec<Seen> {
		std::iter::from_fn(|| self.rx.try_recv().ok())
			.map(|(at, kind, available, cost)| Seen {
				at: at.saturating_duration_since(start),
				kind,
				available,
				cost,
			})
			.collect()
	}
}

/// Watch every change `node` shows at `path`, past what it shows now.
pub(super) async fn record(node: &origin::Producer, path: &'static str) -> Watch {
	let mut routes = node.consume().routes();
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		while let Some(event) = routes.next().await {
			if event.announce.prefix.as_str() != path {
				continue;
			}
			let kind = match event.kind {
				origin::RouteKind::Start => support::Kind::Start,
				origin::RouteKind::Update => support::Kind::Update,
				origin::RouteKind::Restart => support::Kind::Restart,
				origin::RouteKind::End => support::Kind::End,
				origin::RouteKind::Offline => support::Kind::Offline,
			};
			let route = &event.announce.route;
			let seen = (kio::clock::now(), kind, route.available(), route.cost().value());
			if tx.unbounded_send(seen).is_err() {
				break;
			}
		}
	});
	let mut watch = Watch { rx };
	land().await;
	watch.seen(kio::clock::now());
	watch
}

/// Whether the node let go of the path, ended or offline.
pub(super) fn withdrawn(seen: &Seen) -> bool {
	matches!(seen.kind, support::Kind::End | support::Kind::Offline)
}

/// Close a session at both ends, as a dropped connection does.
pub(super) fn cut(pair: &MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

/// Publish `path` on `node`.
pub(super) fn publish(node: &origin::Producer, path: &str) -> origin::Route {
	let route = node.route(path, origin::RouteOptions::hidden()).unwrap();
	route.announce().unwrap();
	route
}

/// Wait long enough for every message in flight to land, without reaching the doubt.
pub(super) async fn land() {
	kio::clock::sleep(Duration::from_millis(100)).await;
}

/// Let the network settle, doubts and holds included.
pub(super) async fn quiesce() {
	kio::clock::sleep(HOLD + DOUBT).await;
}

/// `W` reaches `P` through `A`, and through `C` and `D`, which never cross `A`. The session
/// from `A` to `W` closes: `W` doubts `A`, the far end, and the detour avoids it, so `W`
/// moves onto it at once and the claim never turns unavailable.
#[kio::test]
async fn a_detour_avoiding_the_far_end_is_taken_at_once() {
	let version = lite07();
	let [p, a, c, d, w] = [1, 2, 3, 4, 5].map(produce_origin);
	let _live = publish(&p, "live");
	let _p_a = peer(version, &p, &a).await;
	let a_w = peer(version, &a, &w).await;
	let _p_c = peer(version, &p, &c).await;
	let _c_d = peer(version, &c, &d).await;
	let _d_w = peer(version, &d, &w).await;
	quiesce().await;

	let mut watch = record(&w, "live").await;
	let start = kio::clock::now();
	cut(&a_w);
	land().await;
	let seen = watch.seen(start);
	assert!(
		seen.iter().all(|seen| seen.available && !withdrawn(seen)),
		"the detour serves at once: {seen:?}"
	);
}

/// `W` reaches `P` through `A`, and through `B`, whose route crosses `A` too. The session
/// from `A` to `W` closes: `W` cannot tell `A` from a dead relay whose news is still on its
/// way, so it moves onto `B`'s route only once the doubt lifts, holding the claim meanwhile.
#[kio::test]
async fn a_detour_through_the_far_end_waits_out_the_doubt() {
	let version = lite07();
	let [p, a, b, w] = [1, 2, 3, 4].map(produce_origin);
	let _live = publish(&p, "live");
	let _p_a = peer(version, &p, &a).await;
	let a_w = peer(version, &a, &w).await;
	let _a_b = peer(version, &a, &b).await;
	let _b_w = peer(version, &b, &w).await;
	quiesce().await;

	let mut watch = record(&w, "live").await;
	let start = kio::clock::now();
	cut(&a_w);
	quiesce().await;
	let seen = watch.seen(start);
	assert!(
		!seen.iter().any(withdrawn),
		"the claim is held, not withdrawn: {seen:?}"
	);
	let held = seen.iter().find(|seen| !seen.available).expect("held meanwhile");
	let usable = seen
		.iter()
		.find(|seen| seen.available)
		.expect("B's route serves in the end");
	assert!(usable.at >= held.at + DOUBT, "not before the doubt lifts: {seen:?}");
}

/// `P` is on `A` and `B`, which peer with each other, and `W` is behind both. `P`'s session
/// to `A` closes and reopens every half doubt. Doubt about `P` keeps no relay off `B`'s
/// route, which begins at `P`, so `W` shows the claim available throughout and its
/// subscriber reads each group at once. The reopened session waits out the doubt.
#[kio::test]
async fn a_flapping_link_keeps_a_dual_homed_publisher_routed() {
	let version = lite07();
	let [p, a, b, w] = [1, 2, 3, 4].map(produce_origin);
	let live = publish(&p, "live");
	let track = live.broadcast().create_track("video", timed()).unwrap();
	let mut p_a = peer(version, &p, &a).await;
	let _p_b = peer(version, &p, &b).await;
	let _a_b = peer(version, &a, &b).await;
	let _a_w = peer(version, &a, &w).await;
	let _b_w = peer(version, &b, &w).await;
	quiesce().await;

	let mut watch = record(&w, "live").await;
	let broadcast = w.consume().request("live", None).await.unwrap();
	let mut rx = read(broadcast.track("video").unwrap().subscribe(None).await.unwrap());
	let start = kio::clock::now();
	for flap in 0..8 {
		match flap % 2 {
			0 => cut(&p_a),
			_ => p_a = peer(version, &p, &a).await,
		}
		let sent = kio::clock::now();
		let mut group = track.append_group(None).unwrap();
		group.write_frame(group.at_start(), payload(flap, 0)).unwrap();
		group.finish().unwrap();
		assert_eq!(next(&mut rx).await, (flap, payload(flap, 0)), "flap {flap}");
		let late = kio::clock::now().saturating_duration_since(sent);
		assert!(late < DOUBT / 2, "flap {flap}: read {late:?} after it was written");
		kio::clock::sleep(DOUBT / 2).await;
	}
	let seen = watch.seen(start);
	assert!(
		seen.iter().all(|seen| seen.available && !withdrawn(seen)),
		"available throughout: {seen:?}"
	);
}

/// `X` is `P`'s only way out and neighbours `R1`, `R2` and `R3`, which mesh with each other.
/// `X` dies: each relay hears its own session close and its neighbours' DEADs, and none
/// moves onto another's route through `X` on the way. Each withdraws the claim once.
#[kio::test]
async fn a_dead_relay_reaches_every_neighbour_as_withdrawals() {
	let version = lite07();
	let [p, x, r1, r2, r3] = [1, 2, 3, 4, 5].map(produce_origin);
	let _live = publish(&p, "live");
	let mut dying = vec![peer(version, &p, &x).await];
	for r in [&r1, &r2, &r3] {
		dying.push(peer(version, &x, r).await);
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
	dying.iter().for_each(cut);
	quiesce().await;
	for (i, watch) in watched.iter_mut().enumerate() {
		let seen = watch.seen(start);
		assert!(
			!seen.iter().any(|seen| seen.available && !withdrawn(seen)),
			"R{}: no move onto a route through X: {seen:?}",
			i + 1
		);
		assert_eq!(
			seen.iter().filter(|seen| withdrawn(seen)).count(),
			1,
			"R{}: {seen:?}",
			i + 1
		);
		assert!(withdrawn(seen.last().unwrap()), "R{}: {seen:?}", i + 1);
	}
}

/// What `B` does while `A` holds the route for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Child {
	/// Answers, over a link with a one-way [`LAG`].
	Answers,
	/// Never answers, and its session closes [`LAG`] after the cut.
	Closes,
	/// Never answers.
	Silent,
}

/// How long `B` takes in [`hold_behind`].
const LAG: Duration = Duration::from_secs(2);

/// `P` feeds `A`, which feeds `C`, and peers with `B`, which learned the route from `A` and
/// offers it back. `P`'s session to `A` closes: `A` holds the route for `B`, so `C` sees the
/// claim unavailable, then withdrawn once the hold ends. Returns how long after the cut that
/// was.
async fn hold_behind(child: Child) -> Duration {
	let version = lite07();
	let [p, a, b, c] = [1, 2, 3, 4].map(produce_origin);
	let _live = publish(&p, "live");
	let p_a = link(version, &p, &a).await;
	let lag = if child == Child::Answers { LAG } else { Duration::ZERO };
	let a_b = peer_with_latency(version, &a, &b, lag).await;
	let _a_c = link(version, &a, &c).await;
	// A reader at `B` asks `A` for its routes.
	let _reader = record(&b, "live").await;
	quiesce().await;

	let mut watch = record(&c, "live").await;
	if child != Child::Answers {
		a_b.server_transport.hold_writes();
	}
	let start = kio::clock::now();
	cut(&p_a);
	if child == Child::Closes {
		kio::clock::sleep(LAG).await;
		cut(&a_b);
	}
	quiesce().await;
	let seen = watch.seen(start);
	assert!(
		matches!(
			seen.first(),
			Some(Seen {
				kind: support::Kind::Update,
				available: false,
				..
			})
		),
		"held first: {seen:?}"
	);
	assert_eq!(seen.iter().filter(|seen| withdrawn(seen)).count(), 1, "{seen:?}");
	let ended = seen.last().unwrap();
	assert!(withdrawn(ended), "{seen:?}");
	ended.at
}

/// `A` loses its only route and holds the claim for `B`, which learned it from `A`. `B`
/// answers once the DEAD reaches it, a round trip later, and the hold ends then.
#[kio::test]
async fn a_hold_ends_behind_an_answering_neighbour() {
	let ended = hold_behind(Child::Answers).await;
	assert!(ended >= 2 * LAG && ended < 2 * LAG + DOUBT, "ended at {ended:?}");
}

/// [`a_hold_ends_behind_an_answering_neighbour`], with `B` silent until its session closes:
/// nothing is left to wait for.
#[kio::test]
async fn a_hold_ends_behind_a_closed_session() {
	let ended = hold_behind(Child::Closes).await;
	assert!(ended >= LAG && ended < LAG + DOUBT, "ended at {ended:?}");
}

/// [`a_hold_ends_behind_an_answering_neighbour`], with `B` silent: the hold ends at its
/// deadline.
#[kio::test]
async fn a_hold_ends_at_its_deadline_behind_a_silent_neighbour() {
	let ended = hold_behind(Child::Silent).await;
	assert!(ended >= HOLD && ended < HOLD + DOUBT, "ended at {ended:?}");
}

/// `W` reaches `P` through `A`. `Y` and `Z` peer with `W` and with each other, and carry
/// nothing of `P`'s: their session closing names a hop no route of `W`'s crosses, which
/// changes nothing there.
#[kio::test]
async fn an_unknown_hop_in_a_dead_changes_nothing() {
	let version = lite07();
	let [p, a, w, y, z] = [1, 2, 3, 4, 5].map(produce_origin);
	let _live = publish(&p, "live");
	let _p_a = peer(version, &p, &a).await;
	let _a_w = peer(version, &a, &w).await;
	let _w_y = peer(version, &w, &y).await;
	let _w_z = peer(version, &w, &z).await;
	let y_z = peer(version, &y, &z).await;
	quiesce().await;

	let mut watch = record(&w, "live").await;
	let start = kio::clock::now();
	cut(&y_z);
	quiesce().await;
	assert_eq!(watch.seen(start), [], "W keeps its route as it was");
}

/// A chain one relay longer than a route's hops allow: the last relay refuses the route
/// whose path is full, and every relay before it uses its own.
#[kio::test]
async fn a_path_at_the_hop_limit_is_refused() {
	let version = lite07();
	let nodes: Vec<_> = (1..=33).map(produce_origin).collect();
	let mut watched = Vec::new();
	for node in &nodes {
		watched.push(record(node, "live").await);
	}
	let start = kio::clock::now();
	let _live = publish(&nodes[0], "live");
	let mut links = Vec::new();
	for pair in nodes.windows(2) {
		links.push(link(version, &pair[0], &pair[1]).await);
	}
	quiesce().await;

	let reached: Vec<bool> = (watched.iter_mut())
		.map(|watch| watch.seen(start).iter().any(|seen| seen.available))
		.collect();
	// The publisher names itself in one hop, and each relay adds its own, so the 32nd
	// relay past it hears a path of 32 hops.
	assert!(reached[..32].iter().all(|reached| *reached), "{reached:?}");
	assert!(!reached[32], "the full path is refused");
}

/// A peer whose session is limited to `a` reports its own session closing. That DEAD
/// covers only the routes that peer sent: `W` keeps the route through `B`, which crosses
/// the same hop, until `B` says so too, and moves onto it meanwhile.
#[kio::test]
async fn a_scoped_peers_dead_leaves_other_sessions_routes_alone() {
	let version = lite07();
	let [p, s, b, w] = [1, 2, 3, 4].map(produce_origin);
	let _live = publish(&p, "a/live");
	let p_s = link(version, &p, &s).await;
	let _s_b = link(version, &s, &b).await;
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(b.consume());
	options.client_subscribe = Some(w.clone());
	options.latency = Duration::from_millis(300);
	let _b_w = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(s.consume());
	options.server_subscribe = Some(w.clone());
	let s_w = connect_mock(options).await;
	let mut grant = Grant::default();
	grant.publish = [Pattern::subtree("a").unwrap()].into_iter().collect::<Patterns>();
	s_w.server.auth().authorize(&grant);
	quiesce().await;

	let mut watch = record(&w, "a/live").await;
	let start = kio::clock::now();
	cut(&p_s);
	kio::clock::sleep(Duration::from_millis(200)).await;
	let early = watch.seen(start);
	assert!(
		early.iter().all(|seen| seen.available && !withdrawn(seen)),
		"B's route serves until B reports the hop: {early:?}"
	);
	quiesce().await;
	let late = watch.seen(start);
	assert!(late.iter().any(withdrawn), "B's report withdraws it: {late:?}");
}

/// A peer that stops reading its announce stream stalls every route its relay sends it:
/// the relay closes the session once nothing moved for [`HOLD`].
#[kio::test]
async fn a_peer_that_stops_reading_is_closed() {
	let version = lite07();
	let [p, w] = [1, 2].map(produce_origin);
	let _first = publish(&p, "first");
	let p_w = link(version, &p, &w).await;
	quiesce().await;

	p_w.server_transport.hold_writes();
	let start = kio::clock::now();
	let _second = publish(&p, "second");
	let closed = kio::clock::timeout(HOLD * 2, p_w.server.closed()).await;
	let after = kio::clock::now().saturating_duration_since(start);
	assert!(closed.is_ok(), "the session closes");
	assert!(after >= HOLD, "not before the hold: {after:?}");
}

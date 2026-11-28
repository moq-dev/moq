#![cfg(feature = "async")]

//! Relays of one domain that exchange claims on lite-07 without ROUTE, DEAD, or DRAIN,
//! leaving reachability to whatever sits below them, as a node layer would.
//!
//! A session wired one way (one relay publishes, the other subscribes) declares no Node
//! ID, so each end files the other as scoped: claims cross with a chain and no route is
//! ever offered. Relays pulling from each other over such sessions form the domain.

use crate::path_routes::{DEBOUNCE, cut, land, lite07, publish, quiesce, record, withdrawn};
use crate::route_change::{link, next, payload, produce_origin, read, timed};
use crate::support::{self, AppendNext, AtStart};

use moq_net::{Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// `to` pulls everything `from` publishes over a link both ends mark upstream, so
/// neither passes a claim it learned from one in-domain peer to another, as iBGP's
/// split horizon keeps a full mesh from echoing.
async fn marked(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.clone().with_link(origin::Link::Upstream).consume());
	options.client_subscribe = Some(to.clone().with_link(origin::Link::Upstream));
	connect_mock(options).await
}

/// `a` and `b` each pull everything the other publishes, over two one-way sessions.
async fn both(version: Version, a: &origin::Producer, b: &origin::Producer) -> [MockPair; 2] {
	[link(version, a, b).await, link(version, b, a).await]
}

/// A full mesh of `A`, `B`, and `C` with every in-domain link marked, and a viewer
/// pulling from `B` unmarked. A claim on `A` reaches each once, and its publisher
/// ending it shows as an end everywhere at once.
#[kio::test]
async fn a_marked_mesh_without_routes_ends_a_claim_at_once() {
	let version = lite07();
	let [a, b, c, viewer] = [1, 2, 3, 4].map(produce_origin);
	let mut links = Vec::new();
	for (from, to) in [(&a, &b), (&b, &a), (&b, &c), (&c, &b), (&c, &a), (&a, &c)] {
		links.push(marked(version, from, to).await);
	}
	links.push(link(version, &b, &viewer).await);
	let mut watches = Vec::new();
	for (name, node) in [("B", &b), ("C", &c), ("viewer", &viewer)] {
		watches.push((name, record(node, "live").await));
	}

	let start = kio::clock::now();
	let live = publish(&a, "live");
	quiesce().await;
	for (name, watch) in &mut watches {
		let seen = watch.seen(start);
		assert_eq!(seen.len(), 1, "{name} starts the claim once and keeps it: {seen:?}");
		assert_eq!(seen[0].kind, support::Kind::Start, "{name}: {seen:?}");
		assert!(seen[0].available, "{name} reaches it: {seen:?}");
	}

	let gone = kio::clock::now();
	drop(live);
	quiesce().await;
	for (name, watch) in &mut watches {
		let seen = watch.seen(gone);
		assert_eq!(seen.len(), 1, "{name} only ends the claim: {seen:?}");
		assert_eq!(seen[0].kind, support::Kind::End, "{name}: {seen:?}");
		assert!(seen[0].at.is_zero(), "{name} ends it at once: {seen:?}");
	}
}

/// The ring `A`, `B`, `C` with nothing marked, so each relay passes on what it learns. A
/// claim on `A` reaches the others once each and never comes back to `A`. Its withdrawal
/// leaves each holding the other's echo, which no route or revision orders, so each lets
/// go only when the debounce runs out and never revives it. This pins only that much:
/// how the end shows (offline, as of this writing) is left to a later fix.
#[kio::test]
async fn a_ring_without_routes_lets_an_echo_go_within_the_debounce() {
	let version = lite07();
	let [a, b, c] = [1, 2, 3].map(produce_origin);
	let _ab = both(version, &a, &b).await;
	let _bc = both(version, &b, &c).await;
	let _ca = both(version, &c, &a).await;
	let mut watches = Vec::new();
	for (name, node) in [("A", &a), ("B", &b), ("C", &c)] {
		watches.push((name, record(node, "live").await));
	}

	let start = kio::clock::now();
	let live = publish(&a, "live");
	quiesce().await;
	for (name, watch) in &mut watches {
		let seen = watch.seen(start);
		let starts = seen.iter().filter(|s| s.kind == support::Kind::Start).count();
		assert_eq!(starts, 1, "{name} starts the claim once: {seen:?}");
		assert!(!seen.iter().any(withdrawn), "{name} keeps the claim: {seen:?}");
		assert!(seen.last().unwrap().available, "{name} reaches it: {seen:?}");
	}

	let gone = kio::clock::now();
	drop(live);
	quiesce().await;
	for (name, watch) in &mut watches {
		let seen = watch.seen(gone);
		let end = seen.iter().position(withdrawn);
		let end = end.unwrap_or_else(|| panic!("{name} lets the claim go: {seen:?}"));
		assert_eq!(end + 1, seen.len(), "{name} shows nothing after it: {seen:?}");
		assert!(seen[end].at <= DEBOUNCE, "{name} lets go within the debounce: {seen:?}");
	}
}

/// A reader on `B` served straight from `A` on the unmarked ring keeps reading through
/// `C` when both sessions between `A` and `B` close, with no restart.
#[kio::test]
async fn a_reader_moves_around_a_ring_without_routes() {
	let version = lite07();
	let [a, b, c] = [1, 2, 3].map(produce_origin);
	let broadcast = a.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();
	let mut at_b = record(&b, "live").await;
	let start = kio::clock::now();
	let ab = both(version, &a, &b).await;
	let _bc = both(version, &b, &c).await;
	let _ca = both(version, &c, &a).await;
	land().await;
	let seen = at_b.seen(start);
	assert_eq!(seen.last().unwrap().cost, 1, "B is served straight from A: {seen:?}");

	let remote = b
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let sub = remote
		.track("video")
		.unwrap()
		.subscribe(track::Subscription::default())
		.await
		.unwrap();
	let mut rx = read(sub);

	let mut group = track.append_next(payload(0, 0)).unwrap();
	assert_eq!(next(&mut rx).await, (0, payload(0, 0)));

	let moved = kio::clock::now();
	for pair in &ab {
		cut(pair);
	}
	quiesce().await;
	let seen = at_b.seen(moved);
	let last = seen.last().unwrap_or_else(|| panic!("B moves: {seen:?}"));
	assert_eq!(last.cost, 2, "B moves onto the copy through C: {seen:?}");
	assert!(
		last.available && !seen.iter().any(withdrawn),
		"B never lets go: {seen:?}"
	);
	assert!(
		!seen.iter().any(|event| event.kind == support::Kind::Restart),
		"B keeps the Identity: {seen:?}"
	);
	group.write_frame(group.at_start(), payload(0, 1)).unwrap();
	group.finish().unwrap();
	assert_eq!(next(&mut rx).await, (0, payload(0, 1)));
	let _group = track.append_next(payload(1, 0)).unwrap();
	assert_eq!(next(&mut rx).await, (1, payload(1, 0)));
}

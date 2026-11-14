//! An interest as a set of prefixes edited in place, under [`path::Router`]: what
//! an edit keeps, what it sends, and how it orders against everything else a
//! neighbour says on its one stream.

use super::*;

const ROOMS: [&str; 3] = ["live/a", "live/b", "live/c"];

/// The origin behind one relay, and a leaf client of the relay watching
/// `watch`, with `per` claims in each of `ROOMS`.
fn chain(watch: &[&str], per: usize) -> Sim {
	let (origin, relay, client) = (0, 1, 2);
	let mut sim = Sim::with(Rule::PATH, 3);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, relay, 1);
	sim.join(relay, client, 1);
	sim.watch_all(client, watch);
	sim.originate(origin, 7, 0);
	for room in ROOMS {
		for i in 0..per {
			sim.announce(origin, &format!("{room}/{i}"), 0, None);
		}
	}
	assert!(sim.run());
	sim
}

/// The claims `client` shows, available.
fn shown(sim: &Sim, client: Node) -> BTreeSet<Path> {
	(sim.shown(client).iter())
		.filter(|(_, shown)| shown.available)
		.map(|((_, claim), _)| claim.clone())
		.collect()
}

fn rooms(rooms: &[&str], per: usize) -> BTreeSet<Path> {
	(rooms.iter())
		.flat_map(|room| (0..per).map(move |i| p(&format!("{room}/{i}"))))
		.collect()
}

/// Swapping two prefixes for the one covering both, as the maintainer put it
/// ("fleet absorbing the previous two without retransmitting everything"), sends
/// only what the wider prefix newly covers, and the reader sees only that start.
#[test]
fn widening_swaps_prefixes_without_resending_what_both_cover() {
	const PER: usize = 20;
	let client = 2;
	let mut sim = chain(&["live/a", "live/b"], PER);
	assert_eq!(shown(&sim, client), rooms(&["live/a", "live/b"], PER));
	let (items, before) = (sim.stats.claim_items, sim.events.len());

	sim.watch(client, "live");
	assert!(sim.run());
	assert_eq!(shown(&sim, client), rooms(&ROOMS, PER));
	assert_eq!(sim.stats.claim_items - items, PER, "only live/c is sent");
	let events = &sim.events[before..];
	assert!(
		events
			.iter()
			.all(|event| event.kind == Kind::Start && event.claim.starts_with("live/c/")),
		"{events:?}"
	);
	assert_eq!(sim.path(client).is_synced(1, &p("live")), Some(true));
	sim.check().unwrap();
}

/// Swapping one prefix for two inside it carries the wider floor to each, so a
/// narrowing that changes nothing it keeps sends no claim, and the reader only
/// stops watching what it dropped.
#[test]
fn narrowing_carries_the_wider_floor() {
	const PER: usize = 20;
	let client = 2;
	let mut sim = chain(&["live"], PER);
	let (items, before) = (sim.stats.claim_items, sim.events.len());

	sim.watch_all(client, &["live/a", "live/b"]);
	assert!(sim.run());
	assert_eq!(shown(&sim, client), rooms(&["live/a", "live/b"], PER));
	assert_eq!(sim.stats.claim_items, items, "the floor carries, so nothing is sent");
	let events = &sim.events[before..];
	assert!(
		events
			.iter()
			.all(|event| event.kind == Kind::Unwatched && event.claim.starts_with("live/c/")),
		"{events:?}"
	);
	// The carried floor still certifies a withdrawal.
	sim.unannounce(0, "live/a/0");
	assert!(sim.run());
	assert_eq!(life(&sim, client, "live/a/0"), [Kind::Start, Kind::End]);
	sim.check().unwrap();
}

/// What the relay sent before the marker answers the old set: a withdrawal in a
/// region the edit drops is never shown, one in a region it keeps ends the claim,
/// and the wider set then shows the origin's claims, with nothing revived.
#[test]
fn an_edit_crossing_claim_changes_applies_them_to_the_set_they_answered() {
	let (origin, relay, client) = (0, 1, 2);
	for widen in [false, true] {
		let mut sim = chain(&["live/a", "live/c"], 2);
		sim.hold(relay, client);
		sim.unannounce(origin, "live/a/0");
		sim.unannounce(origin, "live/c/0");
		sim.announce(origin, "live/b/9", 0, None);
		assert!(sim.run());
		match widen {
			true => sim.watch(client, "live"),
			false => sim.watch_all(client, &["live/a"]),
		}
		assert!(sim.run());
		assert!(sim.queued(relay, client) > 0, "the old set's answer waits");
		sim.release(relay, client);
		assert!(sim.run());
		let expected = match widen {
			true => ["live/a/1", "live/b/0", "live/b/1", "live/b/9", "live/c/1"].as_slice(),
			false => ["live/a/1"].as_slice(),
		};
		assert_eq!(shown(&sim, client), expected.iter().map(|claim| p(claim)).collect());
		assert_eq!(life(&sim, client, "live/a/0"), [Kind::Start, Kind::End]);
		let dropped: &[Kind] = match widen {
			true => &[Kind::Start, Kind::End],
			false => &[Kind::Start, Kind::Unwatched],
		};
		assert_eq!(life(&sim, client, "live/c/0"), dropped);
		assert_eq!(revivals(&sim.events), 0);
		assert_eq!(false_ends(&sim.events), 0);
		sim.check().unwrap();
	}
}

/// An edit crosses a session closing behind the relay: the relay's hold on its
/// route and the withdrawal naming the dead hop ride the same stream as the
/// marker, in either order. The client holds the claim it kept through the
/// marker exactly as it does with no edit, then reaches it on the detour, with
/// nothing ended or revived.
#[test]
fn an_edit_crossing_a_dead_hop_and_a_hold_settles() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut baseline = None;
	for edit in [None, Some(false), Some(true)] {
		let edit_first = edit == Some(true);
		let mut sim = Sim::with(Rule::PATH, 4);
		sim.leaf(origin);
		sim.leaf(client);
		sim.join(origin, r1, 1);
		sim.join(origin, r2, 3);
		sim.join(r1, client, 1);
		sim.join(r2, client, 1);
		sim.join(r1, r2, 1);
		sim.watch_all(client, &["live/a"]);
		sim.originate(origin, 7, 0);
		sim.announce(origin, "live/a/x", 0, None);
		sim.announce(origin, "live/b/y", 0, None);
		assert!(sim.run());
		assert_eq!(sim.next_hop(client, 7), Some(r1));

		sim.hold(r1, client);
		sim.hold(r2, client);
		if edit_first {
			sim.watch(client, "live");
			sim.disconnect(origin, r1);
		} else {
			sim.disconnect(origin, r1);
			if edit.is_some() {
				sim.watch(client, "live");
			}
		}
		assert!(sim.run());
		sim.release(r1, client);
		sim.release(r2, client);
		assert!(sim.run());
		if edit.is_some() {
			assert_eq!(shown(&sim, client), [p("live/a/x"), p("live/b/y")].into());
		}
		assert_eq!(sim.reaches(client, 7, &p("live/a/x")), Some(origin));
		let life = life(&sim, client, "live/a/x");
		assert_eq!(&life, baseline.get_or_insert_with(|| life.clone()), "edit {edit:?}");
		assert!(
			life.iter().all(|kind| !matches!(kind, Kind::End | Kind::Offline)),
			"{life:?}"
		);
		assert_eq!(false_ends(&sim.events), 0);
		assert_eq!(revivals(&sim.events), 0);
		assert_eq!(sim.pending(), 0);
		sim.check().unwrap();
	}
}

/// Each added prefix is SYNCED on its own: one waiting behind a held stream is
/// not, while the other neighbour's already is.
#[test]
fn each_added_prefix_is_synced_on_its_own() {
	let (origin, r1, r2, client) = (0, 1, 2, 3);
	let mut sim = Sim::with(Rule::PATH, 4);
	sim.leaf(origin);
	sim.leaf(client);
	sim.join(origin, r1, 1);
	sim.join(origin, r2, 1);
	sim.join(r1, client, 1);
	sim.join(r2, client, 1);
	sim.watch_all(client, &["live/a"]);
	assert!(sim.run());
	sim.hold(r2, client);
	sim.watch_all(client, &["live/a", "live/b"]);
	assert!(sim.run());
	let router = sim.path(client);
	assert_eq!(router.is_synced(r1, &p("live/b")), Some(true));
	assert_eq!(router.is_synced(r2, &p("live/a")), Some(true));
	assert_eq!(
		router.is_synced(r2, &p("live/b")),
		None,
		"its edit waits behind the marker"
	);
	assert_eq!(router.is_synced(r1, &p("live/c")), None);
	sim.release(r2, client);
	assert!(sim.run());
	assert_eq!(sim.path(client).is_synced(r2, &p("live/b")), Some(true));
}

/// An edit that names a prefix it does not hold, or leaves one inside another, is
/// refused whole, and so is an interest with one prefix inside another.
#[test]
fn edits_that_nest_or_name_no_live_prefix_are_refused() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	router.connect(1, 1, Link::default()).unwrap();
	let edit = |remove: &[&str], add: &[&str]| path::Edit {
		remove: remove.iter().map(|prefix| p(prefix)).collect(),
		add: add.iter().map(|prefix| p(prefix)).collect(),
	};
	router.edit(1, edit(&[], &["live/a"])).unwrap();
	assert_eq!(router.edit(1, edit(&["live/b"], &[])), Err(Error::UnknownPrefix));
	assert_eq!(router.edit(1, edit(&[], &["live/a/x"])), Err(Error::Nested));
	assert_eq!(router.edit(1, edit(&[], &["live"])), Err(Error::Nested));
	assert_eq!(router.edit(1, edit(&[], &["live/b", "live/b/x"])), Err(Error::Nested));
	assert_eq!(router.edit(2, edit(&[], &["live"])), Err(Error::UnknownNeighbor));
	let markers = |router: &mut path::Router<usize, u8>| {
		(router.drain().iter())
			.filter(|out| matches!(out, path::Out::Edited { .. }))
			.count()
	};
	assert_eq!(markers(&mut router), 1, "a refused edit sends no marker");
	// A swap in one edit is no nesting.
	router.edit(1, edit(&["live/a"], &["live"])).unwrap();
	assert_eq!(markers(&mut router), 1);
	assert_eq!(
		router.watch(BTreeSet::from([p("live"), p("live/a")])),
		Err(Error::Nested)
	);
}

/// A scope with several regions asks for each where it meets the interest, so
/// each is certified on its own.
#[test]
fn a_scope_with_several_regions_asks_for_each() {
	let mut router = path::Router::<usize, u8>::new(99, 1, Doubt::DEFAULT);
	router.watch(BTreeSet::from([p("live")])).unwrap();
	let link = Link {
		scope: Some(Scope {
			regions: vec![p("live/a"), p("live/b/x"), p("other"), p("")],
			qualify: Arc::new(|id| id | QUALIFIED),
		}),
		..Link::default()
	};
	router.connect(1, 1, link).unwrap();
	let asked: Vec<path::Edit> = (router.drain().into_iter())
		.filter_map(|out| match out {
			path::Out::Ask { edit, .. } => Some(edit),
			_ => None,
		})
		.collect();
	// The root region meets the interest at `live`, which covers the narrower two.
	assert_eq!(
		asked,
		[path::Edit {
			remove: Vec::new(),
			add: vec![p("live")]
		}]
	);
	let link = Link {
		scope: Some(Scope {
			regions: vec![p("live/a"), p("live/b/x"), p("other")],
			qualify: Arc::new(|id| id | QUALIFIED),
		}),
		..Link::default()
	};
	router.connect(2, 2, link).unwrap();
	let asked: Vec<path::Edit> = (router.drain().into_iter())
		.filter_map(|out| match out {
			path::Out::Ask { edit, .. } => Some(edit),
			_ => None,
		})
		.collect();
	assert_eq!(
		asked,
		[path::Edit {
			remove: Vec::new(),
			add: vec![p("live/a"), p("live/b/x")]
		}]
	);
}

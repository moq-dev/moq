//! Failures: a relay resuming on a detour learned after its session closed, a relay dying silently while
//! its neighbours notice seconds apart, failures at one instant on a mesh larger than any
//! exhaustive check, and no group crossing a link twice for one subscription.

use super::{Feed, Join, Mesh, Source, detour_lost, measured, readers, reports, ring, shown};
use crate::path_routes::{cut, lite07, quiesce};
use crate::support::Rng;

use std::collections::BTreeSet;
use std::time::Duration;

/// A ring of `count` relays with a publisher on `R0`, the link from `R1` to `R2` priced
/// `r1_r2`. The link from `R0` to `R1` closes, and `R1` resumes the way round the ring.
/// Unless `held`, every other route to `R0` ran through `R1`, so `R1` held none at the cut:
/// its detour arrives a round trip later, once `R2` hears the closed session named.
async fn detour(count: usize, r1_r2: u64, held: bool) {
	let mut mesh = Mesh::new(count);
	for i in 0..count {
		let cost = if i == 1 { r1_r2 } else { 1 };
		mesh.join(i, (i + 1) % count, Join::peer(cost)).await;
	}
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let _push = source.push(&mesh.relays[0], lite07(), 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	mesh.cut(0, 1);
	quiesce().await;
	let case = format!("ring of {count}, R1-R2 at {r1_r2}, R0-R1 cut");
	for (i, report) in reports(&mut readers, since, &case).iter().enumerate() {
		if i == 1 && !held {
			assert!(detour_lost(report, false), "{case}: R1: {report:?}");
			continue;
		}
		assert!(report.seamless(), "{case}: R{i}: {report:?}");
	}
	assert_eq!(mesh.doubled(), [], "ring of {count}, R1-R2 at {r1_r2}");
}

/// `R1` holds another route to `R0` when its session closes: in a ring of three `R2`
/// reaches `R0` directly, and priced high, `R1`-`R2` keeps `R2`'s route off `R1`.
#[kio::test]
async fn a_relay_resumes_on_a_detour_it_already_holds() {
	for (count, r1_r2) in [(3, 1), (4, 3), (5, 4)] {
		detour(count, r1_r2, true).await;
	}
}

/// `R1` holds no other route to `R0` when its session closes.
#[kio::test]
async fn a_relay_resumes_on_a_detour_learned_after_the_cut() {
	for count in [4, 5, 6] {
		detour(count, 1, false).await;
	}
}

/// How far apart the neighbours of a silent relay notice it is gone.
const NOTICE: Duration = Duration::from_millis(1500);

/// One encoder pushes to `X` (`R0`) and `Y` (`R1`). `X` links to `R2`, `R3`, and `R4` at
/// cost 1 and `Y` to each at 3, and `R2` to `R4` mesh at cost 1, so every reader reads
/// through `X`. `X` goes silent: nothing it holds delivers anything more, and its
/// neighbours notice one at a time, `R2` at once, `R3` [`NOTICE`] later, `R4` and the
/// encoder twice that, as keep-alives spaced that far apart would. Every reader reads on
/// in the broadcast it had, every group once.
#[kio::test]
async fn a_silent_relay_is_routed_around_as_its_neighbours_notice() {
	let mut mesh = Mesh::new(5);
	for r in [2, 3, 4] {
		mesh.join(0, r, Join::peer(1)).await;
		mesh.join(1, r, Join::peer(3)).await;
	}
	for (a, b) in [(2, 3), (3, 4), (2, 4)] {
		mesh.join(a, b, Join::peer(1)).await;
	}
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let x_push = source.push(&mesh.relays[0], lite07(), 1).await;
	let _y_push = source.push(&mesh.relays[1], lite07(), 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	x_push.client_transport.hold_writes();
	for r in [2, 3, 4] {
		mesh.link(0, r).client_transport.hold_writes();
	}
	for (i, r) in [2, 3, 4].into_iter().enumerate() {
		kio::clock::sleep_until(since.saturating_add(NOTICE * i as u32)).await;
		mesh.cut(0, r);
	}
	cut(&x_push);
	quiesce().await;
	let reports = reports(&mut readers, since, &format!("X silent, noticed {NOTICE:?} apart"));
	// `X`'s own reader goes with it.
	for (i, report) in reports.iter().enumerate().skip(1) {
		assert!(report.seamless(), "R{i}: {report:?}");
	}
	assert_eq!(mesh.doubled(), []);
}

/// One seeded case of [`failures_at_one_instant`]: a ring of `relays` with as many chords,
/// links priced 1 to 4, one encoder pushing to two relays, and a reader on every relay.
/// At one instant two relays die and two more links close. Once settled, every surviving
/// relay that still reaches an ingest shows the claim at the cost the links left add up
/// to, and its reader read on in the broadcast it had, every group once; every other one
/// let the claim go. Returns the longest stall.
async fn failures(seed: u64, relays: usize) -> Duration {
	let mut rng = Rng(seed);
	let mut mesh = Mesh::new(relays);
	let mut pairs = BTreeSet::new();
	for i in 0..relays {
		pairs.insert((i, (i + 1) % relays));
	}
	while pairs.len() < relays * 2 {
		let (a, b) = (rng.pick(relays), rng.pick(relays));
		if a != b && !pairs.contains(&(b, a)) {
			pairs.insert((a, b));
		}
	}
	for &(a, b) in &pairs {
		mesh.join(a, b, Join::peer(rng.range(1, 4))).await;
	}
	let ingests = [0, relays / 2];
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let mut pushes = Vec::new();
	for &ingest in &ingests {
		pushes.push(source.push(&mesh.relays[ingest], lite07(), 1).await);
	}
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let mut dead = BTreeSet::new();
	while dead.len() < 2 {
		dead.insert(rng.pick(relays));
	}
	let since = kio::clock::now();
	for &relay in &dead {
		mesh.kill(relay);
		if let Some(i) = ingests.iter().position(|ingest| *ingest == relay) {
			cut(&pushes[i]);
		}
	}
	let mut closed = 0;
	while closed < 2 {
		let (a, b) = (rng.pick(relays), rng.pick(relays));
		if !dead.contains(&a) && !dead.contains(&b) && mesh.neighbours(a).contains(&b) {
			mesh.cut(a, b);
			closed += 1;
		}
	}
	quiesce().await;

	let alive: Vec<_> = ingests.iter().filter(|ingest| !dead.contains(ingest)).collect();
	let reach: Vec<_> = alive.iter().map(|&&ingest| mesh.distances(ingest)).collect();
	let mut worst = Duration::ZERO;
	for (i, reader) in readers.iter_mut().enumerate() {
		if dead.contains(&i) {
			continue;
		}
		let cost = reach
			.iter()
			.filter_map(|distances| distances[i])
			.min()
			.map(|cost| cost + 1);
		let shown = shown(&mesh.relays[i]).await;
		let case = format!("seed {seed:#x}, dead {dead:?}, R{i}");
		match cost {
			Some(cost) => {
				let shown = shown.unwrap_or_else(|| panic!("{case}: shows nothing"));
				assert!(shown.available && shown.cost == cost, "{case}: {shown:?}, want {cost}");
				let report = reader.report(since);
				measured(&format!("failures at one instant, {case}"), &report);
				assert!(report.seamless(), "{case}: {report:?}");
				worst = worst.max(report.stall);
			}
			None => assert_eq!(shown, None, "{case}: let go"),
		}
	}
	// Pinned, not endorsed, until [the resend fix](/quest/m2/multicast/resend.md): a route
	// moving behind an unchanged next hop sends that hop the newest group again, once.
	let doubled = mesh.doubled();
	eprintln!("measured failures at one instant, seed {seed:#x}: groups sent twice {doubled:?}");
	let links: BTreeSet<_> = doubled.iter().map(|&(from, to, _, _)| (from, to)).collect();
	assert!(
		links.len() == doubled.len() && doubled.iter().all(|doubled| doubled.3 == 2),
		"seed {seed:#x}: {doubled:?}"
	);
	worst
}

/// [`failures`] over seeded meshes of 12 relays, past the three relays and one session
/// close the exhaustive check reaches.
#[kio::test]
async fn failures_at_one_instant() {
	for seed in 0..8 {
		let worst = failures(0x6d6f71 + seed, 12).await;
		eprintln!("measured failures at one instant, seed {seed}: worst stall {worst:?}");
	}
}

/// One encoder pushes to `X` (`R0`) at cost 1 and `Y` (`R1`), and `A` (`R2`) links to `X` at
/// 1 and `Y` at 2, with `B` (`R3`) reading through `A`. The link from `X` to `A` closes:
/// `A` moves onto `Y`, and `B` keeps `A` as its next hop while its route moves from `X`
/// to `Y`. A lite-07 request names the relay it routes to, so `B` asks `A` anew, from the
/// newest group it handed out: a range cannot start past a group whose end it does not
/// know. That group crosses `A`-`B` twice. Pinned, not endorsed, until
/// [the resend fix](/quest/m2/multicast/resend.md).
#[kio::test]
async fn a_route_moving_behind_one_next_hop_resends_a_group() {
	let mut mesh = Mesh::new(4);
	mesh.join(0, 2, Join::peer(1)).await;
	mesh.join(1, 2, Join::peer(2)).await;
	mesh.join(2, 3, Join::peer(1)).await;
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let _x = source.push(&mesh.relays[0], lite07(), 1).await;
	let _y = source.push(&mesh.relays[1], lite07(), 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	mesh.cut(0, 2);
	quiesce().await;
	for (i, report) in reports(&mut readers, since, "X-A cut").iter().enumerate().skip(2) {
		assert!(report.seamless(), "R{i}: {report:?}");
	}
	let doubled = mesh.doubled();
	assert!(
		matches!(doubled.as_slice(), [(2, 3, _, 2)]),
		"one group crosses A-B twice: {doubled:?}"
	);
}

/// A ring of six with a publisher on `R0` and a reader on every relay. A link from `R0` to
/// `R3` opens, the cheapest way for `R2`, `R3`, and `R4`, then closes, then opens again:
/// each move overlaps the old route with the new until the new delivers. Every reader
/// reads on in the broadcast it had, and no link carries a group twice for it.
#[kio::test]
async fn moving_routes_carry_no_group_twice_over_a_link() {
	let mut mesh = ring(6).await;
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let _push = source.push(&mesh.relays[0], lite07(), 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;

	let since = kio::clock::now();
	for _ in 0..2 {
		mesh.join(0, 3, Join::peer(1)).await;
		kio::clock::sleep(Duration::from_secs(2)).await;
		mesh.cut(0, 3);
		kio::clock::sleep(Duration::from_secs(2)).await;
	}
	quiesce().await;
	for (i, report) in reports(&mut readers, since, "R0-R3 flapping").iter().enumerate() {
		assert!(report.seamless(), "R{i}: {report:?}");
	}
	assert_eq!(mesh.doubled(), []);
}

//! Routing domains and marks: three meshes joined by scoped links, and the two link marks
//! requirement 3 asks for, an edge marking its core links and a leaf marking every link.

use super::{Feed, Join, Mesh, Reader, Source, Watch, measured, shown, shown_at};
use crate::path_routes::{lite07, quiesce};
use crate::support::Kind;

use moq_net::origin;

/// A scoped link at cost 1: the claim crosses, announced again by the receiving relay.
fn scoped() -> Join {
	Join {
		cost: 1,
		link: origin::Link::Peer,
		scoped: true,
	}
}

/// Three meshes of two relays each, `M0` = `R0`-`R1`, `M1` = `R2`-`R3`, `M2` = `R4`-`R5`,
/// joined by scoped links `R1`->`R2` and `R3`->`R4`, and in a ring also `R5`->`R0`.
async fn meshes(ring: bool) -> Mesh {
	let mut mesh = Mesh::new(6);
	for i in [0, 2, 4] {
		mesh.join(i, i + 1, Join::peer(1)).await;
	}
	mesh.join(1, 2, scoped()).await;
	mesh.join(3, 4, scoped()).await;
	if ring {
		mesh.join(5, 0, scoped()).await;
	}
	mesh
}

/// [`meshes`], with a publisher on `R0`. Every relay shows its claim under its Instance and
/// reads it, nothing changes once settled (no claim circles the ring of meshes), and no
/// link carries a group twice. Ended at its source, the claim ends everywhere, never
/// left to go offline.
async fn joined_meshes(ring: bool) {
	let mesh = meshes(ring).await;
	let source = Source::new(1, None);
	let _feed = Feed::start(vec![source.track.clone()], "P");
	let _push = source.push(&mesh.relays[0], lite07(), 1).await;
	quiesce().await;
	let mut watched = Vec::new();
	for relay in &mesh.relays {
		watched.push(Watch::new(relay).await);
	}
	let mut readers = super::readers(&mesh).await;
	let since = kio::clock::now();
	quiesce().await;
	for (i, relay) in mesh.relays.iter().enumerate() {
		let shown = shown(relay).await.expect("shown");
		assert!(shown.available, "ring {ring}: R{i}: {shown:?}");
		assert_eq!(shown.instance, source.route.instance(), "ring {ring}: R{i}");
		let report = readers[i].report(since);
		assert!(
			report.seamless() && report.stall.is_zero(),
			"ring {ring}: R{i}: {report:?}"
		);
		let seen = watched[i].seen();
		assert_eq!(seen, [], "ring {ring}: R{i} settled");
	}
	assert_eq!(mesh.doubled(), [], "ring {ring}");

	let ended = kio::clock::now();
	source.route.withdraw();
	quiesce().await;
	for (i, watch) in watched.iter_mut().enumerate() {
		let seen = watch.seen();
		let kinds: Vec<_> = seen.iter().map(|seen| seen.kind).collect();
		assert_eq!(kinds, [Kind::End], "ring {ring}: R{i} ends once: {seen:?}");
		eprintln!(
			"measured scoped meshes (ring {ring}), withdrawn at R0, R{i} ended after {:?}",
			seen[0].at.saturating_duration_since(ended)
		);
	}
}

#[kio::test]
async fn a_chain_of_meshes_carries_a_claim_and_its_end() {
	joined_meshes(false).await;
}

#[kio::test]
async fn a_ring_of_meshes_carries_a_claim_and_its_end_without_circling() {
	joined_meshes(true).await;
}

/// A ring of meshes, a publisher on `R0`, and one scoped link closes: the meshes past it
/// reach the claim the other way round. Every reader reads on in the broadcast it had.
#[kio::test]
async fn a_ring_of_meshes_reaches_round_a_closed_scoped_link() {
	for (a, b) in [(5, 0), (1, 2)] {
		let mut mesh = meshes(true).await;
		let source = Source::new(1, None);
		let _feed = Feed::start(vec![source.track.clone()], "P");
		let _push = source.push(&mesh.relays[0], lite07(), 1).await;
		quiesce().await;
		let mut readers = super::readers(&mesh).await;

		let since = kio::clock::now();
		mesh.cut(a, b);
		quiesce().await;
		for (i, reader) in readers.iter_mut().enumerate() {
			let report = reader.report(since);
			measured(&format!("ring of meshes, scoped R{a}-R{b} cut, reader R{i}"), &report);
			assert!(report.seamless(), "R{a}-R{b}: R{i}: {report:?}");
			let shown = shown(&mesh.relays[i]).await.expect("shown");
			assert!(shown.available, "R{a}-R{b}: R{i}: {shown:?}");
		}
	}
}

/// An edge `E` (`R2`) dials two cores `C1` (`R0`) and `C2` (`R1`) that never link to each
/// other, marking both links `upstream` when `marked`. `Q` publishes `live` to `C1`, and
/// `L`, a client of `E`, publishes `local`. Marked, `E` passes its client's claim to both
/// cores and `C1`'s claim to its own readers, but never one core's claim to the other.
/// Unmarked, `C2` reaches `Q` through `E`, which is what the mark is for.
#[kio::test]
async fn an_edge_passes_its_clients_and_its_cores_but_never_core_to_core() {
	for marked in [true, false] {
		let mut mesh = Mesh::new(3);
		let link = match marked {
			true => origin::Link::Upstream,
			false => origin::Link::Peer,
		};
		for core in [0, 1] {
			let join = Join {
				cost: 1,
				link,
				scoped: false,
			};
			mesh.join(2, core, join).await;
		}
		let q = Source::new(1, None);
		let _q_feed = Feed::start(vec![q.track.clone()], "Q");
		let _q_push = q.push(&mesh.relays[0], lite07(), 1).await;
		let l = Source::on(crate::route_change::produce_origin(2), "local", None);
		let _l_push = l.push(&mesh.relays[2], lite07(), 1).await;
		quiesce().await;

		for core in [0, 1] {
			let local = shown_at(&mesh.relays[core], "local").await;
			assert!(
				local.is_some_and(|local| local.available),
				"marked {marked}: C{core} has L"
			);
		}
		let mut reader = Reader::new(&mesh.relays[2]);
		kio::clock::sleep(super::CADENCE * 10).await;
		assert_eq!(reader.tag().as_deref(), Some("Q"), "marked {marked}: E reads Q");
		let across = shown(&mesh.relays[1]).await;
		assert_eq!(across.is_some(), !marked, "marked {marked}: C2 sees Q: {across:?}");
	}
}

/// A leaf `L` (`R2`), a relay that never forwards, links to relays `A` (`R0`) and `B`
/// (`R1`), which never link to each other. `L`'s host publishes `cam`, and `Q` publishes
/// `live` to `A`. `L` passes `cam` to both and reads `live` itself, but never passes `live`
/// to `B`. A relay that forwards in its place does.
#[kio::test]
async fn a_leaf_advertises_only_what_its_host_serves() {
	for leaf in [true, false] {
		let mut mesh = Mesh::new(3);
		mesh.relays[2] = mesh.relays[2].clone().with_proxy(!leaf);
		for relay in [0, 1] {
			mesh.join(2, relay, Join::peer(1)).await;
		}
		let cam = Source::on(mesh.relays[2].clone(), "cam", None);
		let _cam_feed = Feed::start(vec![cam.track.clone()], "L");
		let q = Source::new(1, None);
		let _q_feed = Feed::start(vec![q.track.clone()], "Q");
		let _q_push = q.push(&mesh.relays[0], lite07(), 1).await;
		quiesce().await;

		for relay in [0, 1] {
			let shown = shown_at(&mesh.relays[relay], "cam").await;
			assert!(
				shown.is_some_and(|shown| shown.available),
				"leaf {leaf}: R{relay} has cam"
			);
		}
		let mut reader = Reader::new(&mesh.relays[2]);
		kio::clock::sleep(super::CADENCE * 10).await;
		assert_eq!(reader.tag().as_deref(), Some("Q"), "leaf {leaf}: L reads Q");
		let across = shown(&mesh.relays[1]).await;
		assert_eq!(across.is_some(), !leaf, "leaf {leaf}: B sees Q: {across:?}");
	}
}

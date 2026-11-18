//! Two publishers claiming one path from opposite sides of a mesh, each under its own
//! Instance: every relay uses its nearest, no request loops, and no reader is moved from
//! one Instance to the other mid-stream. moq-transport and lite-06 publishers carry no
//! Instance, so each relay that takes their announcement mints one, and the same holds.

use super::{Feed, Join, Mesh, Source, measured, readers, ring, shown};
use crate::path_routes::{lite07, quiesce};

use moq_net::Version;

/// A 3x3 grid of relays, `R0` at one corner and `R8` at the other, priced unevenly and
/// with a diagonal from `R2` to `R4`, so the nearest of two corners differs per relay.
const GRID: [(usize, usize, u64); 13] = [
	(0, 1, 1),
	(1, 2, 3),
	(3, 4, 2),
	(4, 5, 1),
	(6, 7, 1),
	(7, 8, 2),
	(0, 3, 2),
	(3, 6, 1),
	(1, 4, 1),
	(4, 7, 3),
	(2, 5, 1),
	(5, 8, 2),
	(2, 4, 2),
];

async fn grid() -> Mesh {
	let mut mesh = Mesh::new(9);
	for (a, b, cost) in GRID {
		mesh.join(a, b, Join::peer(cost)).await;
	}
	mesh
}

/// Which publisher each relay should read: `Some(0)` for the one on `R0`, `Some(1)` for
/// the one on `R8`, and `None` at a tie. Both push at cost 1.
fn nearest(mesh: &Mesh) -> Vec<Option<usize>> {
	let [a, b] = [mesh.distances(0), mesh.distances(8)];
	(a.iter().zip(&b))
		.map(|(a, b)| match (a, b) {
			(Some(a), Some(b)) if a < b => Some(0),
			(Some(a), Some(b)) if b < a => Some(1),
			_ => None,
		})
		.collect()
}

/// `A` on `R0` and `B` on `R8` of [`grid`], each its own minted Instance. Every relay
/// shows and reads its nearest at the cost the links add up to, and no link carries a
/// group twice. The link from `R0` to `R1` closes, which leaves `A` farther than `B` from
/// `R1` and `R4`: both show `B` now, a restart, while their readers read on in `A`'s
/// broadcast over the longer route, never moved onto `B`.
#[kio::test]
async fn opposite_claims_each_serve_their_nearest() {
	let mut mesh = grid().await;
	let sources = [Source::new(1, None), Source::new(2, None)];
	let feeds = [
		Feed::start(vec![sources[0].track.clone()], "A"),
		Feed::start(vec![sources[1].track.clone()], "B"),
	];
	let _pushes = [
		sources[0].push(&mesh.relays[0], lite07(), 1).await,
		sources[1].push(&mesh.relays[8], lite07(), 1).await,
	];
	quiesce().await;
	let mut readers = readers(&mesh).await;
	let instances = sources.each_ref().map(|source| source.route.instance());
	let near = nearest(&mesh);
	let distances = [mesh.distances(0), mesh.distances(8)];
	for (i, reader) in readers.iter_mut().enumerate() {
		let shown = shown(&mesh.relays[i]).await.expect("shown");
		let cost = 1 + distances[0][i].unwrap().min(distances[1][i].unwrap());
		assert_eq!(shown.cost, cost, "R{i} shows its nearest: {shown:?}");
		if let Some(near) = near[i] {
			assert_eq!(shown.instance, instances[near], "R{i}");
			assert_eq!(
				reader.tag().as_deref(),
				Some(["A", "B"][near]),
				"R{i} reads its nearest"
			);
		}
	}
	let written = feeds.iter().map(Feed::written).max().unwrap();
	assert_eq!(mesh.doubled(written), [], "no group crosses a link twice");

	let since = kio::clock::now();
	mesh.cut(0, 1);
	quiesce().await;
	let after = nearest(&mesh);
	for (i, reader) in readers.iter_mut().enumerate() {
		let report = reader.report(since);
		measured(&format!("opposite claims, R0-R1 cut, reader R{i}"), &report);
		assert!(report.seamless(), "R{i}: {report:?}");
		if let (Some(before), Some(now)) = (near[i], after[i])
			&& before != now
		{
			let shown = shown(&mesh.relays[i]).await.expect("shown");
			assert_eq!(shown.instance, instances[now], "R{i} shows its new nearest");
			assert_eq!(reader.tag().as_deref(), Some(["A", "B"][before]), "R{i} reads on");
		}
	}
	assert_eq!(after[1], Some(1), "the cut moves R1's nearest");
	assert_eq!(after[4], Some(1), "the cut moves R4's nearest");
}

/// Two publishers announce [`super::PATH`] to opposite relays of a ring of six over
/// `versions`, neither carrying an Instance. Each relay reads its nearest; the ring link
/// from `R0` to `R1` closes, leaving `R1` nearer `R3`'s, and `R1`'s reader stays in the
/// broadcast it had. Then the publisher on `R0` goes: the readers on its announcement
/// restart onto the other, with nothing spliced.
async fn unversioned(versions: [Version; 2]) {
	let mut mesh = ring(6).await;
	let sources = [Source::new(1, None), Source::new(2, None)];
	let _feeds = [
		Feed::start(vec![sources[0].track.clone()], "A"),
		Feed::start(vec![sources[1].track.clone()], "B"),
	];
	let a_push = sources[0].push(&mesh.relays[0], versions[0], 1).await;
	let _b_push = sources[1].push(&mesh.relays[3], versions[1], 1).await;
	quiesce().await;
	let mut readers = readers(&mesh).await;
	let nearest = ["A", "A", "B", "B", "B", "A"];
	let mut minted = Vec::new();
	for (i, reader) in readers.iter_mut().enumerate() {
		assert_eq!(reader.tag().as_deref(), Some(nearest[i]), "{versions:?}: R{i}");
		minted.push(shown(&mesh.relays[i]).await.expect("shown").instance);
	}
	for (i, instance) in minted.iter().enumerate() {
		let same = (minted.iter().zip(nearest)).all(|(other, tag)| (other == instance) == (tag == nearest[i]));
		assert!(same, "{versions:?}: one Instance per announcement: {minted:?}");
	}

	let since = kio::clock::now();
	mesh.cut(0, 1);
	quiesce().await;
	for (i, reader) in readers.iter_mut().enumerate() {
		let report = reader.report(since);
		measured(&format!("{versions:?}, R0-R1 cut, reader R{i}"), &report);
		assert!(report.seamless(), "{versions:?}: R{i}: {report:?}");
		assert_eq!(reader.tag().as_deref(), Some(nearest[i]), "{versions:?}: R{i} reads on");
	}
	let r1 = shown(&mesh.relays[1]).await.expect("shown");
	assert_eq!(r1.instance, minted[3], "{versions:?}: R1 shows R3's announcement now");

	let since = kio::clock::now();
	crate::path_routes::cut(&a_push);
	quiesce().await;
	for (i, reader) in readers.iter_mut().enumerate() {
		let report = reader.report(since);
		measured(&format!("{versions:?}, A lost, reader R{i}"), &report);
		assert!(report.clean(), "{versions:?}: R{i}: {report:?}");
		assert_eq!(reader.tag().as_deref(), Some("B"), "{versions:?}: R{i} ends on B");
		let restarts = usize::from(nearest[i] == "A");
		assert_eq!(report.runs.len(), 1 + restarts, "{versions:?}: R{i}: {report:?}");
	}
}

#[kio::test]
async fn moq_transport_publishers_each_serve_their_nearest() {
	let version: Version = "moq-transport-22".parse().unwrap();
	unversioned([version, version]).await;
}

#[kio::test]
async fn lite06_publishers_each_serve_their_nearest() {
	let version: Version = "moq-lite-06".parse().unwrap();
	unversioned([version, version]).await;
}

#[kio::test]
async fn a_moq_transport_and_a_lite06_publisher_each_serve_their_nearest() {
	unversioned(["moq-transport-22".parse().unwrap(), "moq-lite-06".parse().unwrap()]).await;
}

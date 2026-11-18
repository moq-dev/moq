//! Failures: a detour learned only after the cut.

use super::{Feed, Reader, Source, measured, readers, ring, shown};
use crate::path_routes::{lite07, quiesce};

#[kio::test]
async fn probe_detour_after_cut() {
	let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).try_init();
	for (count, r1r2) in [(4, 1)] {
		let mut mesh = super::Mesh::new(count);
		for i in 0..count {
			let cost = if i == 1 { r1r2 } else { 1 };
			mesh.join(i, (i + 1) % count, super::Join::peer(cost)).await;
		}
		let source = Source::new(1, None);
		let _feed = Feed::start(vec![source.track.clone()], "A");
		let _push = source.push(&mesh.relays[0], lite07(), 1).await;
		quiesce().await;
		let mut readers = readers(&mesh).await;
		eprintln!("before: {:?}", shown(&mesh.relays[1]).await);
		let since = kio::clock::now();
		eprintln!("=== CUT at {:?}", since.saturating_duration_since(kio::clock::Instant::ZERO));
		mesh.cut(0, 1);
		crate::path_routes::land().await;
		eprintln!("landed: {:?}", shown(&mesh.relays[1]).await);
		kio::clock::sleep(std::time::Duration::from_secs(2)).await;
		eprintln!("=== END");
		quiesce().await;
		eprintln!("after: {:?}", shown(&mesh.relays[1]).await);
		for (i, reader) in readers.iter_mut().enumerate() {
			let report = reader.report(since);
			measured(&format!("ring {count} r1r2 {r1r2}, R0-R1 cut, reader R{i}"), &report);
		}
		let mut fresh = Reader::new(&mesh.relays[1]);
		kio::clock::sleep(std::time::Duration::from_secs(1)).await;
		eprintln!("fresh reader got {} frames", fresh.got().len());
		eprintln!("groups 2->1 {} 0->1 {} 1->2 {}", mesh.groups(2, 1), mesh.groups(0,1), mesh.groups(1,2));
	}
}

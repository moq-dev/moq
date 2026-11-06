//! Catalog parse and serialization cost across tracks and distinct init entries.

use std::hint::black_box;
use std::str::FromStr;

use criterion::{Criterion, criterion_group, criterion_main};
use moq_msf::Catalog;

fn catalog(tracks: usize, entries: usize) -> String {
	let suffix = "A".repeat(16 * 1024 - 8);
	let init_data_list = (0..entries)
		.map(
			|id| serde_json::json!({ "id": format!("init{id}"), "type": "inline", "data": format!("{id:08x}{suffix}") }),
		)
		.collect::<Vec<_>>();
	let tracks = (0..tracks)
		.map(|index| {
			serde_json::json!({
				"name": format!("track{index}"), "packaging": "legacy",
				"isLive": true, "role": "video", "initRef": format!("init{}", index % entries)
			})
		})
		.collect::<Vec<_>>();
	serde_json::json!({ "version": "draft-01", "initDataList": init_data_list, "tracks": tracks }).to_string()
}

fn bench(c: &mut Criterion) {
	for tracks in [1, 16, 256] {
		for entries in [1, 4, 16] {
			let json = catalog(tracks, entries);
			let parsed = Catalog::<()>::from_str(&json).unwrap();
			let id = format!("{tracks}tracks/{entries}entries");
			c.bench_function(&format!("msf_parse/{id}"), |b| {
				b.iter(|| black_box(Catalog::<()>::from_str(black_box(&json)).unwrap()))
			});
			c.bench_function(&format!("msf_serialize/{id}"), |b| {
				b.iter(|| black_box(black_box(&parsed).to_json().unwrap()))
			});
		}
	}
}

criterion_group!(benches, bench);
criterion_main!(benches);

//! Catalog parse and serialization cost across tracks and distinct init entries.
//!
//! Run with `cargo bench -p moq-msf --bench catalog`.

use std::fmt;
use std::str::FromStr;

use moq_msf::Catalog;
use swole::bench;

/// A catalog's shape, named `<tracks>tracks/<entries>entries`.
#[derive(Clone, Copy)]
struct Case {
	tracks: usize,
	entries: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}tracks/{}entries", self.tracks, self.entries)
	}
}

fn cases() -> Vec<Case> {
	[1, 16, 256]
		.into_iter()
		.flat_map(|tracks| [1, 4, 16].map(|entries| Case { tracks, entries }))
		.collect()
}

fn json(case: Case) -> String {
	let suffix = "A".repeat(16 * 1024 - 8);
	let init_data_list = (0..case.entries)
		.map(
			|id| serde_json::json!({ "id": format!("init{id}"), "type": "inline", "data": format!("{id:08x}{suffix}") }),
		)
		.collect::<Vec<_>>();
	let tracks = (0..case.tracks)
		.map(|index| {
			serde_json::json!({
				"name": format!("track{index}"), "packaging": "legacy",
				"isLive": true, "role": "video", "initRef": format!("init{}", index % case.entries)
			})
		})
		.collect::<Vec<_>>();
	serde_json::json!({ "version": "draft-01", "initDataList": init_data_list, "tracks": tracks }).to_string()
}

fn parsed(case: Case) -> Catalog<()> {
	Catalog::from_str(&json(case)).unwrap()
}

#[bench(args = cases(), setup = json)]
fn msf_parse(json: &str) -> Catalog<()> {
	Catalog::from_str(json).unwrap()
}

#[bench(args = cases(), setup = parsed)]
fn msf_serialize(catalog: &Catalog<()>) -> String {
	catalog.to_json().unwrap()
}

fn main() {
	swole::main();
}

//! Cost of decoding window frames into typed records, swept over records per header.
//!
//! `header` decodes one group header holding `records` records into a fresh decoder; `push`
//! decodes `records` push frames after such a header. The frames are built outside the measured
//! section.
//!
//! Run with `cargo bench -p moq-json --bench window`.
#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "push times itself to leave out the header its group must decode first"
)]

use std::time::{Duration, Instant};

use moq_json::window::{Decoder, consumer};
use swole::{Iterations, bench};

/// A stats-shaped record: flat counters plus one nested object, nothing owned on the heap.
#[derive(serde::Deserialize)]
#[allow(dead_code, reason = "the bench only measures deserializing it")]
struct Record {
	bytes: u64,
	frames: u64,
	groups: u64,
	subscriptions: u64,
	nested: Nested,
}

#[derive(serde::Deserialize)]
#[allow(dead_code, reason = "the bench only measures deserializing it")]
struct Nested {
	bytes: u64,
	frames: u64,
}

const RECORDS: [usize; 4] = [1, 16, 256, 4096];

fn record(index: usize) -> serde_json::Value {
	serde_json::json!({
		"bytes": index * 1500,
		"frames": index,
		"groups": index / 30,
		"subscriptions": 3,
		"nested": { "bytes": index * 1200, "frames": index },
	})
}

/// A header of `records` records and the `records` pushes that follow it.
struct Window {
	decoder: Decoder<Record>,
	header: Vec<u8>,
	pushes: Vec<Vec<u8>>,
}

fn window(records: usize) -> Window {
	let header = serde_json::to_vec(&serde_json::json!({
		"offset": 0,
		"records": (0..records).map(record).collect::<Vec<_>>(),
	}))
	.unwrap();
	let pushes = (records..2 * records)
		.map(|index| serde_json::to_vec(&serde_json::json!({ "push": record(index) })).unwrap())
		.collect();
	Window {
		decoder: Decoder::new(consumer::Config::default()),
		header,
		pushes,
	}
}

fn fresh(window: &mut Window) {
	window.decoder = Decoder::new(consumer::Config::default());
}

#[bench(args = RECORDS, setup = window, before = fresh, items = |records| records)]
fn header(window: &mut Window) {
	window.decoder.group().decode(&window.header).unwrap();
}

/// A push needs its group positioned by the header first, and the group borrows its decoder, so
/// no step outside the call can leave one ready: this times the pushes alone.
#[bench(args = RECORDS, setup = window, items = |records| records)]
fn push(window: &Window, iterations: Iterations) -> Duration {
	let mut elapsed = Duration::ZERO;
	for _ in iterations {
		let mut decoder = Decoder::<Record>::new(consumer::Config::default());
		let mut group = decoder.group();
		group.decode(&window.header).unwrap();
		let start = Instant::now();
		for push in &window.pushes {
			group.decode(push).unwrap();
		}
		elapsed += start.elapsed();
	}
	elapsed
}

fn main() {
	swole::main();
}

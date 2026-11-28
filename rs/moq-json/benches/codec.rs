//! CPU benchmarks for the raw codec operations behind `moq-json`, with no Producer/Consumer or
//! moq-net framing in the loop. Each measures one step of the per-delta pipeline:
//!
//! 1. `encode_patch`  - generate a merge patch from the old value and the new one ([`diff`]).
//! 2. `decode_patch`  - apply a merge patch to reconstruct the new value (`json_patch::merge`), with
//!    a consuming variant (`merge_owned`) for comparison.
//! 3. `producer`      - the full producer step: encode patch, serialize.
//! 4. `consumer`      - the full consumer step: parse, merge.
//!
//! Compression is moq-net's, so it is benchmarked there.
//!
//! Run with `cargo bench -p moq-json --features bench --bench codec`.

use moq_json::bench::diff;
use serde_json::{Map, Value, json};
use swole::bench;

/// One second of telemetry: a big static core plus a few moving numbers. Most fields change a little
/// each tick, so the delta is small but touches much of the document.
fn telemetry(tick: u64) -> Value {
	let t = tick as f64;
	let lat = 37.7749 + (t * 0.0001).sin() * 0.01;
	let lon = -122.4194 + (t * 0.0001).cos() * 0.01;

	json!({
		"device": {
			"id": "veh-4417-a2",
			"model": "Sentinel X2",
			"firmware": "4.18.2-rc1",
			"serial": "SNX2-0000-4417-A2C9",
			"region": "us-west-2",
			"fleet": "logistics-prod",
			"tags": ["cold-chain", "long-haul", "priority"],
		},
		"config": {
			"sample_hz": 1,
			"upload_hz": 1,
			"geofence": "bay-area",
			"thresholds": { "temp_c": 8.0, "humidity": 85, "shock_g": 3.5, "battery_pct": 15 },
			"contacts": ["ops@example.com", "fleet@example.com"],
		},
		"ts": 1_700_000_000 + tick,
		"uptime_s": tick,
		"location": {
			"lat": (lat * 1e6).round() / 1e6,
			"lon": (lon * 1e6).round() / 1e6,
			"alt_m": 12 + (tick % 5),
			"heading": (tick * 7) % 360,
			"speed_kph": 40 + (tick % 25),
			"fix": "3d",
			"sats": 9 + (tick % 3),
		},
		"sensors": {
			"temp_c": ((4.0 + (t * 0.05).sin() * 1.5) * 100.0).round() / 100.0,
			"humidity": 60 + (tick % 10),
			"shock_g": (((t * 0.3).sin().abs()) * 100.0).round() / 100.0,
			"door_open": tick.is_multiple_of(30),
		},
		"power": {
			"battery_pct": 100 - (tick / 6) % 100,
			"charging": false,
			"voltage_mv": 12_400 - (tick % 50) as i64,
			"current_ma": 850 + (tick % 120) as i64,
		},
		"network": {
			"rssi_dbm": -70 - (tick % 15) as i64,
			"type": "lte",
			"bytes_up": 1_024 * tick,
			"bytes_down": 256 * tick,
			"latency_ms": 35 + (tick % 40),
		},
		"counters": {
			"events": tick,
			"errors": tick / 50,
			"reconnects": tick / 120,
		},
	})
}

/// A large mostly-static document: a big config blob that never changes plus a few counters that
/// tick. The delta is tiny relative to the document.
fn big_static(tick: u64) -> Value {
	let routes: Vec<Value> = (0..80)
		.map(|i| {
			json!({
				"id": format!("route-{i:04}"),
				"cidr": format!("10.{}.{}.0/24", i / 16, i % 16),
				"gateway": format!("10.0.{i}.1"),
				"metric": 100 + i,
				"enabled": true,
				"tags": ["prod", "egress", "monitored"],
			})
		})
		.collect();

	json!({
		"meta": { "version": "9.2.1", "node": "edge-router-77", "region": "us-east-1" },
		"routes": routes,
		"counters": {
			"packets_in": 1_000_000 + tick * 137,
			"packets_out": 990_000 + tick * 131,
			"errors": tick / 7,
			"uptime_s": tick,
		},
	})
}

/// RFC 7396 merge that consumes the patch, moving values into the target instead of cloning them like
/// `json_patch::merge` (which takes `&Value`). Used to test whether a consuming merge decodes faster.
fn merge_owned(target: &mut Value, patch: Value) {
	let Value::Object(patch) = patch else {
		*target = patch;
		return;
	};
	if !target.is_object() {
		*target = Value::Object(Map::new());
	}
	let map = target.as_object_mut().unwrap();
	for (key, value) in patch {
		if value.is_null() {
			map.remove(&key);
		} else {
			merge_owned(map.entry(key).or_insert(Value::Null), value);
		}
	}
}

/// One workload reduced to a single old -> new transition and the artifacts each op needs: the patch
/// value and the serialized snapshot, patch, and new value.
struct Fixture {
	old: Value,
	new: Value,
	patch: Value,
	snapshot_bytes: Vec<u8>,
	patch_bytes: Vec<u8>,
	// The full new value: what a snapshot-only stream (no merge patch) would send each tick, the fair
	// baseline for the delta path.
	new_bytes: Vec<u8>,
}

/// The workloads, by name.
const FIXTURES: [&str; 2] = ["telemetry", "big_static"];

fn fixture(name: &str) -> Fixture {
	let make = match name {
		"telemetry" => telemetry,
		"big_static" => big_static,
		_ => unreachable!("unknown fixture {name}"),
	};
	let old = make(0);
	let new = make(1);
	let patch = diff(&old, &new).patch;
	let snapshot_bytes = serde_json::to_vec(&old).unwrap();
	let patch_bytes = serde_json::to_vec(&patch).unwrap();
	let new_bytes = serde_json::to_vec(&new).unwrap();

	Fixture {
		old,
		new,
		patch,
		snapshot_bytes,
		patch_bytes,
		new_bytes,
	}
}

/// The size of the fixture's snapshot, each benchmark's throughput.
fn snapshot_len(name: &str) -> usize {
	fixture(name).snapshot_bytes.len()
}

/// 1. Generate a merge patch from the old and new values.
#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
fn encode_patch(f: &Fixture) -> moq_json::bench::Diff {
	diff(&f.old, &f.new)
}

/// 2. Apply a merge patch to reconstruct the new value, json_patch (borrowing) vs consuming merge.
mod decode_patch {
	use serde_json::Value;
	use swole::bench;

	use super::{FIXTURES, snapshot_len};

	/// The old value and the patch, fresh for each call.
	fn input(name: &str) -> (Value, Value) {
		let f = super::fixture(name);
		(f.old, f.patch)
	}

	#[bench(args = FIXTURES, setup = input, bytes = snapshot_len)]
	fn json_patch((mut current, patch): (Value, Value)) -> Value {
		::json_patch::merge(&mut current, &patch);
		current
	}

	#[bench(args = FIXTURES, setup = input, bytes = snapshot_len)]
	fn merge_owned((mut current, patch): (Value, Value)) -> Value {
		super::merge_owned(&mut current, patch);
		current
	}
}

/// The marshal each path pays per tick: a full `to_vec` of the document (snapshot-only) vs a `to_vec`
/// of just the patch (delta). The diff already walks the whole document, so this is the extra cost
/// the snapshot path carries that the delta path avoids.
mod marshal {
	use swole::bench;

	use super::{FIXTURES, Fixture, fixture};

	#[bench(args = FIXTURES, setup = fixture)]
	fn full(f: &Fixture) -> Vec<u8> {
		serde_json::to_vec(&f.new).unwrap()
	}

	#[bench(args = FIXTURES, setup = fixture)]
	fn patch(f: &Fixture) -> Vec<u8> {
		serde_json::to_vec(&f.patch).unwrap()
	}
}

/// Seeding the delta baseline at each snapshot, two ways. The encoder has already serialized the
/// document to produce the frame, so it can parse the baseline back out of those bytes
/// (`from_slice`) instead of walking the value a second time (`to_value`).
///
/// The encoder uses `from_slice` because it makes the baseline the emitted snapshot by construction,
/// which `to_value` cannot guarantee for a stateful `Serialize`. This measures what that costs: a
/// text parse in place of a second traversal, which is not automatically the cheaper of the two.
mod baseline {
	use serde_json::Value;
	use swole::bench;

	use super::{FIXTURES, Fixture, fixture, snapshot_len};

	#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
	fn from_slice(f: &Fixture) -> Value {
		serde_json::from_slice(&f.snapshot_bytes).unwrap()
	}

	#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
	fn to_value(f: &Fixture) -> Value {
		serde_json::to_value(&f.old).unwrap()
	}
}

/// The full producer step, head to head: the delta path (diff + serialize patch) vs the
/// snapshot-only path (serialize the whole document).
mod producer {
	use moq_json::bench::diff;
	use swole::bench;

	use super::{FIXTURES, Fixture, fixture, snapshot_len};

	#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
	fn merge(f: &Fixture) -> Vec<u8> {
		let patch = diff(&f.old, &f.new).patch;
		serde_json::to_vec(&patch).unwrap()
	}

	#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
	fn snapshot(f: &Fixture) -> Vec<u8> {
		serde_json::to_vec(&f.new).unwrap()
	}
}

/// The full consumer step, head to head: the delta path (parse patch + merge) vs the snapshot-only
/// path (parse the whole document). The snapshot path re-parses the entire document every tick; the
/// delta path parses only the patch.
mod consumer {
	use serde_json::Value;
	use swole::bench;

	use super::{FIXTURES, Fixture, fixture, snapshot_len};

	/// The old value, fresh for each call, and the patch's bytes.
	fn input(name: &str) -> (Value, Vec<u8>) {
		let f = fixture(name);
		(f.old, f.patch_bytes)
	}

	#[bench(args = FIXTURES, setup = input, bytes = snapshot_len)]
	fn merge((mut current, patch_bytes): (Value, Vec<u8>)) -> Value {
		let patch: Value = serde_json::from_slice(&patch_bytes).unwrap();
		json_patch::merge(&mut current, &patch);
		current
	}

	#[bench(args = FIXTURES, setup = fixture, bytes = snapshot_len)]
	fn snapshot(f: &Fixture) -> Value {
		serde_json::from_slice(&f.new_bytes).unwrap()
	}
}

/// One small delta applied to a snapshot of `entries` entries, with and without a size budget.
mod snapshot_budget {
	use moq_json::snapshot::{Decoder, consumer};
	use serde_json::{Map, Value, json};
	use swole::bench;

	const ENTRIES: [usize; 3] = [100, 1000, 10000];

	fn decoder(entries: usize, max_size: Option<usize>) -> Decoder<Value> {
		let value: Map<String, Value> = (0..entries)
			.map(|i| (format!("path-{i}"), json!({"bytes": 123})))
			.collect();
		let mut config = consumer::Config::default();
		config.max_size = max_size;
		let mut decoder = Decoder::<Value>::new(config);
		decoder.snapshot(&serde_json::to_vec(&value).unwrap()).unwrap();
		decoder
	}

	fn unbounded_decoder(entries: usize) -> Decoder<Value> {
		decoder(entries, None)
	}

	fn bounded_decoder(entries: usize) -> Decoder<Value> {
		decoder(entries, Some(8 * 1024 * 1024))
	}

	#[bench(args = ENTRIES, setup = unbounded_decoder)]
	fn unbounded(decoder: &mut Decoder<Value>) {
		decoder.delta(br#"{"path-0":{"bytes":124}}"#).unwrap();
	}

	#[bench(args = ENTRIES, setup = bounded_decoder)]
	fn bounded(decoder: &mut Decoder<Value>) {
		decoder.delta(br#"{"path-0":{"bytes":124}}"#).unwrap();
	}
}

fn main() {
	swole::main();
}

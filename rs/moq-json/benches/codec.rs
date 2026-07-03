//! CPU benchmarks for the raw codec operations behind `moq-json`, with no Producer/Consumer or
//! moq-net framing in the loop. Each measures one step of the per-delta pipeline:
//!
//! 1. `encode_patch`  - generate a merge patch from the old value and the new one ([`diff`]).
//! 2. `decode_patch`  - apply a merge patch to reconstruct the new value (`json_patch::merge`), with
//!    a consuming variant (`merge_owned`) for comparison.
//! 3. `deflate`       - compress a delta into a warm DEFLATE window.
//! 4. `inflate`       - decompress a delta from a warm DEFLATE window.
//! 5. `producer`      - the full producer step: encode patch, serialize, deflate.
//! 6. `consumer`      - the full consumer step: inflate, parse, merge.
//!
//! Run with `cargo bench -p moq-json`.

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_flate::{Decoder, Encoder};
use moq_json::diff;
use serde_json::{Map, Value, json};

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
			"door_open": tick % 30 == 0,
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
/// value, the serialized snapshot and patch, and the compressed snapshot/delta slices (the delta is
/// compressed against a window already holding the snapshot, matching the real per-group stream).
struct Fixture {
	name: &'static str,
	old: Value,
	new: Value,
	patch: Value,
	snapshot_bytes: Vec<u8>,
	patch_bytes: Vec<u8>,
	snapshot_slice: Vec<u8>,
	delta_slice: Vec<u8>,
	// The full new value compressed against a window already holding the old snapshot: what a
	// snapshot-only stream (no merge patch) would send each tick, the fair baseline for the delta path.
	new_slice: Vec<u8>,
}

impl Fixture {
	fn new(name: &'static str, make: fn(u64) -> Value) -> Self {
		let old = make(0);
		let new = make(1);
		let patch = diff(&old, &new).patch;
		let snapshot_bytes = serde_json::to_vec(&old).unwrap();
		let patch_bytes = serde_json::to_vec(&patch).unwrap();
		let new_bytes = serde_json::to_vec(&new).unwrap();

		// Snapshot then delta (the merge-patch stream).
		let mut enc = Encoder::new();
		let snapshot_slice = enc.frame(&snapshot_bytes).to_vec();
		let delta_slice = enc.frame(&patch_bytes).to_vec();

		// Snapshot then the full new snapshot again (the snapshot-only stream).
		let mut enc = Encoder::new();
		enc.frame(&snapshot_bytes);
		let new_slice = enc.frame(&new_bytes).to_vec();

		Self {
			name,
			old,
			new,
			patch,
			snapshot_bytes,
			patch_bytes,
			snapshot_slice,
			delta_slice,
			new_slice,
		}
	}

	/// A DEFLATE encoder warmed with the snapshot, ready to compress the delta as the next frame.
	fn warm_encoder(&self) -> Encoder {
		let mut enc = Encoder::new();
		enc.frame(&self.snapshot_bytes);
		enc
	}

	/// A DEFLATE decoder warmed with the snapshot, ready to decompress the delta as the next frame.
	fn warm_decoder(&self) -> Decoder {
		let mut dec = Decoder::new();
		dec.frame(&self.snapshot_slice).unwrap();
		dec
	}
}

fn fixtures() -> Vec<Fixture> {
	vec![
		Fixture::new("telemetry", telemetry),
		Fixture::new("big_static", big_static),
	]
}

/// 1. Generate a merge patch from the old and new values.
fn encode_patch(c: &mut Criterion) {
	let mut group = c.benchmark_group("encode_patch");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.snapshot_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::from_parameter(f.name), f, |b, f| {
			b.iter(|| black_box(diff(&f.old, &f.new)));
		});
	}
	group.finish();
}

/// 2. Apply a merge patch to reconstruct the new value, json_patch (borrowing) vs consuming merge.
fn decode_patch(c: &mut Criterion) {
	let mut group = c.benchmark_group("decode_patch");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.snapshot_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::new("json_patch", f.name), f, |b, f| {
			b.iter_batched(
				|| f.old.clone(),
				|mut current| {
					json_patch::merge(&mut current, &f.patch);
					black_box(current);
				},
				BatchSize::SmallInput,
			);
		});
		group.bench_with_input(BenchmarkId::new("merge_owned", f.name), f, |b, f| {
			b.iter_batched(
				|| (f.old.clone(), f.patch.clone()),
				|(mut current, patch)| {
					merge_owned(&mut current, patch);
					black_box(current);
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// 3. Compress a delta into a warm DEFLATE window.
fn deflate(c: &mut Criterion) {
	let mut group = c.benchmark_group("deflate");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.patch_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::from_parameter(f.name), f, |b, f| {
			b.iter_batched(
				|| f.warm_encoder(),
				|mut enc| black_box(enc.frame(&f.patch_bytes)),
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// 4. Decompress a delta from a warm DEFLATE window.
fn inflate(c: &mut Criterion) {
	let mut group = c.benchmark_group("inflate");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.patch_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::from_parameter(f.name), f, |b, f| {
			b.iter_batched(
				|| f.warm_decoder(),
				|mut dec| black_box(dec.frame(&f.delta_slice).unwrap()),
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// The marshal each path pays per tick: a full `to_vec` of the document (snapshot-only) vs a `to_vec`
/// of just the patch (delta). The diff already walks the whole document, so this is the extra cost
/// the snapshot path carries that the delta path avoids.
fn marshal(c: &mut Criterion) {
	let mut group = c.benchmark_group("marshal");
	for f in &fixtures() {
		group.bench_with_input(BenchmarkId::new("full", f.name), f, |b, f| {
			b.iter(|| black_box(serde_json::to_vec(&f.new).unwrap()));
		});
		group.bench_with_input(BenchmarkId::new("patch", f.name), f, |b, f| {
			b.iter(|| black_box(serde_json::to_vec(&f.patch).unwrap()));
		});
	}
	group.finish();
}

/// The full producer step, head to head: the delta path (diff + serialize patch + deflate) vs the
/// snapshot-only path (serialize the whole document + deflate it), both into a warm window. The
/// snapshot path is charged for the full marshal it pays every tick.
fn producer(c: &mut Criterion) {
	let mut group = c.benchmark_group("producer");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.snapshot_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::new("merge", f.name), f, |b, f| {
			b.iter_batched(
				|| f.warm_encoder(),
				|mut enc| {
					let patch = diff(&f.old, &f.new).patch;
					let bytes = serde_json::to_vec(&patch).unwrap();
					black_box(enc.frame(&bytes));
				},
				BatchSize::SmallInput,
			);
		});
		group.bench_with_input(BenchmarkId::new("snapshot", f.name), f, |b, f| {
			b.iter_batched(
				|| f.warm_encoder(),
				|mut enc| {
					let bytes = serde_json::to_vec(&f.new).unwrap();
					black_box(enc.frame(&bytes));
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// The full consumer step, head to head: the delta path (inflate + parse patch + merge) vs the
/// snapshot-only path (inflate + parse the whole document). The snapshot path re-parses the entire
/// document every tick; the delta path parses only the patch.
fn consumer(c: &mut Criterion) {
	let mut group = c.benchmark_group("consumer");
	for f in &fixtures() {
		group.throughput(Throughput::Bytes(f.snapshot_bytes.len() as u64));
		group.bench_with_input(BenchmarkId::new("merge", f.name), f, |b, f| {
			b.iter_batched(
				|| (f.warm_decoder(), f.old.clone()),
				|(mut dec, mut current)| {
					let plain = dec.frame(&f.delta_slice).unwrap();
					let patch: Value = serde_json::from_slice(&plain).unwrap();
					json_patch::merge(&mut current, &patch);
					black_box(current);
				},
				BatchSize::SmallInput,
			);
		});
		group.bench_with_input(BenchmarkId::new("snapshot", f.name), f, |b, f| {
			b.iter_batched(
				|| f.warm_decoder(),
				|mut dec| {
					let plain = dec.frame(&f.new_slice).unwrap();
					let value: Value = serde_json::from_slice(&plain).unwrap();
					black_box(value);
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	encode_patch,
	decode_patch,
	deflate,
	inflate,
	marshal,
	producer,
	consumer
);
criterion_main!(benches);

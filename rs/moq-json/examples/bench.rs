//! CPU cost: JSON Merge Patch + DEFLATE vs DEFLATE-only streaming.
//!
//! Both approaches keep one shared DEFLATE window per group, so the wire bytes are comparable. The
//! question is how much CPU the merge-patch machinery (Value tree, diff, merge apply) adds on top of
//! just feeding the full snapshot through the same window every tick.
//!
//! Run with: `cargo run --release -p moq-json --example bench`

use std::hint::black_box;
use std::task::Poll;
use std::time::Instant;

use moq_flate::{Decoder, Encoder};
use moq_json::{ConsumerConfig, Producer, ProducerConfig};
use serde_json::{Map, Value, json};

/// One second of telemetry: a big static core plus a few moving numbers (mirrors examples/telemetry.rs).
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

// ---- A copy of the crate-private diff::diff so we can micro-time the merge-patch step. ----

fn diff(old: &Value, new: &Value) -> Value {
	if let (Value::Object(old), Value::Object(new)) = (old, new) {
		let mut patch = Map::new();
		diff_objects(old, new, &mut patch);
		Value::Object(patch)
	} else {
		new.clone()
	}
}

fn diff_objects(old: &Map<String, Value>, new: &Map<String, Value>, patch: &mut Map<String, Value>) {
	for key in old.keys() {
		if !new.contains_key(key) {
			patch.insert(key.clone(), Value::Null);
		}
	}
	for (key, new_val) in new {
		let old_val = old.get(key);
		if old_val == Some(new_val) {
			continue;
		}
		if let (Some(Value::Object(old_obj)), Value::Object(new_obj)) = (old_val, new_val) {
			let mut sub = Map::new();
			diff_objects(old_obj, new_obj, &mut sub);
			if !sub.is_empty() {
				patch.insert(key.clone(), Value::Object(sub));
			}
			continue;
		}
		patch.insert(key.clone(), new_val.clone());
	}
}

/// A large mostly-static document: a big config blob that never changes plus a few counters that
/// tick. This is the shape where a tiny merge patch should beat re-feeding the whole snapshot.
fn big_static(tick: u64) -> Value {
	// ~6 KB of static config (routes table) that is identical every tick.
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

const TICKS: u64 = 60;
const ITERS: u32 = 4000;

/// Print the headline merge-vs-deflate comparison (wire + producer + consumer CPU) for a workload.
fn compare(label: &str, frames: &[Value]) {
	let snapshot_len = serde_json::to_vec(&frames[0]).unwrap().len();
	let merge_wire: usize = merge_codec_frames(frames, 6).iter().map(|f| f.len()).sum();
	let deflate_wire = deflate_only_wire(frames, 6);
	let merge_stream = merge_codec_frames(frames, 6);
	let deflate_stream = deflate_only_frames(frames, 6);

	println!("=== {label} (~{snapshot_len} B snapshot, {TICKS} ticks, level 6) ===");
	println!("                                  wire      producer        consumer");
	let mp = bench_quiet(|| black_box_drop(merge_codec_frames(frames, 6)));
	let mc = bench_quiet(|| black_box_drop(merge_codec_consume(&merge_stream)));
	let dp = bench_quiet(|| black_box_drop(deflate_only_frames(frames, 6)));
	let dc = bench_quiet(|| black_box_drop(deflate_only_consume(&deflate_stream)));
	println!("  merge-patch + deflate    {merge_wire:>7} B   {mp:>8.0} ns/t    {mc:>8.0} ns/t");
	println!("  deflate-only             {deflate_wire:>7} B   {dp:>8.0} ns/t    {dc:>8.0} ns/t");
	println!(
		"  merge vs deflate-only:   {:.2}x bytes   {:+.0}% producer   {:+.0}% consumer\n",
		merge_wire as f64 / deflate_wire as f64,
		100.0 * (mp / dp - 1.0),
		100.0 * (mc / dc - 1.0),
	);
}

fn black_box_drop<T>(t: T) {
	black_box(t);
}

/// Generous ratio + compression: every tick after the first lands as a compressed delta in one group.
fn merge_cfg() -> ProducerConfig {
	let mut config = ProducerConfig::default();
	config.delta_ratio = 1_000_000;
	config.compression = true;
	config
}

/// Time a closure that runs the whole TICKS stream once, averaged over ITERS runs. Returns ns/tick.
fn bench_quiet(mut run: impl FnMut()) -> f64 {
	for _ in 0..50 {
		run();
	}
	let start = Instant::now();
	for _ in 0..ITERS {
		run();
	}
	start.elapsed().as_nanos() as f64 / (ITERS as f64 * TICKS as f64)
}

fn bench(name: &str, run: impl FnMut()) -> f64 {
	let per_tick = bench_quiet(run);
	println!("  {name:<42} {per_tick:>8.1} ns/tick");
	per_tick
}

fn main() {
	let frames: Vec<Value> = (0..TICKS).map(telemetry).collect();
	let big: Vec<Value> = (0..TICKS).map(big_static).collect();

	compare("telemetry: small doc, many fields move", &frames);
	compare("big-static: large doc, few fields move", &big);

	// ---------------- DEFLATE level sweep: the dominant CPU lever ----------------
	println!("\nDEFLATE level sweep (merge-patch producer): size vs CPU");
	for level in [1u32, 3, 6, 9] {
		let wire: usize = merge_codec_frames(&frames, level).iter().map(|f| f.len()).sum();
		let ns = bench_quiet(|| {
			black_box(merge_codec_frames(&frames, level));
		});
		println!("  level {level}: {wire:>6} B   {ns:>8.1} ns/tick");
	}

	// ---------------- Real Producer/Consumer (includes moq-net plumbing + double serialize) ----
	println!("\nFull stack (real Producer/Consumer, level 6, includes moq-net plumbing):");
	bench("Producer::update loop", || {
		black_box(merge_producer_wire(&frames));
	});
	let real_stream = merge_producer_frames(&frames);
	bench("Consumer::poll_next loop", || {
		black_box(merge_consume(&real_stream));
	});

	// ---------------- Where the merge-patch CPU goes (producer side) ----------------
	println!("\nProducer breakdown (merge-patch path, per tick):");
	bench("serde_json::to_value (build Value tree)", || {
		for f in &frames {
			black_box(serde_json::to_value(f).unwrap());
		}
	});
	bench("serde_json::to_vec (serialize snapshot)", || {
		for f in &frames {
			black_box(serde_json::to_vec(f).unwrap());
		}
	});
	let values: Vec<Value> = frames.iter().map(|f| serde_json::to_value(f).unwrap()).collect();
	bench("diff (generate merge patch)", || {
		for w in values.windows(2) {
			black_box(diff(&w[0], &w[1]));
		}
	});
	let patches: Vec<Value> = values.windows(2).map(|w| diff(&w[0], &w[1])).collect();
	bench("serde_json::to_vec (serialize patch)", || {
		for p in &patches {
			black_box(serde_json::to_vec(p).unwrap());
		}
	});
	let patch_bytes: Vec<Vec<u8>> = patches.iter().map(|p| serde_json::to_vec(p).unwrap()).collect();
	bench("deflate frame (compress patch)", || {
		let mut enc = Encoder::new();
		enc.frame(&serde_json::to_vec(&frames[0]).unwrap());
		for p in &patch_bytes {
			black_box(enc.frame(p));
		}
	});
	bench("deflate frame (compress full snapshot)", || {
		let mut enc = Encoder::new();
		for f in &frames {
			black_box(enc.frame(&serde_json::to_vec(f).unwrap()));
		}
	});

	println!("\nConsumer breakdown (merge-patch path, per tick):");
	bench("json_patch::merge (apply patch)", || {
		let mut cur = values[0].clone();
		for p in &patches {
			json_patch::merge(&mut cur, p);
			black_box(&cur);
		}
	});
	bench("from_value clone (reconstruct T)", || {
		for v in &values {
			let _: Value = black_box(serde_json::from_value(v.clone()).unwrap());
		}
	});
}

/// Run the real Producer over the stream and return total wire bytes.
fn merge_producer_wire(frames: &[Value]) -> usize {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	drain_wire(consumer)
}

/// Run the real Producer and capture the raw (compressed) frames, grouped, for the consumer bench.
fn merge_producer_frames(frames: &[Value]) -> Vec<Vec<Vec<u8>>> {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	collect_groups(consumer)
}

/// Decode a captured merge-patch stream the way the real Consumer does.
fn merge_consume(stream: &[Vec<Vec<u8>>]) -> Value {
	let track = build_track(stream);
	let mut cc = ConsumerConfig::default();
	cc.compression = true;
	let mut consumer = moq_json::Consumer::<Value>::new(track, cc);
	let waiter = kio::Waiter::noop();
	let mut last = Value::Null;
	while let Poll::Ready(Ok(Some(v))) = consumer.poll_next(&waiter) {
		last = v;
	}
	last
}

/// Merge-patch producer, pure codec: frame 0 is a snapshot, the rest are compressed merge patches.
/// Mirrors the real `Inner` delta path without moq-net Track/Group plumbing.
fn merge_codec_frames(frames: &[Value], level: u32) -> Vec<Vec<u8>> {
	let mut enc = Encoder::with_level(level);
	let mut out = Vec::with_capacity(frames.len());
	out.push(enc.frame(&serde_json::to_vec(&frames[0]).unwrap()).to_vec());
	let mut last = serde_json::to_value(&frames[0]).unwrap();
	for f in &frames[1..] {
		let next = serde_json::to_value(f).unwrap();
		let patch = diff(&last, &next);
		let bytes = serde_json::to_vec(&patch).unwrap();
		out.push(enc.frame(&bytes).to_vec());
		last = next;
	}
	out
}

/// Merge-patch consumer, pure codec: decode each frame, applying snapshot then merge patches, and
/// reconstruct only the final value (backlog collapse).
fn merge_codec_consume(stream: &[Vec<u8>]) -> Value {
	let mut dec = Decoder::new();
	let mut cur = serde_json::from_slice(&dec.frame(&stream[0]).unwrap()).unwrap();
	for f in &stream[1..] {
		let patch: Value = serde_json::from_slice(&dec.frame(f).unwrap()).unwrap();
		json_patch::merge(&mut cur, &patch);
	}
	let v: Value = serde_json::from_value(cur).unwrap();
	v
}

/// Deflate-only producer: feed the full snapshot through one shared window each tick.
fn deflate_only_frames(frames: &[Value], level: u32) -> Vec<Vec<u8>> {
	let mut enc = Encoder::with_level(level);
	frames
		.iter()
		.map(|f| enc.frame(&serde_json::to_vec(f).unwrap()).to_vec())
		.collect()
}

fn deflate_only_wire(frames: &[Value], level: u32) -> usize {
	deflate_only_frames(frames, level).iter().map(|f| f.len()).sum()
}

/// Deflate-only consumer: decode each frame and parse the full snapshot.
fn deflate_only_consume(stream: &[Vec<u8>]) -> Value {
	let mut dec = Decoder::new();
	let mut last = Value::Null;
	for f in stream {
		let plain = dec.frame(f).unwrap();
		last = serde_json::from_slice(&plain).unwrap();
	}
	last
}

// ---- moq-net plumbing helpers ----

fn drain_wire(consumer: moq_net::TrackConsumer) -> usize {
	collect_groups(consumer).iter().flatten().map(|f| f.len()).sum()
}

/// Capture the stored frames preserving group boundaries: one inner `Vec` per group. Each group is
/// its own DEFLATE stream, so the boundaries must survive the round-trip or `build_track` would feed
/// a later group's frames into an earlier group's window and decode against the wrong dictionary.
fn collect_groups(consumer: moq_net::TrackConsumer) -> Vec<Vec<Vec<u8>>> {
	let waiter = kio::Waiter::noop();
	let mut out = Vec::new();
	let mut track = consumer;
	while let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) {
		let mut frames = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			frames.push(frame.to_vec());
		}
		out.push(frames);
	}
	out
}

/// Replay captured groups onto a fresh track, one moq-net group per captured group.
fn build_track(groups: &[Vec<Vec<u8>>]) -> moq_net::TrackConsumer {
	let mut track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	for frames in groups {
		let mut group = track.append_group().unwrap();
		for f in frames {
			group.write_frame(bytes::Bytes::from(f.clone())).unwrap();
		}
		group.finish().unwrap();
	}
	track.finish().unwrap();
	consumer
}

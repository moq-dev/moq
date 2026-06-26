//! CPU benchmarks for `moq-json`.
//!
//! Compares the real serializer-backed merge-patch Producer/Consumer against a DEFLATE-only baseline
//! (full snapshot through one shared window every tick), plus a DEFLATE level sweep. The merge path
//! runs through the public `Producer`/`Consumer`, so it benchmarks the actual diffing serializer.
//!
//! Run with `cargo bench -p moq-json`.

use std::hint::black_box;
use std::task::Poll;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_flate::{Decoder, Encoder};
use moq_json::{ConsumerConfig, Producer, ProducerConfig};
use serde_json::{Value, json};

const TICKS: u64 = 60;

/// One second of telemetry: a big static core plus a few moving numbers. Most fields change a little
/// each tick, so deltas stay small but the document is mostly fresh.
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
/// tick. This is the shape where a tiny merge patch most beats re-feeding the whole snapshot.
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

/// Generous ratio + compression: every tick after the first lands as a compressed delta in one group.
fn merge_cfg() -> ProducerConfig {
	let mut config = ProducerConfig::default();
	config.delta_ratio = 1_000_000;
	config.compression = true;
	config
}

/// Total uncompressed JSON bytes across the stream, used as the benchmark throughput.
fn raw_bytes(frames: &[Value]) -> u64 {
	frames.iter().map(|f| serde_json::to_vec(f).unwrap().len() as u64).sum()
}

/// Drive the real merge Producer over the whole stream (serializer + DEFLATE + moq-net framing).
fn run_merge_producer(frames: &[Value]) -> usize {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	collect_groups(consumer).iter().flatten().map(Vec::len).sum()
}

/// DEFLATE-only producer: feed the full snapshot through one shared window each tick.
fn run_deflate_only_producer(frames: &[Value], level: u32) -> usize {
	let mut enc = Encoder::with_level(level);
	frames
		.iter()
		.map(|f| enc.frame(&serde_json::to_vec(f).unwrap()).len())
		.sum()
}

/// Capture the real merge stream (grouped frames) so the consumer bench can replay it repeatedly.
fn capture_merge(frames: &[Value]) -> Vec<Vec<Vec<u8>>> {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	collect_groups(consumer)
}

/// Decode a captured merge stream through the real Consumer.
fn run_merge_consumer(stream: &[Vec<Vec<u8>>]) -> Value {
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

/// DEFLATE-only consumer: decode each frame and parse the full snapshot.
fn run_deflate_only_consumer(stream: &[Vec<u8>]) -> Value {
	let mut dec = Decoder::new();
	let mut last = Value::Null;
	for f in stream {
		last = serde_json::from_slice(&dec.frame(f).unwrap()).unwrap();
	}
	last
}

fn deflate_only_stream(frames: &[Value], level: u32) -> Vec<Vec<u8>> {
	let mut enc = Encoder::with_level(level);
	frames
		.iter()
		.map(|f| enc.frame(&serde_json::to_vec(f).unwrap()).to_vec())
		.collect()
}

/// Capture the stored frames preserving group boundaries (one inner Vec per group): each group is its
/// own DEFLATE stream, so replaying them into a single group would decode against the wrong window.
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
			group.write_frame(Bytes::from(f.clone())).unwrap();
		}
		group.finish().unwrap();
	}
	track.finish().unwrap();
	consumer
}

/// The two workloads, each a full `TICKS`-long stream.
fn workloads() -> Vec<(&'static str, Vec<Value>)> {
	vec![
		("telemetry", (0..TICKS).map(telemetry).collect()),
		("big_static", (0..TICKS).map(big_static).collect()),
	]
}

/// Producer CPU: serializer-backed merge + DEFLATE vs a DEFLATE-only baseline.
fn producer(c: &mut Criterion) {
	let mut group = c.benchmark_group("producer");
	for (name, frames) in workloads() {
		group.throughput(Throughput::Bytes(raw_bytes(&frames)));
		group.bench_with_input(BenchmarkId::new("merge", name), &frames, |b, frames| {
			b.iter(|| black_box(run_merge_producer(frames)));
		});
		group.bench_with_input(BenchmarkId::new("deflate_only", name), &frames, |b, frames| {
			b.iter(|| black_box(run_deflate_only_producer(frames, 6)));
		});
	}
	group.finish();
}

/// Consumer CPU: reconstructing from merge deltas vs parsing a full snapshot every frame.
fn consumer(c: &mut Criterion) {
	let mut group = c.benchmark_group("consumer");
	for (name, frames) in workloads() {
		group.throughput(Throughput::Bytes(raw_bytes(&frames)));
		let merge = capture_merge(&frames);
		group.bench_with_input(BenchmarkId::new("merge", name), &merge, |b, merge| {
			b.iter(|| black_box(run_merge_consumer(merge)));
		});
		let deflate = deflate_only_stream(&frames, 6);
		group.bench_with_input(BenchmarkId::new("deflate_only", name), &deflate, |b, deflate| {
			b.iter(|| black_box(run_deflate_only_consumer(deflate)));
		});
	}
	group.finish();
}

/// DEFLATE level sweep: the dominant CPU lever. Compresses the full telemetry snapshot at each level.
fn deflate_level(c: &mut Criterion) {
	let frames: Vec<Value> = (0..TICKS).map(telemetry).collect();
	let mut group = c.benchmark_group("deflate_level");
	group.throughput(Throughput::Bytes(raw_bytes(&frames)));
	for level in [1u32, 3, 6, 9] {
		group.bench_with_input(BenchmarkId::from_parameter(level), &level, |b, &level| {
			b.iter(|| black_box(run_deflate_only_producer(&frames, level)));
		});
	}
	group.finish();
}

criterion_group!(benches, producer, consumer, deflate_level);
criterion_main!(benches);

//! Measure wire savings of group-scoped DEFLATE + snapshot/delta on a telemetry stream.
//!
//! Simulates a realistic device telemetry blob that ticks once per second: most of the document
//! is static (identity, config, geo) while a handful of gauges and counters change each tick. This
//! is exactly the shape `moq-json` targets, so it shows the snapshot/delta and compression knobs
//! pulling in the same direction.
//!
//! Run with: `cargo run -p moq-json --example telemetry`

use std::task::Poll;

use moq_json::{ConsumerConfig, Producer, ProducerConfig};
use serde_json::{Value, json};

/// One second of telemetry for a fleet device: a big static core plus a few moving numbers.
fn telemetry(tick: u64) -> Value {
	// A slow drift so consecutive ticks differ by a little, like real sensors.
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
			"temp_c": (4.0 + (t * 0.05).sin() * 1.5 * 100.0).round() / 100.0,
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

/// Total wire bytes of every frame across every group for a full run under `config`.
fn wire_bytes(config: ProducerConfig, ticks: u64) -> usize {
	let track = moq_net::BroadcastInfo::new()
		.produce()
		.create_track("telemetry", None)
		.unwrap();
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, config);

	for tick in 0..ticks {
		producer.update(&telemetry(tick)).unwrap();
	}
	producer.finish().unwrap();

	// Drain the raw stored frames (compressed if the producer compressed them) and sum their sizes.
	let waiter = kio::Waiter::noop();
	let mut total = 0;
	let mut track = consumer;
	while let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) {
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			total += frame.len();
		}
	}
	total
}

/// Drive a producer and a live consumer in lockstep, asserting that EVERY tick reconstructs to the
/// exact input value after decompression and delta application (not just the final one).
fn verify(producer_config: ProducerConfig, ticks: u64) {
	let track = moq_net::BroadcastInfo::new()
		.produce()
		.create_track("telemetry", None)
		.unwrap();
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, producer_config.clone());

	let mut consumer_config = ConsumerConfig::default();
	consumer_config.compression = producer_config.compression;
	let mut consumer = moq_json::Consumer::<Value>::new(consumer, consumer_config);
	let waiter = kio::Waiter::noop();

	for tick in 0..ticks {
		let expected = telemetry(tick);
		producer.update(&expected).unwrap();
		// The producer emits exactly one frame per update, so the live consumer yields exactly one
		// reconstructed value: it must match the input byte-for-byte after decompression + patching.
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, expected, "tick {tick} reconstruction mismatch"),
			other => panic!("tick {tick}: expected a value, got {other:?}"),
		}
	}
	producer.finish().unwrap();

	// Drain: nothing left and the stream ends cleanly.
	assert!(
		matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))),
		"stream did not end cleanly"
	);
}

/// A consumer that joins only after the whole stream exists must still rebuild the latest value from
/// the newest group's snapshot + deltas. For the compressed path this exercises the lazy decoder
/// replaying the group's already-stored slices to warm its window before decoding the final frame.
///
/// Returns how many values the late joiner surfaced to the application: with backlog collapsing this
/// is far below `ticks`, since stale intermediate reconstructions are applied internally but skipped.
fn verify_late_joiner(producer_config: ProducerConfig, ticks: u64) -> usize {
	let track = moq_net::BroadcastInfo::new()
		.produce()
		.create_track("telemetry", None)
		.unwrap();
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, producer_config.clone());
	for tick in 0..ticks {
		producer.update(&telemetry(tick)).unwrap();
	}
	producer.finish().unwrap();

	let mut consumer_config = ConsumerConfig::default();
	consumer_config.compression = producer_config.compression;
	let mut consumer = moq_json::Consumer::<Value>::new(consumer, consumer_config);
	let waiter = kio::Waiter::noop();
	let mut last = None;
	let mut yielded = 0;
	while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
		last = Some(value);
		yielded += 1;
	}
	assert_eq!(
		last.as_ref(),
		Some(&telemetry(ticks - 1)),
		"late joiner reconstruction mismatch"
	);
	yielded
}

fn cfg(delta_ratio: u32, compression: bool) -> ProducerConfig {
	let mut config = ProducerConfig::default();
	config.delta_ratio = delta_ratio;
	config.compression = compression;
	config
}

fn main() {
	const TICKS: u64 = 60;

	// Raw baseline: every tick as a full JSON blob, no moq-json framing tricks.
	let raw: usize = (0..TICKS)
		.map(|t| serde_json::to_vec(&telemetry(t)).unwrap().len())
		.sum();
	let snapshot_len = serde_json::to_vec(&telemetry(0)).unwrap().len();

	let combos = [
		("snapshot-per-group, plaintext", cfg(0, false)),
		("snapshot-per-group, deflate   ", cfg(0, true)),
		("snapshot+delta,     plaintext", cfg(8, false)),
		("snapshot+delta,     deflate   ", cfg(8, true)),
	];

	println!("Telemetry stream: {TICKS} ticks, ~{snapshot_len} bytes per snapshot\n");
	println!("Raw JSON (one blob per tick):        {raw:>8} bytes  (baseline)\n");

	println!("{:<32} {:>10} {:>10} {:>9}", "config", "wire", "vs raw", "saved");
	println!("{}", "-".repeat(64));
	for (name, config) in combos.clone() {
		verify(config.clone(), TICKS);
		verify_late_joiner(config.clone(), TICKS);
		let bytes = wire_bytes(config, TICKS);
		let pct = 100.0 * bytes as f64 / raw as f64;
		let saved = 100.0 - pct;
		println!("{name:<32} {bytes:>8} B {pct:>8.1}% {saved:>7.1}%");
	}

	println!("\nVerified: every tick reconstructs exactly (live + late joiner) for all 4 configs.");

	// Late-joiner collapse: a consumer joining after all {TICKS} ticks exist gets the head in one
	// step, not a replay of every superseded state.
	println!("\nLate joiner: values surfaced to the app (was {TICKS} per-frame, now collapsed):");
	for (name, config) in combos {
		let yielded = verify_late_joiner(config, TICKS);
		println!("  {name:<32} {yielded:>3} value(s)");
	}
}

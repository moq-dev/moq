//! Measure the savings of snapshot/delta on a telemetry stream, and check it reads back the same
//! over a compressed track.
//!
//! Simulates a realistic device telemetry blob that ticks once per second: most of the document
//! is static (identity, config, geo) while a handful of gauges and counters change each tick. This
//! is exactly the shape `moq-json` targets. The bytes counted are the JSON frames a plain reader
//! sees; a [`compress`](moq_net::track::Info::compress)ed track shrinks them further on the wire.
//!
//! Run with: `cargo run -p moq-json --example telemetry`

use std::task::Poll;

use moq_json::snapshot::{self, Consumer, Producer};
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

/// The telemetry track, optionally compressed.
fn track(compress: bool) -> moq_net::track::Producer {
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_compress(compress);
	moq_net::broadcast::Info::new()
		.produce()
		.create_track("telemetry", info)
		.unwrap()
}

/// Total bytes of every frame across every group for a full run under `config`.
fn frame_bytes(config: snapshot::Config, ticks: u64) -> usize {
	let track = track(false);
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, config);

	for tick in 0..ticks {
		producer
			.update(moq_net::Timed::from(&telemetry(tick)).at(moq_net::Timestamp::now()))
			.unwrap();
	}
	producer.finish().unwrap();

	// Drain the frames and sum their sizes.
	let waiter = kio::Waiter::noop();
	let mut total = 0;
	let mut track = consumer.ordered();
	while let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) {
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			total += frame.payload.len();
		}
	}
	total
}

/// Drive a producer and a live consumer in lockstep, asserting that EVERY tick reconstructs to the
/// exact input value after delta application (not just the final one).
fn verify(producer_config: snapshot::Config, compress: bool, ticks: u64) {
	let track = track(compress);
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, producer_config);
	let mut consumer = Consumer::<Value>::new(consumer, Default::default());
	let waiter = kio::Waiter::noop();

	for tick in 0..ticks {
		let expected = telemetry(tick);
		producer
			.update(moq_net::Timed::from(&expected).at(moq_net::Timestamp::now()))
			.unwrap();
		// The producer emits exactly one frame per update, so the live consumer yields exactly one
		// reconstructed value: it must match the input byte-for-byte after patching.
		match consumer.poll_latest(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value.value, expected, "tick {tick} reconstruction mismatch"),
			other => panic!("tick {tick}: expected a value, got {other:?}"),
		}
	}
	producer.finish().unwrap();

	// Drain: nothing left and the stream ends cleanly.
	assert!(
		matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(None))),
		"stream did not end cleanly"
	);
}

/// A consumer that joins only after the whole stream exists must still rebuild the latest value from
/// the newest group's snapshot + deltas. For a compressed track this reads moq-net's inflated copy
/// of the group.
///
/// Returns how many values the late joiner surfaced to the application: with backlog collapsing this
/// is far below `ticks`, since stale intermediate reconstructions are applied internally but skipped.
fn verify_late_joiner(producer_config: snapshot::Config, compress: bool, ticks: u64) -> usize {
	let track = track(compress);
	let consumer = track.subscribe(None);
	let mut producer = Producer::<Value>::new(track, producer_config);
	for tick in 0..ticks {
		producer
			.update(moq_net::Timed::from(&telemetry(tick)).at(moq_net::Timestamp::now()))
			.unwrap();
	}
	producer.finish().unwrap();

	let mut consumer = Consumer::<Value>::new(consumer, Default::default());
	let waiter = kio::Waiter::noop();
	let mut last = None;
	let mut yielded = 0;
	while let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) {
		last = Some(value.value);
		yielded += 1;
	}
	assert_eq!(
		last.as_ref(),
		Some(&telemetry(ticks - 1)),
		"late joiner reconstruction mismatch"
	);
	yielded
}

fn main() {
	const TICKS: u64 = 60;

	// Raw baseline: every tick as a full JSON blob, no moq-json framing tricks.
	let raw: usize = (0..TICKS)
		.map(|t| serde_json::to_vec(&telemetry(t)).unwrap().len())
		.sum();
	let snapshot_len = serde_json::to_vec(&telemetry(0)).unwrap().len();

	let combos = [
		("snapshot-per-group", snapshot::Config::default().with_deltas(false)),
		("snapshot+delta", snapshot::Config::default().with_deltas(true)),
	];

	println!("Telemetry stream: {TICKS} ticks, ~{snapshot_len} bytes per snapshot\n");
	println!("Raw JSON (one blob per tick):        {raw:>8} bytes  (baseline)\n");

	println!("{:<32} {:>10} {:>10} {:>9}", "config", "frames", "vs raw", "saved");
	println!("{}", "-".repeat(64));
	for (name, config) in combos.clone() {
		for compress in [false, true] {
			verify(config.clone(), compress, TICKS);
			verify_late_joiner(config.clone(), compress, TICKS);
		}
		let bytes = frame_bytes(config, TICKS);
		let pct = 100.0 * bytes as f64 / raw as f64;
		let saved = 100.0 - pct;
		println!("{name:<32} {bytes:>8} B {pct:>8.1}% {saved:>7.1}%");
	}

	println!("\nVerified: every tick reconstructs exactly (live + late joiner), plain and compressed.");

	// Late-joiner collapse: a consumer joining after all {TICKS} ticks exist gets the head in one
	// step, not a replay of every superseded state.
	println!("\nLate joiner: values surfaced to the app (was {TICKS} per-frame, now collapsed):");
	for (name, config) in combos {
		let yielded = verify_late_joiner(config, false, TICKS);
		println!("  {name:<32} {yielded:>3} value(s)");
	}
}

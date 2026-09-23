//! `.json.z` against `.fb.z` on the producer's own frame buffers: bytes after
//! DEFLATE, encode and decode time, and allocations on both sides, swept over
//! broadcasts x tiers. Each tier carries a publisher and a subscriber track, and
//! every tick the given share of broadcasts moves its counters, as live media
//! does.
//!
//! The deterministic half (bytes, allocations) runs in CI. For the timing
//! table, run in release:
//! `cargo test -p moq-stats --release --lib bench::report -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use moq_net::PathOwned;
use moq_net::stats::Traffic;

use crate::counting::allocs;
use crate::fb;
use crate::produce::Frame;

/// Totals for one format over a run.
#[derive(Default)]
struct Totals {
	bytes: u64,
	encode: Duration,
	decode: Duration,
	/// Allocations in steady ticks, after the first frame warmed every buffer.
	encode_allocs: usize,
	decode_allocs: usize,
	/// Steady ticks measured, to report per-tick averages.
	ticks: usize,
}

struct Scenario {
	broadcasts: usize,
	tiers: usize,
	/// Percent of broadcasts whose counters move each tick.
	changed: usize,
	ticks: u64,
}

impl Scenario {
	/// One track's frame at `tick`: sorted entries, cumulative counters.
	fn frame(&self, tier: usize, tick: u64) -> Frame<Traffic> {
		let moving = self.broadcasts * self.changed / 100;
		let mut frame = Frame::default();
		for b in 0..self.broadcasts {
			let steps = if b < moving { tick } else { 0 };
			let mut t = Traffic::default();
			t.announces_started = 1;
			t.announced_bytes = 12;
			t.broadcasts_started = 3 + (b % 5) as u64;
			t.subscriptions_started = 6 + (b % 7) as u64;
			t.subscriptions_ended = 2;
			t.bytes = (b as u64 + 1) * 1_000_000 + steps * (48_000 + (b as u64 * 7919) % 20_000);
			t.frames = (b as u64 + 1) * 3_000 + steps * 30;
			t.groups = (b as u64 + 1) * 100 + steps;
			frame
				.entries
				.push((PathOwned::from(format!("tier{tier}/room{b:05}/cam")), t));
		}
		frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		frame
	}

	/// Run every track through `.json.z`, the producer's config.
	fn json(&self) -> Totals {
		let mut totals = Totals::default();
		for tier in 0..self.tiers * 2 {
			let mut config = moq_json::snapshot::Config::default();
			config.compression = moq_json::Compression::Deflate;
			let mut encoder = moq_json::snapshot::Encoder::<Frame<Traffic>>::new(config);
			let mut consumer = moq_json::snapshot::consumer::Config::default();
			consumer.compression = moq_json::Compression::Deflate;
			let mut decoder = moq_json::snapshot::Decoder::<BTreeMap<String, Traffic>>::new(consumer);

			for tick in 0..self.ticks {
				let frame = self.frame(tier, tick);
				let steady = tick > 0;

				let (start, before) = (Instant::now(), allocs());
				let encoded = match encoder.update(&frame).expect("encode") {
					Some(pending) => {
						let encoded = (*pending).clone();
						pending.commit();
						Some(encoded)
					}
					None => None,
				};
				let (elapsed, count) = (start.elapsed(), allocs() - before);
				totals.encode += elapsed;
				if steady {
					totals.encode_allocs += count;
				}
				let Some(encoded) = encoded else { continue };
				totals.bytes += encoded.payload.len() as u64;

				let (start, before) = (Instant::now(), allocs());
				match encoded.keyframe {
					true => decoder.snapshot(&encoded.payload).expect("snapshot"),
					false => decoder.delta(&encoded.payload).expect("delta"),
				}
				let decoded = decoder.decode().expect("decode").expect("value");
				let (elapsed, count) = (start.elapsed(), allocs() - before);
				totals.decode += elapsed;
				if steady {
					totals.decode_allocs += count;
				}
				assert_eq!(decoded.len(), self.broadcasts);
			}
		}
		totals.ticks = self.ticks as usize - 1;
		totals
	}

	/// Run every track through `.fb.z`.
	fn flatbuffers(&self) -> Totals {
		let mut totals = Totals::default();
		for tier in 0..self.tiers * 2 {
			let mut encoder = fb::Encoder::<Traffic>::new();
			let mut decoder = fb::Decoder::<Traffic>::new();

			for tick in 0..self.ticks {
				let frame = self.frame(tier, tick);
				let steady = tick > 0;

				let (start, before) = (Instant::now(), allocs());
				let encoded = encoder.encode(&frame.entries).expect("encode");
				let (elapsed, count) = (start.elapsed(), allocs() - before);
				totals.encode += elapsed;
				if steady {
					totals.encode_allocs += count;
				}
				let Some(encoded) = encoded else { continue };
				totals.bytes += encoded.payload.len() as u64;

				let (start, before) = (Instant::now(), allocs());
				decoder.inflate(&encoded.payload, encoded.keyframe).expect("inflate");
				let decoded = decoder.decode().expect("decode");
				let (elapsed, count) = (start.elapsed(), allocs() - before);
				totals.decode += elapsed;
				if steady {
					totals.decode_allocs += count;
				}
				assert_eq!(decoded.len(), self.broadcasts);
			}
		}
		totals.ticks = self.ticks as usize - 1;
		totals
	}
}

fn scenarios(broadcasts: &[usize], ticks: u64) -> Vec<Scenario> {
	let mut out = Vec::new();
	for &changed in &[100, 10] {
		for &tiers in &[1, 4] {
			for &broadcasts in broadcasts {
				out.push(Scenario {
					broadcasts,
					tiers,
					changed,
					ticks,
				});
			}
		}
	}
	out
}

/// `.fb.z` must beat `.json.z` on bytes and on allocations in both
/// directions, at every size.
#[test]
fn flatbuffers_beats_json() {
	for scenario in scenarios(&[1, 16, 256], 20) {
		let json = scenario.json();
		let fb = scenario.flatbuffers();
		let label = format!(
			"{} broadcasts x {} tiers, {}% changed",
			scenario.broadcasts, scenario.tiers, scenario.changed
		);
		assert!(
			fb.bytes < json.bytes,
			"bytes: fb {} >= json {}: {label}",
			fb.bytes,
			json.bytes
		);
		assert!(
			fb.encode_allocs < json.encode_allocs,
			"encode allocations: fb {} >= json {}: {label}",
			fb.encode_allocs,
			json.encode_allocs
		);
		assert!(
			fb.decode_allocs < json.decode_allocs,
			"decode allocations: fb {} >= json {}: {label}",
			fb.decode_allocs,
			json.decode_allocs
		);
	}
}

/// Print the full comparison table.
#[test]
#[ignore = "timing report; run in release with --ignored --nocapture"]
fn report() {
	println!(
		"| broadcasts | tiers | changed | format | bytes/tick | encode us/tick | decode us/tick | encode allocs/tick | decode allocs/tick |"
	);
	println!("|---:|---:|---:|---|---:|---:|---:|---:|---:|");
	for scenario in scenarios(&[1, 16, 256, 4096], 60) {
		for (name, totals) in [(".json.z", scenario.json()), (".fb.z", scenario.flatbuffers())] {
			let ticks = scenario.ticks as f64;
			let steady = totals.ticks as f64;
			println!(
				"| {} | {} | {}% | {} | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} |",
				scenario.broadcasts,
				scenario.tiers,
				scenario.changed,
				name,
				totals.bytes as f64 / ticks,
				totals.encode.as_secs_f64() * 1e6 / ticks,
				totals.decode.as_secs_f64() * 1e6 / ticks,
				totals.encode_allocs as f64 / steady,
				totals.decode_allocs as f64 / steady,
			);
		}
	}
}

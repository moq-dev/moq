//! `snapshot::Encoder::update` on a keyed table shaped like a moq-stats traffic frame, swept over
//! total rows and the share of rows that change per tick. Cases that change the same number of rows
//! (100 rows at 100%, 10k at 1%) separate a cost that grows with the table from one that grows with
//! the changed rows.
//!
//! Each iteration is one tick against a long-lived encoder, so the periodic snapshots the delta
//! budget forces are amortized in, as they are in production.
//!
//! Run with `cargo bench -p moq-json --bench table`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_json::snapshot::{Config, Encoder};
use moq_net::stats::Traffic;
use serde::Serialize;

/// One tick of the table: rows sorted by path, serialized as a JSON object like moq-stats' `Frame`.
struct Table {
	rows: Vec<(String, Traffic)>,
	/// Every `stride`-th row changes each tick.
	stride: usize,
}

impl Serialize for Table {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_map(self.rows.iter().map(|(path, value)| (path.as_str(), value)))
	}
}

impl Table {
	fn new(rows: usize, changed_percent: usize) -> Self {
		let mut rows: Vec<(String, Traffic)> = (0..rows as u64)
			.map(|i| {
				let mut traffic = Traffic::default();
				traffic.announces_started = 1;
				traffic.announced_bytes = 24;
				traffic.broadcasts_started = 3 + i % 7;
				traffic.broadcasts_ended = i % 3;
				traffic.subscriptions_started = 9 + i % 21;
				traffic.subscriptions_ended = 3 * (i % 3);
				traffic.bytes = 62_500 * (1_000 + i * 37);
				traffic.frames = 80 * (1_000 + i * 37);
				traffic.groups = 2 * (1_000 + i * 37);
				(
					format!("anon/live-stream-{:05}-{i}", i.wrapping_mul(7919) % 100_000),
					traffic,
				)
			})
			.collect();
		rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		Self {
			rows,
			stride: 100 / changed_percent,
		}
	}

	/// Advance one tick: the changed rows move their payload counters, the rest stay put.
	fn tick(&mut self) {
		for (_, traffic) in self.rows.iter_mut().step_by(self.stride) {
			traffic.bytes += 62_500;
			traffic.frames += 80;
			traffic.groups += 2;
		}
	}
}

/// The two encoders the stats producer runs per track: `.json.z` and the plain `.json` sibling.
fn configs() -> [(&'static str, Config); 2] {
	let mut compressed = Config::default();
	compressed.compression = moq_json::Compression::Deflate;
	[("json.z", compressed), ("json", Config::default().with_delta_ratio(0))]
}

fn table(c: &mut Criterion) {
	for (name, config) in configs() {
		let mut group = c.benchmark_group(format!("table/{name}"));
		group.sample_size(10);
		group.warm_up_time(std::time::Duration::from_secs(1));
		group.measurement_time(std::time::Duration::from_secs(2));
		for rows in [100, 1_000, 10_000, 20_000] {
			for changed in [1, 10, 100] {
				let mut table = Table::new(rows, changed);
				let mut encoder = Encoder::<Table>::new(config.clone());
				group.throughput(Throughput::Elements(rows as u64));
				group.bench_function(BenchmarkId::new(format!("{changed}%"), rows), |b| {
					b.iter(|| {
						table.tick();
						let frame = encoder.update(&table).unwrap().expect("a changed tick");
						black_box(&frame.payload);
						frame.commit();
					});
				});
			}
		}
		group.finish();
	}
}

criterion_group!(benches, table);
criterion_main!(benches);

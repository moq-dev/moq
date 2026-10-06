//! `Writer::run` recording every enrolled track into memory, swept over the number of tracks and
//! the groups each one publishes. Throughput is per frame, so a per-event cost that grows with the
//! track table shows up as a slope across the track counts.
//!
//! Run with `cargo bench -p moq-archive --bench writer`.

use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_archive::object_store::memory::InMemory;
use moq_archive::{Store, Writer, writer};
use moq_net::{Timescale, Timestamp, broadcast, group, track};

const FRAMES_PER_GROUP: u64 = 2;

/// A writer with `tracks` enrolled tracks, each already holding `groups` one-second groups.
async fn setup(tracks: usize, groups: u64) -> Writer<InMemory> {
	let source = broadcast::Info::new().produce();
	let store = Store::new(InMemory::new(), "bench");
	let writer = Writer::new(store, source.consume(), writer::Config::default())
		.await
		.unwrap();
	let control = writer.control();
	let config = moq_mux::timeline::Config::default().with_duration_min(Duration::from_secs(1));

	for index in 0..tracks {
		let name = format!("track{index}");
		let info = track::Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(Duration::from_secs(3600));
		let track = source.create_track(name.as_str(), info).unwrap();
		control.track(&name, config.clone()).await.unwrap();
		for sequence in 0..groups {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			for frame in 0..FRAMES_PER_GROUP {
				let pts = Timestamp::from_millis(sequence * 1_000 + frame * 500).unwrap();
				group.write_frame(pts, vec![0u8; 64]).unwrap();
			}
			group.finish().unwrap();
		}
		track.finish().unwrap();
	}
	source.close();
	writer
}

fn writer(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap();

	let mut bench = c.benchmark_group("writer");
	bench.sample_size(10);
	bench.warm_up_time(Duration::from_secs(1));
	bench.measurement_time(Duration::from_secs(2));
	for tracks in [1, 10, 100] {
		for groups in [10, 100] {
			bench.throughput(Throughput::Elements(tracks as u64 * groups * FRAMES_PER_GROUP));
			bench.bench_function(BenchmarkId::new(format!("{groups} groups"), tracks), |b| {
				b.iter_custom(|iters| {
					let mut total = Duration::ZERO;
					for _ in 0..iters {
						let writer = runtime.block_on(setup(tracks, groups));
						let start = Instant::now();
						runtime.block_on(writer.run()).unwrap();
						total += start.elapsed();
					}
					total
				});
			});
		}
	}
	bench.finish();
}

criterion_group!(benches, writer);
criterion_main!(benches);

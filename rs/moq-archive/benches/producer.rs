#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "host time measures benchmark throughput"
)]

//! `Producer::run` recording every track of a broadcast into memory, swept over the number of
//! tracks and the groups each one holds. Throughput is per frame, so a per-event cost that grows
//! with the track table shows up as a slope across the track counts.
//!
//! Run with `cargo bench -p moq-archive --bench producer`.

use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_archive::object_store::memory::InMemory;
use moq_archive::{Producer, Store, producer};
use moq_net::{Timescale, Timestamp, broadcast, group, track};

const FRAMES_PER_GROUP: u64 = 2;

/// A broadcast of `tracks` tracks, each already holding `groups` one-second groups.
fn setup(tracks: usize, groups: u64) -> (broadcast::Producer, Vec<track::Producer>) {
	let source = broadcast::Info::new().produce();
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(Duration::from_secs(3600));
	let tracks = (0..tracks)
		.map(|index| {
			let track = source.create_track(format!("track{index}"), info.clone()).unwrap();
			for sequence in 0..groups {
				let mut group = track.create_group(group::Info { sequence }).unwrap();
				for frame in 0..FRAMES_PER_GROUP {
					let pts = Timestamp::from_millis(sequence * 1_000 + frame * 500).unwrap();
					group.write_frame(pts, vec![0u8; 64]).unwrap();
				}
				group.finish().unwrap();
			}
			track
		})
		.collect();
	(source, tracks)
}

/// Record the broadcast until every track ends, from the producer's start.
async fn record(source: broadcast::Producer, tracks: Vec<track::Producer>) -> Duration {
	let config = producer::Config::default()
		.with_duration(Duration::from_secs(2))
		.with_max_delay(Duration::from_secs(3600));
	let store = Store::new(InMemory::new(), "bench");
	let start = Instant::now();
	let run = tokio::spawn(Producer::new(store, source.consume(), config).run());
	for track in &tracks {
		while track.subscription().is_none() {
			tokio::task::yield_now().await;
		}
		track.finish().unwrap();
	}
	source.close();
	run.await.unwrap().unwrap();
	start.elapsed()
}

fn producer(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap();

	let mut bench = c.benchmark_group("producer");
	bench.sample_size(10);
	bench.warm_up_time(Duration::from_secs(1));
	bench.measurement_time(Duration::from_secs(2));
	for tracks in [1, 10, 100] {
		for groups in [10, 100] {
			bench.throughput(Throughput::Elements(tracks as u64 * groups * FRAMES_PER_GROUP));
			bench.bench_function(BenchmarkId::new(format!("{groups} groups"), tracks), |b| {
				b.iter_custom(|iters| {
					(0..iters)
						.map(|_| {
							let (source, tracks) = setup(tracks, groups);
							runtime.block_on(record(source, tracks))
						})
						.sum()
				});
			});
		}
	}
	bench.finish();
}

criterion_group!(benches, producer);
criterion_main!(benches);

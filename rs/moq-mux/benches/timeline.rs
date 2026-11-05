//! The shared clock's update and presentation cost, over queued pictures and independent readers.
use std::{hint::black_box, time::Duration};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kio::clock::Instant;
use moq_mux::timeline;
use moq_net::Timestamp;

fn presentation(c: &mut Criterion) {
	let mut group = c.benchmark_group("timeline_present");
	let now = Instant::ZERO + Duration::from_secs(1);
	for readers in [1, 8, 64] {
		for frames in [1, 3, 30] {
			let clock = timeline::Producer::new(Duration::from_millis(100));
			clock.audio(
				moq_mux::timeline::Audio {
					end: Duration::from_secs(1),
					buffered: Duration::from_millis(100),
					target: Duration::from_millis(100),
				},
				now,
			);
			let consumers: Vec<_> = (0..readers).map(|_| clock.consume()).collect();
			let timestamps: Vec<_> = (0..frames)
				.map(|index| Timestamp::from_millis(1_000 + index * 20).unwrap())
				.collect();
			group.throughput(Throughput::Elements(readers));
			group.bench_function(BenchmarkId::new(readers.to_string(), frames), |b| {
				b.iter(|| {
					for consumer in &consumers {
						let selected =
							consumer.present(black_box(timestamps.iter().copied()), now + Duration::from_secs(1));
						assert_eq!(selected, Some(frames as usize - 1));
					}
				});
			});
		}
	}
	group.finish();
}

fn update(c: &mut Criterion) {
	let now = Instant::ZERO + Duration::from_secs(1);
	let clock = timeline::Producer::new(Duration::from_millis(100));
	c.bench_function("timeline_audio_update", |b| {
		b.iter(|| {
			black_box(clock.audio(
				moq_mux::timeline::Audio {
					end: black_box(Duration::from_secs(1)),
					buffered: Duration::from_millis(100),
					target: Duration::from_millis(100),
				},
				now,
			))
		});
	});
}

criterion_group!(benches, presentation, update);
criterion_main!(benches);

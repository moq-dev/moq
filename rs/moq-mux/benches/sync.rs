//! The shared clock's update and presentation cost, over queued pictures and independent readers.
use std::{hint::black_box, time::Duration};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kio::clock::Instant;
use moq_mux::{container::Kind, sync};
use moq_net::Timestamp;

fn presentation(c: &mut Criterion) {
	let mut group = c.benchmark_group("sync_present");
	let now = Instant::ZERO.saturating_add(Duration::from_secs(1));
	for readers in [1, 8, 64] {
		for frames in [1, 3, 30] {
			let clock = sync::Producer::new(Duration::from_millis(100));
			let audio = clock.track(Kind::Audio);
			let video = clock.track(Kind::Video);
			audio.received(Timestamp::from_millis(1_000).unwrap(), now);
			video.received(Timestamp::from_millis(1_000).unwrap(), now);
			let consumers: Vec<_> = (0..readers).map(|_| clock.consume()).collect();
			let timestamps: Vec<_> = (0..frames)
				.map(|index| Timestamp::from_millis(1_000 + index * 20).unwrap())
				.collect();
			group.throughput(Throughput::Elements(readers));
			group.bench_function(BenchmarkId::new(readers.to_string(), frames), |b| {
				b.iter(|| {
					for consumer in &consumers {
						let selected = consumer.present(
							black_box(timestamps.iter().copied()),
							now.saturating_add(Duration::from_secs(1)),
						);
						assert_eq!(selected, Some(frames as usize - 1));
					}
				});
			});
		}
	}
	group.finish();
}

fn update(c: &mut Criterion) {
	let now = Instant::ZERO.saturating_add(Duration::from_secs(1));
	let clock = sync::Producer::new(Duration::from_millis(100));
	let audio = clock.track(Kind::Audio);
	let _video = clock.track(Kind::Video);
	c.bench_function("sync_received", |b| {
		b.iter(|| black_box(audio.received(black_box(Timestamp::from_millis(1_000).unwrap()), now)));
	});
}

criterion_group!(benches, presentation, update);
criterion_main!(benches);

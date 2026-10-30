//! The decode clock swept over how many frames it holds and how their PTS run: draining what
//! it holds, and settling frames as they are read until the overdue rule bounds them.
use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_clock::Instant;

#[path = "../src/container/decode.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod decode;

/// A 40 ms frame in 90 kHz ticks.
const FRAME: u64 = 3_600;

/// The `index`-th frame's PTS in decode order: stuck, running backward, or a valid B-pyramid.
fn pts(shape: &str, index: u64) -> u64 {
	let base = 1_000 * 90_000;
	match shape {
		"flat" => base,
		"backward" => base - index,
		_ => {
			const PYRAMID: [u64; 9] = [0, 4, 2, 1, 3, 8, 6, 5, 7];
			base + (index / 9 * 9 + PYRAMID[(index % 9) as usize]) * FRAME
		}
	}
}

/// Flush `held` frames read with nothing settled, as at a track's end.
fn drain(c: &mut Criterion) {
	let mut group = c.benchmark_group("decode_clock_drain");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for shape in ["flat", "backward", "pyramid"] {
		for held in [1_000u64, 10_000, 100_000] {
			for size in [64usize, 4_096] {
				let payload = Bytes::from(vec![0; size]);
				group.throughput(Throughput::Elements(held));
				group.bench_function(BenchmarkId::new(format!("{shape}/{size}B"), held), |b| {
					b.iter(|| {
						let mut clock = decode::DecodeClock::default();
						for index in 0..held {
							clock.push(payload.clone(), pts(shape, index), Instant::ZERO, 0);
						}
						let mut drained = 0;
						while clock.pop(decode::MAX_REORDER, 0, decode::Settle::All).is_some() {
							drained += 1;
						}
						assert_eq!(drained, held);
					})
				});
			}
		}
	}
	group.finish();
}

/// Read `frames` frames 1 ms apart, a 120 ms lookahead and 500 ms delay, settling each as an
/// exporter does: flat or backward PTS holds what the overdue rule lets through.
fn read(c: &mut Criterion) {
	let mut group = c.benchmark_group("decode_clock_read");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	let lookahead = 120 * 90;
	let delay = Duration::from_millis(500);
	for shape in ["flat", "backward", "pyramid"] {
		for frames in [1_000u64, 10_000, 100_000] {
			group.throughput(Throughput::Elements(frames));
			group.bench_function(BenchmarkId::new(shape, frames), |b| {
				b.iter(|| {
					let mut clock = decode::DecodeClock::default();
					let (mut held, mut most) = (0, 0);
					for index in 0..frames {
						let latest = Instant::ZERO + Duration::from_millis(index);
						clock.push(index, pts(shape, index), latest, 0);
						held += 1;
						loop {
							let settle = clock.settle(lookahead, latest, delay, usize::MAX);
							if clock.pop(lookahead, 0, settle).is_none() {
								break;
							}
							held -= 1;
						}
						most = most.max(held);
					}
					// What arrives in the deepest lookahead and the delay, a frame a millisecond.
					assert!(most <= 2_501, "{shape}: {most} held");
				})
			});
		}
	}
	group.finish();
}

criterion_group!(benches, drain, read);
criterion_main!(benches);

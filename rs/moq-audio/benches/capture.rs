//! Shared capture publication cost, swept over sources and independent readers.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_audio::capture::{Device, Source};
use moq_audio::{Error, Layout, encode};
use moq_net::time::Instant;

// Exercise the actual history implementation without opening hardware or
// exporting a synthetic device API solely for the benchmark.
#[allow(
	dead_code,
	reason = "compile the private capture history to benchmark it without device I/O"
)]
#[path = "../src/capture/fanout.rs"]
mod fanout;

fn capture(c: &mut Criterion) {
	let mut group = c.benchmark_group("capture_fanout");
	for sources in [1, 4, 16] {
		for readers in [1, 4, 16] {
			let producers: Vec<_> = (0..sources)
				.map(|_| fanout::Producer::new(encode::Input::new(48_000, Layout::Stereo), Source::default(), None))
				.collect();
			let mut consumers: Vec<Vec<_>> = producers
				.iter()
				.map(|producer| (0..readers).map(|_| producer.consume()).collect())
				.collect();
			let data: Arc<[f32]> = vec![0.25; 960].into();
			let captured = Instant::now();
			let waiter = kio::Waiter::noop();
			group.throughput(Throughput::Elements((sources * 480) as u64));
			group.bench_function(BenchmarkId::new(format!("sources_{sources}"), readers), |b| {
				b.iter(|| {
					for (producer, readers) in producers.iter().zip(&mut consumers) {
						producer.write(fanout::Buffer {
							data: data.clone(),
							captured,
							gap: false,
						});
						for reader in readers {
							assert!(matches!(
								black_box(reader.poll_read(&waiter)),
								std::task::Poll::Ready(Ok(_))
							));
						}
					}
				})
			});
		}
	}
	group.finish();
}

criterion_group!(benches, capture);
criterion_main!(benches);

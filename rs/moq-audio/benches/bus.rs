//! Program bus cost per 10 ms block, swept over the number of channels and
//! whether they need resampling.
//!
//! Run with `cargo bench -p moq-audio --bench bus`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_audio::bus::{Bus, Config, Strip};
use moq_audio::encode::Input;
use moq_audio::{Format, Layout};

/// The bus's 10 ms block at its default 48 kHz.
const BLOCK: usize = 480;

fn bench_mix(c: &mut Criterion) {
	let mut group = c.benchmark_group("bus_mix");
	group.throughput(Throughput::Elements(BLOCK as u64));

	for rate in [48_000u32, 44_100] {
		for channels in [1usize, 4, 16] {
			let bus = Bus::new(Config::default()).unwrap();
			let frames = BLOCK * rate as usize / 48_000;
			let block: Vec<u8> = std::iter::repeat_n(0.25f32, frames * 2)
				.flat_map(f32::to_le_bytes)
				.collect();
			let mut inputs: Vec<_> = (0..channels)
				.map(|_| {
					let mut input = Input::new(rate, Layout::Stereo);
					input.format = Format::F32;
					bus.channel(input, Strip::default()).unwrap()
				})
				.collect();
			let mut out = vec![0.0f32; BLOCK * 2];
			// Writes before the first read are dropped, so open the channels first.
			bus.read(&mut out);

			group.bench_function(BenchmarkId::new(format!("{rate}hz"), channels), |b| {
				b.iter(|| {
					for input in &mut inputs {
						input.write(black_box(&block)).unwrap();
					}
					bus.read(black_box(&mut out));
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench_mix);
criterion_main!(benches);

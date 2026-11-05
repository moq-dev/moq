//! Bus cost per 10 ms block along both axes: sources and mixes.
//!
//! - `bus_sources`: one mix of every source, swept over the source count and
//!   whether the sources need resampling.
//! - `bus_mixes`: 16 sources, swept over the mix count. `full` routes every
//!   source into every mix; `sparse` routes one source into each, which should
//!   cost little more than the sources alone.
//! - `bus_minus`: a program of every source plus a mix-minus per source, the
//!   per-guest return feed, which should grow with the source count rather
//!   than its square.
//!
//! Run with `cargo bench -p moq-audio --bench bus`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_audio::bus::{Bus, Channel, Config, Driver, Strip};
use moq_audio::encode::Input;
use moq_audio::{Format, Layout};

/// The bus's 10 ms block at its default 48 kHz.
const BLOCK: usize = 480;

/// A bus with `count` stereo sources at `rate`, opened by a first advance, and
/// one block of PCM for each.
fn sources(count: usize, rate: u32) -> (Bus, Driver, Vec<(Channel, Strip)>, Vec<u8>) {
	let (bus, mut driver) = Bus::new(Config::default()).unwrap();
	let frames = BLOCK * rate as usize / 48_000;
	let block: Vec<u8> = std::iter::repeat_n(0.25f32, frames * 2)
		.flat_map(f32::to_le_bytes)
		.collect();
	let sources = (0..count)
		.map(|_| {
			let mut input = Input::new(rate, Layout::Stereo);
			input.format = Format::F32;
			let strip = Strip::default();
			(bus.channel(input, strip.clone()).unwrap(), strip)
		})
		.collect();
	// Writes before the first advance are dropped, so open the channels first.
	driver.advance(BLOCK);
	(bus, driver, sources, block)
}

fn run(driver: &mut Driver, sources: &mut [(Channel, Strip)], block: &[u8]) {
	for (channel, _) in sources.iter_mut() {
		channel.write(black_box(block)).unwrap();
	}
	driver.advance(black_box(BLOCK));
}

fn bench_sources(c: &mut Criterion) {
	let mut group = c.benchmark_group("bus_sources");
	group.throughput(Throughput::Elements(BLOCK as u64));

	for rate in [48_000u32, 44_100] {
		for count in [1usize, 4, 16, 64] {
			let (bus, mut driver, mut sources, block) = sources(count, rate);
			let program = bus.mix();
			for (_, strip) in &sources {
				program.route(strip);
			}

			group.bench_function(BenchmarkId::new(format!("{rate}hz"), count), |b| {
				b.iter(|| run(&mut driver, &mut sources, &block));
			});
		}
	}
	group.finish();
}

fn bench_mixes(c: &mut Criterion) {
	let mut group = c.benchmark_group("bus_mixes");
	group.throughput(Throughput::Elements(BLOCK as u64));

	for routing in ["full", "sparse"] {
		for count in [1usize, 4, 16, 64] {
			let (bus, mut driver, mut sources, block) = sources(16, 48_000);
			let mixes: Vec<_> = (0..count)
				.map(|index| {
					let mix = bus.mix();
					match routing {
						"full" => sources.iter().for_each(|(_, strip)| mix.route(strip)),
						_ => mix.route(&sources[index % sources.len()].1),
					}
					mix
				})
				.collect();

			group.bench_function(BenchmarkId::new(routing, count), |b| {
				b.iter(|| run(&mut driver, &mut sources, &block));
			});
			drop(mixes);
		}
	}
	group.finish();
}

fn bench_minus(c: &mut Criterion) {
	let mut group = c.benchmark_group("bus_minus");
	group.throughput(Throughput::Elements(BLOCK as u64));

	for count in [1usize, 4, 16, 64] {
		let (bus, mut driver, mut sources, block) = sources(count, 48_000);
		let program = bus.mix();
		for (_, strip) in &sources {
			program.route(strip);
		}
		let minuses: Vec<_> = sources.iter().map(|(_, strip)| program.minus(strip)).collect();

		group.bench_function(BenchmarkId::from_parameter(count), |b| {
			b.iter(|| run(&mut driver, &mut sources, &block));
		});
		drop(minuses);
	}
	group.finish();
}

criterion_group!(benches, bench_sources, bench_mixes, bench_minus);
criterion_main!(benches);

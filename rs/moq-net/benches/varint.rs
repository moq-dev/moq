//! Lite-07 leading-ones varints against lite-06 QUIC varints: the bytes and the CPU to
//! encode and decode what a publisher writes per frame, per group, and per request.
//!
//! The two versions share these layouts, so the only difference is the varint codec.
//! The byte counts print once before the timings.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench varint`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{Version, fuzz::LiteSample};

fn versions() -> [(&'static str, Version); 2] {
	["moq-lite-06", "moq-lite-07-wip"].map(|name| (name, name.parse().unwrap()))
}

fn bench(c: &mut Criterion) {
	println!("{:<10} {:>8} {:>8}", "sample", "lite-06", "lite-07");
	for sample in LiteSample::ALL {
		let [lite06, lite07] = versions().map(|(_, version)| sample.encode(version).len());
		println!("{:<10} {lite06:>8} {lite07:>8}", format!("{sample:?}"));
	}

	let mut group = c.benchmark_group("lite_varint");
	for sample in LiteSample::ALL {
		for (name, version) in versions() {
			let id = format!("{sample:?}/{name}");
			group.bench_function(BenchmarkId::new("encode", &id), |b| {
				b.iter(|| black_box(sample.encode(black_box(version))))
			});

			let wire = sample.encode(version);
			group.bench_function(BenchmarkId::new("decode", &id), |b| {
				b.iter(|| black_box(sample.decode(black_box(version), black_box(&wire))))
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

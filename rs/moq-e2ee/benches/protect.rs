//! AES-128-GCM throughput for grouped frames and datagrams.
//!
//! Run with `cargo bench -p moq-e2ee`.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_e2ee::{MAX_GROUPED_PAYLOAD, protect};

fn bench_protect(c: &mut Criterion) {
	let key = [0x11u8; 16];
	let grouped = vec![0u8; 1024];
	let opus = vec![0u8; 160];

	let mut group = c.benchmark_group("protect");
	group.throughput(Throughput::Bytes(grouped.len() as u64));
	group.bench_function("grouped_1k", |b| {
		let mut seq = 0u64;
		b.iter(|| {
			seq += 1;
			protect(&key, black_box(seq), 0, black_box(&grouped), MAX_GROUPED_PAYLOAD).unwrap()
		});
	});
	group.throughput(Throughput::Bytes(opus.len() as u64));
	group.bench_function("datagram_opus160", |b| {
		let mut seq = 0u64;
		b.iter(|| {
			seq += 1;
			protect(&key, black_box(seq), 0, black_box(&opus), 1196).unwrap()
		});
	});
	group.finish();
}

criterion_group!(benches, bench_protect);
criterion_main!(benches);

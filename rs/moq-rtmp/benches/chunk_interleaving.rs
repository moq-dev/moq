//! Decode one hot CSID while other CSIDs retain incomplete messages.
//! Sweep the table size independently of message size and chunk count.

#[path = "../src/rml/mod.rs"]
mod rml;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rml::chunk_io::ChunkDeserializer;
use std::hint::black_box;

fn message(csid: u16, length: usize, chunk: usize) -> Vec<u8> {
	let basic = if csid < 64 {
		vec![csid as u8]
	} else {
		vec![0, (csid - 64) as u8]
	};
	let mut bytes = basic.clone();
	bytes.extend_from_slice(&[0, 0, 1]);
	bytes.extend_from_slice(&(length as u32).to_be_bytes()[1..]);
	bytes.extend_from_slice(&[9, 1, 0, 0, 0]);
	bytes.extend(std::iter::repeat_n(42, length.min(chunk)));
	for start in (chunk..length).step_by(chunk) {
		let mut continuation = basic.clone();
		continuation[0] |= 0xc0;
		bytes.extend_from_slice(&continuation);
		bytes.extend(std::iter::repeat_n(42, chunk.min(length - start)));
	}
	bytes
}

fn bench(c: &mut Criterion) {
	let mut group = c.benchmark_group("rtmp_chunks");
	for ids in [1, 16, 64, 256] {
		for length in [128, 4096, 65536] {
			for chunk in [128, 4096] {
				let mut decoder = ChunkDeserializer::new();
				decoder.set_max_chunk_size(chunk).unwrap();
				for csid in 3..(ids + 2) {
					let bytes = message(csid, chunk * 2, chunk);
					let header = if csid < 64 { 12 } else { 13 };
					assert!(decoder.get_next_message(&bytes[..header + chunk]).unwrap().is_none());
				}
				let bytes = message(2, length, chunk);
				group.throughput(Throughput::Bytes(length as u64));
				group.bench_function(BenchmarkId::new(format!("ids_{ids}/chunk_{chunk}"), length), |b| {
					b.iter(|| {
						let payload = decoder.get_next_message(black_box(&bytes)).unwrap().unwrap();
						assert_eq!(payload.data.len(), length);
						black_box(payload);
						assert!(decoder.get_next_message(&[]).unwrap().is_none());
					});
				});
			}
		}
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

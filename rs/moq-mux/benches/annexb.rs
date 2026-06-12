//! Compares the memchr-based `find_start_code` against the original
//! byte-at-a-time scalar scanner it replaced.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::codec::annexb::find_start_code;
use std::hint::black_box;

/// The original scalar scanner, kept here as a baseline to benchmark against.
/// The production code now uses `find_start_code` (memchr substring search).
fn find_start_code_scalar(mut b: &[u8]) -> Option<(usize, usize)> {
	let size = b.len();

	while b.len() >= 3 {
		match b[2] {
			0 if b.len() >= 4 => match b[3] {
				1 => match b[1] {
					0 => match b[0] {
						0 => return Some((size - b.len(), 4)),
						_ => return Some((size - b.len() + 1, 3)),
					},
					_ => b = &b[4..],
				},
				0 => b = &b[1..],
				_ => b = &b[4..],
			},
			0 => return None,
			1 => match b[1] {
				0 => match b[0] {
					0 => return Some((size - b.len(), 3)),
					_ => b = &b[3..],
				},
				_ => b = &b[3..],
			},
			_ => b = &b[3..],
		}
	}

	None
}

/// Deterministic xorshift so the benchmark inputs are stable across runs.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		let mut x = self.0;
		x ^= x << 13;
		x ^= x >> 7;
		x ^= x << 17;
		self.0 = x;
		x
	}

	fn byte(&mut self) -> u8 {
		(self.next() >> 24) as u8
	}
}

/// One NAL of random payload terminated by a 4-byte start code. The payload
/// avoids accidental `00 00 01` runs so the scan reaches the planted code.
fn nal(rng: &mut Rng, len: usize, out: &mut Vec<u8>) {
	let mut zeros = 0;
	for _ in 0..len {
		let mut b = rng.byte();
		// Emulation-prevention guarantees no 3 consecutive zeros in real bitstreams.
		if b == 0 && zeros >= 2 {
			b = 0xff;
		}
		zeros = if b == 0 { zeros + 1 } else { 0 };
		out.push(b);
	}
	out.extend_from_slice(&[0, 0, 0, 1]);
}

/// A typical stream: many NALs a few KB apart. The scan finds a start code quickly.
fn typical_stream() -> Vec<u8> {
	let mut rng = Rng(0x1234_5678);
	let mut out = Vec::new();
	for _ in 0..256 {
		nal(&mut rng, 2048, &mut out);
	}
	out
}

/// Worst case for the scalar walk: one huge NAL with no start code until the
/// very end, sprinkled with `00 00 xx` near-misses that defeat the 3rd-byte skip.
fn sparse_with_near_misses() -> Vec<u8> {
	let mut rng = Rng(0x9e37_79b9);
	let mut out = Vec::new();
	// Only 0x00 and 0xff, biased toward zeros: lots of `00 00` near-misses that
	// defeat the scalar 3rd-byte skip, but never a `1` so no real start code
	// appears until the one we plant at the end. The scan must cover the whole MB.
	for _ in 0..(1 << 20) {
		out.push(if rng.byte() < 96 { 0 } else { 0xff });
	}
	out.extend_from_slice(&[0, 0, 0, 1]);
	out
}

fn bench(c: &mut Criterion) {
	let inputs = [
		("typical_2kb_nals", typical_stream()),
		("sparse_1mb_near_misses", sparse_with_near_misses()),
	];

	for (name, data) in &inputs {
		let mut group = c.benchmark_group(*name);
		group.throughput(Throughput::Bytes(data.len() as u64));

		group.bench_function("memchr", |b| b.iter(|| black_box(find_start_code(black_box(data)))));
		group.bench_function("scalar", |b| {
			b.iter(|| black_box(find_start_code_scalar(black_box(data))))
		});

		group.finish();
	}
}

criterion_group!(benches, bench);
criterion_main!(benches);

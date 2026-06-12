//! Benchmarks several `find_start_code` implementations against each other:
//!
//! - `naive`:  check every byte triplet for `00 00 01`.
//! - `scalar`: the original branch-per-byte scanner that skips up to 4 bytes.
//! - `memmem`: the production `find_start_code`, a SIMD substring search for `00 00 01`.
//! - `memchr`: SIMD scan for the sparse `0x01` byte, then a cheap `00 00` head check.
//!
//! All four must agree on every input (asserted before timing), so the only
//! difference measured is speed.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::codec::annexb::find_start_code;
use std::hint::black_box;

/// Truly naive baseline: walk one byte at a time looking for `00 00 01`.
fn find_start_code_naive(b: &[u8]) -> Option<(usize, usize)> {
	let mut i = 0;
	while i + 3 <= b.len() {
		if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
			if i > 0 && b[i - 1] == 0 {
				return Some((i - 1, 4));
			}
			return Some((i, 3));
		}
		i += 1;
	}
	None
}

/// The original scalar scanner that this PR replaced, kept here as a baseline.
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

/// memchr variant: a start code's `01` byte sits at index >= 2, and `0x01` is
/// sparse (~1/256) in random data, so scan for it with a single-byte SIMD search
/// and confirm the two preceding bytes are `00 00` (a cheap 16-bit compare).
fn find_start_code_memchr(b: &[u8]) -> Option<(usize, usize)> {
	if b.len() < 3 {
		return None;
	}

	let one = memchr::memchr_iter(0x01, &b[2..])
		.map(|idx| idx + 2)
		.find(|&idx| b[idx - 2..idx] == [0, 0])?;

	let start = one - 2;
	if start > 0 && b[start - 1] == 0 {
		Some((start - 1, 4))
	} else {
		Some((start, 3))
	}
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

/// Walk the whole buffer NAL by NAL, returning how many start codes were found.
/// Mimics real iterator usage: many `find` calls on a shrinking buffer, where
/// per-call setup cost (e.g. memmem's) is paid once per NAL instead of amortized.
fn split_all(mut b: &[u8], find: fn(&[u8]) -> Option<(usize, usize)>) -> usize {
	let mut count = 0;
	while let Some((pos, size)) = find(b) {
		let next = pos + size;
		if next >= b.len() {
			break;
		}
		b = &b[next..];
		count += 1;
	}
	count
}

const IMPLS: &[(&str, fn(&[u8]) -> Option<(usize, usize)>)] = &[
	("naive", find_start_code_naive),
	("scalar", find_start_code_scalar),
	("memmem", find_start_code),
	("memchr", find_start_code_memchr),
];

fn bench(c: &mut Criterion) {
	let inputs = [
		("typical_2kb_nals", typical_stream()),
		("sparse_1mb_near_misses", sparse_with_near_misses()),
	];

	// All implementations must agree before we trust the timings.
	for (name, data) in &inputs {
		let want = find_start_code_naive(data);
		for (label, f) in IMPLS {
			assert_eq!(f(data), want, "{label} disagrees on single-find for {name}");
		}
	}

	// Single find: locate the next start code in one buffer.
	for (name, data) in &inputs {
		let mut group = c.benchmark_group(format!("find/{name}"));
		group.throughput(Throughput::Bytes(data.len() as u64));
		for (label, f) in IMPLS {
			group.bench_function(*label, |b| b.iter(|| black_box(f(black_box(data)))));
		}
		group.finish();
	}

	// Split-all: repeated finds across the whole stream, NAL by NAL.
	let (name, data) = &inputs[0];
	let want = split_all(data, find_start_code_naive);
	for (label, f) in IMPLS {
		assert_eq!(split_all(data, *f), want, "{label} disagrees on split count");
	}

	let mut group = c.benchmark_group(format!("split_all/{name}"));
	group.throughput(Throughput::Bytes(data.len() as u64));
	for (label, f) in IMPLS {
		group.bench_function(*label, |b| b.iter(|| black_box(split_all(black_box(data), *f))));
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

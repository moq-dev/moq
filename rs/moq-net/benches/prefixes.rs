//! What a peer's grant costs to read: collecting the prefixes an `AUTH_OK` carries, and
//! comparing them with the previous grant, swept over encoded size up to a whole message.
//!
//! Run with `cargo bench -p moq-net --bench prefixes`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Prefix, Prefixes};

fn grants(c: &mut Criterion) {
	let mut group = c.benchmark_group("prefixes");
	// Prefixes reduce in one sort and one sweep, so every row should grow as n log n.
	for bytes in [4096, 16384, MAX_MESSAGE] {
		let (message, previous) = grant(bytes);
		let size = wire(&message);
		assert!(
			size <= bytes && size * 16 > bytes * 15,
			"prefixes fill {size} of {bytes} bytes"
		);
		let message: Vec<Prefix> = message.iter().map(|text| text.parse().unwrap()).collect();
		let reduced: Prefixes = message.iter().cloned().collect();
		assert_eq!(reduced.len(), message.len(), "members are disjoint");
		assert!(previous.covers(&reduced));
		group.throughput(Throughput::Bytes(size as u64));
		group.bench_function(BenchmarkId::new("collect", bytes), |b| {
			b.iter(|| std::hint::black_box(message.iter().cloned().collect::<Prefixes>().len()));
		});
		group.bench_function(BenchmarkId::new("covers", bytes), |b| {
			b.iter(|| std::hint::black_box(previous.covers(&reduced)));
		});
		group.bench_function(BenchmarkId::new("intersect", bytes), |b| {
			b.iter(|| std::hint::black_box(previous.intersect(&reduced).len()));
		});
	}
	group.finish();
}

/// Distinct members two segments beneath a shared parent, filling at most `bytes` encoded
/// and given in reverse order so collecting sorts them, with a previous grant of their
/// parents.
fn grant(bytes: usize) -> (Vec<String>, Prefixes) {
	let (mut texts, mut parents) = (Vec::new(), Vec::new());
	let mut size = 0;
	for index in 0.. {
		let parent = format!("room/{index:05}");
		let text = format!("{parent}/cam");
		size += varint(text.len()) + text.len();
		if varint(texts.len() + 1) + size > bytes {
			break;
		}
		texts.push(text);
		parents.push(parent.parse::<Prefix>().unwrap());
	}
	texts.reverse();
	(texts, parents.into_iter().collect())
}

/// The largest moq-lite control message, and so the largest `AUTH_OK`.
const MAX_MESSAGE: usize = u16::MAX as usize;

/// A QUIC varint's size, as `AUTH_OK` writes counts and lengths.
fn varint(value: usize) -> usize {
	match value {
		0..64 => 1,
		64..16384 => 2,
		_ => 4,
	}
}

/// The encoded size of a list of prefixes: a count, then each length and its bytes.
fn wire(texts: &[String]) -> usize {
	varint(texts.len()) + texts.iter().map(|text| varint(text.len()) + text.len()).sum::<usize>()
}

criterion_group!(benches, grants);
criterion_main!(benches);

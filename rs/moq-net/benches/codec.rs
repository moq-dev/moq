//! The wire codec on its own: a fixed mix of moq-lite and moq-transport messages, and raw
//! varints in each wire form. Every message and frame header pays this cost.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench codec`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::{Messages, decode_varints, encode_varints};

type Encode = fn(&Messages, &mut Vec<u8>);
type Decode = fn(&Messages, &[u8]);

/// Varints spanning the length classes of both wire forms.
fn varints() -> Vec<u64> {
	(0..1_024u64)
		.map(|n| match n % 4 {
			0 => n % 60,
			1 => 1_000 + n,
			2 => 1_000_000 + n,
			_ => (1 << 40) + n,
		})
		.collect()
}

fn bench(c: &mut Criterion) {
	let messages = Messages::default();

	let mut group = c.benchmark_group("codec_messages");
	let protocols: [(&str, Encode, Decode); 2] = [
		("lite", Messages::encode_lite, Messages::decode_lite),
		("ietf", Messages::encode_ietf, Messages::decode_ietf),
	];
	for (name, encode, decode) in protocols {
		let mut encoded = Vec::new();
		encode(&messages, &mut encoded);
		group.throughput(Throughput::Bytes(encoded.len() as u64));

		group.bench_function(BenchmarkId::new("encode", name), |b| {
			let mut out = Vec::with_capacity(encoded.len());
			b.iter(|| {
				out.clear();
				encode(&messages, &mut out);
			});
		});
		group.bench_function(BenchmarkId::new("decode", name), |b| {
			b.iter(|| decode(&messages, &encoded))
		});
	}
	group.finish();

	let values = varints();
	let mut group = c.benchmark_group("codec_varint");
	group.throughput(Throughput::Elements(values.len() as u64));
	for (name, ietf) in [("quic", false), ("leading_ones", true)] {
		let mut encoded = Vec::new();
		encode_varints(&values, ietf, &mut encoded);

		group.bench_function(BenchmarkId::new("encode", name), |b| {
			let mut out = Vec::with_capacity(encoded.len());
			b.iter(|| {
				out.clear();
				encode_varints(&values, ietf, &mut out);
			});
		});
		group.bench_function(BenchmarkId::new("decode", name), |b| {
			b.iter(|| decode_varints(&encoded, ietf))
		});
	}
	group.finish();
}

/// An outgoing request reads the counted aggregate and checks only its named group,
/// independent of the audience beside it and the cache's unrelated groups.
fn compressed_request(c: &mut Criterion) {
	use moq_net::{broadcast, track};
	use std::hint::black_box;
	let mut bench = c.benchmark_group("codec_compressed_request");
	for groups in [1, 64, 1024] {
		for readers in [1, 64, 1024] {
			for held in [false, true] {
				let broadcast = broadcast::Info::default().produce();
				let track = broadcast
					.create_track("compressed", track::Info::default().with_compress(true))
					.unwrap();
				for _ in 0..groups {
					let mut group = track.append_group().unwrap();
					group.write_frame(None, bytes::Bytes::from_static(b"prefix")).unwrap();
					group.finish().unwrap();
				}
				let sequence = if held { groups - 1 } else { groups };
				let subscription = track::Subscription::default().with_ranges([track::Range {
					start: track::Anchor::Absolute(track::Position {
						group: sequence,
						frame: 1,
					}),
					end: None,
				}]);
				let _readers: Vec<_> = (0..readers).map(|_| track.subscribe(subscription.clone())).collect();
				bench.bench_function(BenchmarkId::new(format!("held_{held}_{readers}r"), groups), |b| {
					b.iter(|| {
						let mut subscription = track.subscription().unwrap();
						moq_net::fuzz::widen_compressed(&track, &mut subscription);
						black_box(subscription);
					});
				});
			}
		}
	}
	bench.finish();
}

criterion_group!(benches, bench, compressed_request);
criterion_main!(benches);

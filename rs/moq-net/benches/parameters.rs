//! SETUP parameter bags, swept from a small handshake to a message-sized bag.
//!
//! Keep the full fuzz round trip in its own executable: its broad codec call graph
//! perturbs message-codec timings even when this group is filtered out.
//! Run with `cargo bench -p moq-net --features fuzz --bench parameters`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::{encode_varints, lite_wire};

fn bench(c: &mut Criterion) {
	let mut group = c.benchmark_group("setup_parameters");
	for count in [4, 16, 64, 256, 1024, 4096] {
		let values: Vec<u64> = std::iter::once(count)
			.chain((100..100 + count).flat_map(|kind| [kind, 0]))
			.collect();
		// The existing fuzz adapter round-trips the private parameter codec on lite-07.
		let mut wire = vec![6, 20];
		encode_varints(&values, true, &mut wire);
		group.throughput(Throughput::Elements(count));
		group.bench_with_input(BenchmarkId::from_parameter(count), &wire, |b, wire| {
			assert!(lite_wire(wire));
			b.iter(|| lite_wire(std::hint::black_box(wire)));
		});
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

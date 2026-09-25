//! Lite-07 announce compression against lite-06 literal framing: the bytes an
//! announce stream costs, and the CPU to encode and decode it.
//!
//! Swept over live routes and churn. Each stream announces `routes` routes, then
//! replaces `churn` of them (an END and a fresh START), so the bases the encoder
//! picks come and go. Two workloads bracket the gain:
//!
//! - health: `<pid>/private/channel_N/stream-health-<ts>` from a few origins behind
//!   a shared pair of relays, the shape that motivated compression.
//! - unique: random single-segment paths and random two-hop chains, where nothing
//!   is shared and every START pays the four zero base/keep bytes.
//!
//! The byte counts print once before the timings.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench announce`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{
	Hop, Hops, PathOwned,
	fuzz::{Announced, decode_announces, encode_announces},
};

/// `(routes, churn)` shapes.
const SHAPES: [(u64, u64); 4] = [(100, 0), (1_000, 0), (1_000, 1_000), (10_000, 1_000)];

/// How many origins publish health streams, each behind the same two relays.
const ORIGINS: u64 = 8;

/// A deterministic 62-bit id, standing in for a random Hop ID.
fn id(seed: u64) -> u64 {
	// SplitMix64, masked to the varint range and kept non-zero.
	let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	((z ^ (z >> 31)) & ((1 << 62) - 1)).max(1)
}

fn hops(ids: &[u64]) -> Hops {
	Hops::try_from(ids.iter().map(|id| Hop::new(*id).unwrap()).collect::<Vec<_>>()).unwrap()
}

/// Announce `routes` routes, then replace `churn` of them, oldest first.
fn stream(routes: u64, churn: u64, route: impl Fn(u64) -> (PathOwned, Hops)) -> Vec<Announced> {
	let mut announced: Vec<_> = (0..routes)
		.map(|n| {
			let (path, hops) = route(n);
			Announced::Start(path, hops)
		})
		.collect();
	for n in 0..churn {
		announced.push(Announced::End(n));
		let (path, hops) = route(routes + n);
		announced.push(Announced::Start(path, hops));
	}
	announced
}

fn health(n: u64) -> (PathOwned, Hops) {
	let origin = n % ORIGINS;
	let path = format!(
		"{:x}/private/channel_{}/stream-health-{}",
		id(origin),
		n % 16,
		1_700_000_000 + n
	);
	(path.into(), hops(&[id(origin), id(100), id(101)]))
}

fn unique(n: u64) -> (PathOwned, Hops) {
	(
		format!("{:x}", id(n)).into(),
		hops(&[id(n + 1_000_000), id(n + 2_000_000)]),
	)
}

type Workload = (&'static str, fn(u64) -> (PathOwned, Hops));
const WORKLOADS: [Workload; 2] = [("health", health), ("unique", unique)];

fn report() {
	eprintln!("announce stream bytes (lite-06 literal -> lite-07 compressed):");
	for (name, route) in WORKLOADS {
		for (routes, churn) in SHAPES {
			let announced = stream(routes, churn, route);
			let literal = encode_announces(&announced, false).len();
			let compressed = encode_announces(&announced, true).len();
			let starts = routes + churn;
			eprintln!(
				"  {name:>6} routes={routes:>5} churn={churn:>5}: {literal:>8} -> {compressed:>8} bytes, {:.1} -> {:.1} per START ({:+.0}%)",
				literal as f64 / starts as f64,
				compressed as f64 / starts as f64,
				(compressed as f64 / literal as f64 - 1.0) * 100.0,
			);
		}
	}
}

fn bench(c: &mut Criterion) {
	report();

	for (name, route) in WORKLOADS {
		let mut group = c.benchmark_group(format!("announce_{name}"));
		for (routes, churn) in SHAPES {
			let announced = stream(routes, churn, route);
			let shape = format!("{routes}x{churn}");
			group.throughput(Throughput::Elements(announced.len() as u64));

			for compress in [false, true] {
				let version = if compress { "lite07" } else { "lite06" };
				let encoded = encode_announces(&announced, compress);

				group.bench_with_input(
					BenchmarkId::new(format!("encode_{version}"), &shape),
					&announced,
					|b, announced| b.iter(|| encode_announces(announced, compress)),
				);
				group.bench_with_input(
					BenchmarkId::new(format!("decode_{version}"), &shape),
					&encoded,
					|b, encoded| b.iter(|| decode_announces(encoded, compress)),
				);
			}
		}
		group.finish();
	}
}

criterion_group!(benches, bench);
criterion_main!(benches);

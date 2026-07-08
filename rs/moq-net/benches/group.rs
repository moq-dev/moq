//! Per-frame overhead benchmarks for the group model.
//!
//! The point of interest is small frames: today each frame in a group is a
//! `frame::Producer` owning its own `kio` channel plus a couple of `Arc`s, so a
//! group with thousands of tiny frames allocates thousands of tiny control
//! objects. These benchmarks write and read many small frames so that cost shows
//! up as wall-clock time, giving a before/after for reshaping frames into plain
//! data.
//!
//! Run with `cargo bench -p moq-net`.

use std::hint::black_box;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Timestamp, broadcast, group, track};

/// A small, fixed payload shared across every frame. Cloning a `Bytes` is a
/// refcount bump (no allocation), so the benchmark isolates the per-frame control
/// overhead rather than payload allocation.
const PAYLOAD: usize = 64;

/// Frame counts to sweep. The top end intentionally reaches the raised
/// `MAX_GROUP_FRAMES` so a full group of tiny frames is exercised.
const COUNTS: [usize; 3] = [512, 8_192, 32_768];

/// Keeps the broadcast/track producers alive alongside the group so the group
/// isn't torn down mid-benchmark. Only `group` is written to.
struct Ctx {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	group: group::Producer,
}

/// Build a fresh, empty group via the public producer path.
fn fresh_group() -> Ctx {
	let mut broadcast = broadcast::Producer::new(broadcast::Info::default());
	let mut track = broadcast.create_track("bench", None).unwrap();
	let group = track.append_group().unwrap();
	Ctx {
		_broadcast: broadcast,
		_track: track,
		group,
	}
}

/// Write N small frames into a fresh group (producer-side per-frame cost).
fn bench_write(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_write_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				fresh_group,
				|mut ctx| {
					for _ in 0..n {
						ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					// Return so the drop happens outside the timed region.
					ctx
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

/// Drain N small frames from a pre-filled group (consumer-side per-frame cost).
fn bench_read(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_read_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let mut ctx = fresh_group();
					for _ in 0..n {
						ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					ctx.group.finish().unwrap();
					let consumer = ctx.group.consume();
					(ctx, consumer)
				},
				|(ctx, mut consumer)| {
					for _ in 0..n {
						// All frames are already present and finished, so a single poll
						// resolves immediately (no runtime needed).
						let frame = consumer.read_frame().now_or_never().unwrap().unwrap();
						black_box(frame);
					}
					(ctx, consumer)
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

/// The full lifecycle: build a group, write N frames, then drain them.
fn bench_roundtrip(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_roundtrip");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				fresh_group,
				|mut ctx| {
					for _ in 0..n {
						ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					ctx.group.finish().unwrap();
					let mut consumer = ctx.group.consume();
					for _ in 0..n {
						let frame = consumer.read_frame().now_or_never().unwrap().unwrap();
						black_box(frame);
					}
					(ctx, consumer)
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

criterion_group!(benches, bench_write, bench_read, bench_roundtrip);
criterion_main!(benches);

//! Delivery-path benchmarks for the group and track models.
//!
//! The point of interest is small frames: today each frame in a group is a
//! `frame::Producer` owning its own `kio` channel plus a couple of `Arc`s, so a
//! group with thousands of tiny frames allocates thousands of tiny control
//! objects. These benchmarks write and read many small frames so that cost shows
//! up as wall-clock time, giving a before/after for reshaping frames into plain
//! data.
//!
//! `track_recv_groups` covers the layer above: how much it costs to hand out one
//! cached group, swept over cache depth so a per-delivery scan shows up as a slope.
//!
//! Run with `cargo bench -p moq-net`.

use std::hint::black_box;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Timestamp, broadcast, frame, group, track};

/// A small, fixed payload shared across every frame. Cloning a `Bytes` is a
/// refcount bump (no allocation), so the benchmark isolates the per-frame control
/// overhead rather than payload allocation.
const PAYLOAD: usize = 64;

/// Frame counts to sweep. The top end intentionally reaches the raised
/// `MAX_GROUP_FRAMES` so a full group of tiny frames is exercised.
const COUNTS: [usize; 3] = [512, 8_192, 32_768];

/// Cached group counts to sweep for the track-level delivery benchmarks. A track
/// publishing one group per frame at the default 5s retention sits in the hundreds,
/// so the top end is deliberately past anything realistic: a per-delivery scan over
/// the cache shows up as a slope here, while a seek stays flat.
const DEPTHS: [usize; 3] = [64, 512, 4_096];

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
///
/// `single` is one `write_frame` per frame: a group lock plus a track-level eviction
/// settle each. `batch32` fills a `frame::Buffer` and hands the whole thing to
/// `write_frames`, paying both once per batch.
fn bench_write(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_write_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("batch32", n), &n, |b, &n| {
			b.iter_batched(
				|| (fresh_group(), frame::Buffer::<32>::new()),
				|(mut ctx, mut buf)| {
					for _ in 0..n {
						let frame = frame::Frame {
							timestamp: Timestamp::ZERO,
							payload: payload.clone(),
						};
						// A full buffer hands the frame back: flush, then take it.
						if let Err(frame) = buf.push(frame) {
							ctx.group.write_frames(&mut buf).unwrap();
							buf.push(frame).unwrap();
						}
					}
					ctx.group.write_frames(&mut buf).unwrap();
					(ctx, buf)
				},
				BatchSize::SmallInput,
			);
		});
		g.bench_with_input(BenchmarkId::new("single", n), &n, |b, &n| {
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

/// A pre-filled, finished group plus a consumer positioned at its first frame.
fn filled_group(n: usize, payload: &Bytes) -> (Ctx, group::Consumer) {
	let mut ctx = fresh_group();
	for _ in 0..n {
		ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	ctx.group.finish().unwrap();
	let consumer = ctx.group.consume();
	(ctx, consumer)
}

/// Drain a whole group through a reused `frame::Buffer<N>`.
fn drain_batched<const N: usize>(consumer: &mut group::Consumer, buf: &mut frame::Buffer<N>) {
	loop {
		let batch = consumer.read_frames(buf).now_or_never().unwrap().unwrap();
		if batch.is_empty() {
			break;
		}
		for frame in batch.iter() {
			black_box(frame);
		}
	}
}

/// Drain N small frames from a pre-filled group (consumer-side per-frame cost).
///
/// `single` is one frame at a time via [`group::Consumer::read_frame`]: a lock and a
/// waker each. `batch{N}` is [`group::Consumer::read_frames`] cloning the ready tail
/// into a reused `N`-frame buffer, amortizing both across the batch.
///
/// Throughput keeps climbing to 32 and stalls after (128 falls out of L1), but the
/// default capacity is 8: the tail of that curve only pays off for a reader draining a
/// backlog, while every buffer pays its stack whether or not the batch fills.
fn bench_read(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_read_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("single", n), &n, |b, &n| {
			b.iter_batched(
				|| filled_group(n, &payload),
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

		// The buffer is allocated in setup and refilled for the whole group, like a
		// real reader that keeps one per task.
		macro_rules! batched {
			($cap:expr) => {
				g.bench_with_input(BenchmarkId::new(concat!("batch", $cap), n), &n, |b, &n| {
					b.iter_batched(
						|| {
							let (ctx, consumer) = filled_group(n, &payload);
							(ctx, consumer, frame::Buffer::<$cap>::new())
						},
						|(ctx, mut consumer, mut buf)| {
							drain_batched(&mut consumer, &mut buf);
							(ctx, consumer, buf)
						},
						BatchSize::SmallInput,
					);
				});
			};
		}

		batched!(4);
		batched!(8);
		batched!(32);
		batched!(128);
	}
	g.finish();
}

/// Keeps the broadcast alive alongside the track being drained.
struct TrackCtx {
	_broadcast: broadcast::Producer,
	track: track::Producer,
}

/// Build a track holding N cached groups, each with a single small frame.
fn filled_track(n: usize, payload: &Bytes) -> TrackCtx {
	let mut broadcast = broadcast::Producer::new(broadcast::Info::default());
	let mut track = broadcast.create_track("bench", None).unwrap();
	for _ in 0..n {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();
	}
	TrackCtx {
		_broadcast: broadcast,
		track,
	}
}

/// Drain N cached groups from a track, in arrival order and in sequence order.
///
/// The two differ in how they find the next group: `recv_group` walks an arrival
/// index, while `next_group` seeks by sequence. Both should be flat in the cache
/// depth; a per-delivery scan over the cache makes the sequence-ordered arm
/// quadratic in N.
fn bench_track_recv(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("track_recv_groups");
	for &n in &DEPTHS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("arrival", n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let ctx = filled_track(n, &payload);
					let subscriber = ctx.track.subscribe(None);
					(ctx, subscriber)
				},
				|(ctx, mut subscriber)| {
					for _ in 0..n {
						let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
						black_box(group);
					}
					(ctx, subscriber)
				},
				BatchSize::SmallInput,
			);
		});
		g.bench_with_input(BenchmarkId::new("sequence", n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let ctx = filled_track(n, &payload);
					let subscriber = ctx.track.subscribe(None);
					(ctx, subscriber)
				},
				|(ctx, mut subscriber)| {
					for _ in 0..n {
						let group = subscriber.next_group().now_or_never().unwrap().unwrap().unwrap();
						black_box(group);
					}
					(ctx, subscriber)
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

criterion_group!(benches, bench_write, bench_read, bench_roundtrip, bench_track_recv);
criterion_main!(benches);

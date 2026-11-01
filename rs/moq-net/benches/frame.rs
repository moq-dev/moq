//! Frame receive throughput: what allocating a frame's declared size up front saves
//! over growing its buffer with the bytes received, swept over frame size and the
//! number of streams receiving at once.
//!
//! `upfront` allocates every declared size, `grow` only what has arrived, and
//! `budget` is the session default, which allocates up front until the frames in
//! flight would pass it. Every stream reads a packet-sized chunk per turn.
//!
//! `frame_recv_large` sweeps frames up to the group's byte cap over fewer streams,
//! where a frame spans many storage segments.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench frame`.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::FrameRecv;

/// Roughly one QUIC packet's payload.
const CHUNK: usize = 1200;

const SIZES: [usize; 3] = [16 * 1024, 256 * 1024, 2 * 1024 * 1024];
const STREAMS: [usize; 3] = [1, 16, 64];

const LARGE_SIZES: [usize; 3] = [4 * 1024 * 1024, 16 * 1024 * 1024, 32 * 1024 * 1024];
const LARGE_STREAMS: [usize; 2] = [1, 4];

const BUDGETS: [(&str, Option<usize>); 3] = [("upfront", Some(usize::MAX)), ("grow", Some(0)), ("budget", None)];

fn sweep(c: &mut Criterion, name: &str, sizes: &[usize], streams: &[usize]) {
	let mut group = c.benchmark_group(name);
	for &size in sizes {
		for &streams in streams {
			group.throughput(Throughput::Bytes((size * streams) as u64));
			for (name, budget) in BUDGETS {
				let id = format!("size={size}/streams={streams}");
				group.bench_function(BenchmarkId::new(name, id), |b| {
					b.iter_batched(
						|| FrameRecv::new(streams, size, CHUNK, budget),
						FrameRecv::run,
						BatchSize::PerIteration,
					)
				});
			}
		}
	}
	group.finish();
}

fn bench(c: &mut Criterion) {
	sweep(c, "frame_recv", &SIZES, &STREAMS);
	sweep(c, "frame_recv_large", &LARGE_SIZES, &LARGE_STREAMS);
}

criterion_group!(benches, bench);
criterion_main!(benches);

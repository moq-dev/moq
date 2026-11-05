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
//! `frame_read` adds subscribers reading each frame after every chunk that arrives, as
//! a relay forwards a frame still streaming in, swept over publishers, readers, and
//! arrival chunks per frame. Readers keep every chunk until the frame ends, so the
//! chunks read before a `grow` buffer outgrew itself still hold the old one. A read
//! should cost nothing that grows with the readers: `FRAME_ALLOCS=1` prints the
//! allocations and bytes allocated per read instead of timing, past what the same
//! frames cost to receive with no readers.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench frame`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::fuzz::FrameRecv;

/// Roughly one QUIC packet's payload.
const CHUNK: usize = 1200;

const SIZES: [usize; 3] = [16 * 1024, 256 * 1024, 2 * 1024 * 1024];
const STREAMS: [usize; 3] = [1, 16, 64];

const LARGE_SIZES: [usize; 3] = [4 * 1024 * 1024, 16 * 1024 * 1024, 32 * 1024 * 1024];
const LARGE_STREAMS: [usize; 2] = [1, 4];

const BUDGETS: [(&str, Option<usize>); 3] = [("upfront", Some(usize::MAX)), ("grow", Some(0)), ("budget", None)];

/// One buffer, or a chain of segments, for the frames read while they stream in.
const READ_SIZES: [usize; 2] = [16 * 1024, 2 * 1024 * 1024];
const READ_BUDGETS: [(&str, Option<usize>); 2] = [("upfront", Some(usize::MAX)), ("grow", Some(0))];
const PUBLISHERS: [usize; 2] = [1, 4];
const READERS: [usize; 3] = [1, 16, 128];
/// Chunks each frame arrives in, every one read by every reader.
const ARRIVALS: [usize; 2] = [4, 32];

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

impl Counter {
	fn count(size: usize) {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
			ALLOCATED.fetch_add(size, Ordering::Relaxed);
		}
	}
}

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		Self::count(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

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

/// One `frame_read` case: its id and the frames it receives.
struct ReadCase {
	id: String,
	publishers: usize,
	size: usize,
	chunk: usize,
	budget: Option<usize>,
	readers: usize,
}

impl ReadCase {
	fn recv(&self, readers: usize) -> FrameRecv {
		FrameRecv::new(self.publishers, self.size, self.chunk, self.budget).with_readers(readers)
	}
}

fn read_cases() -> impl Iterator<Item = ReadCase> {
	READ_SIZES.into_iter().flat_map(|size| {
		READ_BUDGETS.into_iter().flat_map(move |(name, budget)| {
			PUBLISHERS.into_iter().flat_map(move |publishers| {
				READERS.into_iter().flat_map(move |readers| {
					ARRIVALS.into_iter().map(move |arrivals| ReadCase {
						id: format!("{name}/size={size}/publishers={publishers}/readers={readers}/arrivals={arrivals}"),
						publishers,
						size,
						chunk: size / arrivals,
						budget,
						readers,
					})
				})
			})
		})
	})
}

/// Allocations and bytes allocated while `recv` runs, and the chunks its readers read.
fn count(recv: FrameRecv) -> (usize, usize, usize) {
	ALLOCATIONS.store(0, Ordering::Relaxed);
	ALLOCATED.store(0, Ordering::Relaxed);
	COUNTING.store(true, Ordering::Relaxed);
	let reads = recv.run();
	COUNTING.store(false, Ordering::Relaxed);
	(
		ALLOCATIONS.load(Ordering::Relaxed),
		ALLOCATED.load(Ordering::Relaxed),
		reads,
	)
}

/// Print each `frame_read` case's allocations and bytes allocated per read, past what
/// receiving its frames costs alone, instead of timing. An argument filters the cases.
fn allocations() {
	let filter = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
	for case in read_cases().filter(|case| filter.as_ref().is_none_or(|filter| case.id.contains(filter))) {
		let (base_allocations, base_bytes, _) = count(case.recv(0));
		let (allocations, bytes, reads) = count(case.recv(case.readers));
		let per_read = |total: usize, base: usize| total.saturating_sub(base) as f64 / reads as f64;
		println!(
			"frame_read/{}: {reads} reads, {:.2} allocations, {:.1} bytes allocated per read",
			case.id,
			per_read(allocations, base_allocations),
			per_read(bytes, base_bytes),
		);
	}
}

fn bench_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("frame_read");
	group.warm_up_time(Duration::from_secs(1));
	group.measurement_time(Duration::from_secs(2));
	for case in read_cases() {
		group.throughput(Throughput::Elements((case.publishers * case.readers) as u64));
		group.bench_function(BenchmarkId::from_parameter(&case.id), |b| {
			b.iter_batched(|| case.recv(case.readers), FrameRecv::run, BatchSize::PerIteration)
		});
	}
	group.finish();
}

fn bench(c: &mut Criterion) {
	if std::env::var_os("FRAME_ALLOCS").is_some() {
		allocations();
		return;
	}
	sweep(c, "frame_recv", &SIZES, &STREAMS);
	sweep(c, "frame_recv_large", &LARGE_SIZES, &LARGE_STREAMS);
	bench_read(c);
}

criterion_group!(benches, bench);
criterion_main!(benches);

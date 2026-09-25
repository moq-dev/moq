//! Cost of decoding window frames into typed records, swept over records per header.
//!
//! Each run decodes one group header holding `records` records, then `records` push frames. The
//! frames are built outside the measured section.
//!
//! Run `cargo bench -p moq-json --bench window` and compare the table across revisions.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use moq_json::window::{ConsumerConfig, Decoder};

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// A stats-shaped record: flat counters plus one nested object, nothing owned on the heap.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct Record {
	bytes: u64,
	frames: u64,
	groups: u64,
	subscriptions: u64,
	nested: Nested,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct Nested {
	bytes: u64,
	frames: u64,
}

/// Runs measured per configuration.
const RUNS: u32 = 16;

fn record(index: usize) -> serde_json::Value {
	serde_json::json!({
		"bytes": index * 1500,
		"frames": index,
		"groups": index / 30,
		"subscriptions": 3,
		"nested": { "bytes": index * 1200, "frames": index },
	})
}

fn measure<R>(f: impl FnOnce() -> R) -> (usize, Duration) {
	ALLOCATIONS.store(0, Ordering::Relaxed);
	COUNTING.store(true, Ordering::Relaxed);
	let start = Instant::now();
	let out = f();
	let elapsed = start.elapsed();
	COUNTING.store(false, Ordering::Relaxed);
	drop(black_box(out));
	(ALLOCATIONS.load(Ordering::Relaxed), elapsed)
}

/// Mean allocations and time for the header, and per push.
fn run(records: usize) -> ((usize, Duration), (usize, Duration)) {
	let header = serde_json::to_vec(&serde_json::json!({
		"offset": 0,
		"records": (0..records).map(record).collect::<Vec<_>>(),
	}))
	.unwrap();
	let pushes: Vec<_> = (records..2 * records)
		.map(|index| serde_json::to_vec(&serde_json::json!({ "push": record(index) })).unwrap())
		.collect();

	let (mut header_allocs, mut header_time) = (0, Duration::ZERO);
	let (mut push_allocs, mut push_time) = (0, Duration::ZERO);
	for _ in 0..RUNS {
		let mut decoder = Decoder::<Record>::new(ConsumerConfig::default());
		let mut group = decoder.group();

		let (allocs, time) = measure(|| group.decode(&header).unwrap());
		header_allocs += allocs;
		header_time += time;

		let (allocs, time) = measure(|| {
			for push in &pushes {
				group.decode(push).unwrap();
			}
		});
		push_allocs += allocs;
		push_time += time;
	}

	let pushes = RUNS * records as u32;
	(
		(header_allocs / RUNS as usize, header_time / RUNS),
		(push_allocs / pushes as usize, push_time / pushes),
	)
}

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	println!("records header_allocs header_us push_allocs push_ns");
	for records in [1, 16, 256, 4096] {
		let ((header_allocs, header_time), (push_allocs, push_time)) = run(records);
		let header_us = header_time.as_secs_f64() * 1e6;
		let push_ns = push_time.as_nanos();
		println!("{records:>7} {header_allocs:>13} {header_us:>9.1} {push_allocs:>11} {push_ns:>7}");
	}
}

//! Cost of reading one tick off a group track, swept over broadcasts per tier, tiers, and the share
//! of broadcasts that changed since the last tick.
//!
//! Each tick is one merge-patch delta, applied and materialized the way [`moq_stats::Consumer`]
//! does per yield. The encoded frames are built outside the measured section, and so is the
//! track's own DEFLATE, which moq-net inflates before the reader sees a frame.
//!
//! Run `cargo bench -p moq-stats --bench decode` and compare the table across revisions.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use moq_json::snapshot::{self, Decoder, Encoder};
use moq_stats::{Group, GroupFrame, Traffic};

/// A group frame as the producer writes it.
#[derive(serde::Serialize)]
struct Body {
	epoch: String,
	tiers: BTreeMap<String, Group>,
}

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

/// Ticks measured per configuration, after one warm-up delta.
const TICKS: u64 = 16;

/// Broadcast `index` at `tick`: every tenth broadcast is live when `changed_percent` is 10, all of
/// them when it is 100. Idle broadcasts keep their counters, so they drop out of the delta.
fn traffic(index: usize, tick: u64, changed_percent: usize) -> Traffic {
	let live = index.is_multiple_of(100 / changed_percent);
	let tick = if live { tick } else { 0 };
	let mut traffic = Traffic::default();
	traffic.announces_started = 1;
	traffic.broadcasts_started = 3;
	traffic.broadcasts_ended = 1;
	traffic.subscriptions_started = 6;
	traffic.subscriptions_ended = 2;
	traffic.bytes = 180_000 * tick + index as u64;
	traffic.frames = 30 * tick;
	traffic.groups = tick / 2;
	traffic
}

/// The frames a group's producer would publish over the run, snapshot first.
fn frames(broadcasts: usize, tiers: usize, changed_percent: usize) -> Vec<(Vec<u8>, bool)> {
	let mut encoder = Encoder::<Body>::new(snapshot::Config::default().with_deltas(true));
	let epoch = moq_net::Epoch::mint().to_string();
	(0..TICKS + 2)
		.map(|tick| {
			let tiers = (0..tiers)
				.map(|tier| {
					let mut group = Group::default();
					for index in 0..broadcasts {
						let path = format!("acme/room-{index:06}/camera");
						let usage = group.paths.entry(path).or_default();
						usage.publisher = traffic(index, tick, changed_percent);
					}
					(format!("tier-{tier}"), group)
				})
				.collect();
			let frame = Body {
				epoch: epoch.clone(),
				tiers,
			};
			let encoded = encoder.update(&frame).unwrap().expect("every tick changes something");
			let out = (encoded.payload.to_vec(), encoded.keyframe);
			encoded.commit();
			out
		})
		.collect()
}

/// Mean allocations and time per tick.
fn measure(broadcasts: usize, tiers: usize, changed_percent: usize) -> (usize, Duration) {
	let mut frames = frames(broadcasts, tiers, changed_percent).into_iter();
	let mut decoder = Decoder::<GroupFrame>::new(snapshot::consumer::Config::default());
	let (snapshot, keyframe) = frames.next().unwrap();
	assert!(keyframe);
	decoder.snapshot(&snapshot).unwrap();
	let (warm, _) = frames.next().unwrap();
	decoder.delta(&warm).unwrap();
	decoder.decode().unwrap();

	let mut allocations = 0;
	let mut elapsed = Duration::ZERO;
	for _ in 0..TICKS {
		let (payload, keyframe) = frames.next().unwrap();
		ALLOCATIONS.store(0, Ordering::Relaxed);
		COUNTING.store(true, Ordering::Relaxed);
		let start = Instant::now();
		// A large, busy group reaches its byte cap and opens a fresh one.
		match keyframe {
			true => decoder.snapshot(&payload).unwrap(),
			false => decoder.delta(&payload).unwrap(),
		}
		let frame = decoder.decode().unwrap();
		elapsed += start.elapsed();
		COUNTING.store(false, Ordering::Relaxed);
		allocations += ALLOCATIONS.load(Ordering::Relaxed);
		drop(black_box(frame));
	}
	(allocations / TICKS as usize, elapsed / TICKS as u32)
}

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	println!("broadcasts tiers changed% allocs/tick us/tick");
	for changed in [10, 100] {
		for tiers in [1, 4] {
			for broadcasts in [1, 16, 256, 4096] {
				let (allocations, elapsed) = measure(broadcasts, tiers, changed);
				let micros = elapsed.as_secs_f64() * 1e6;
				println!("{broadcasts:>10} {tiers:>5} {changed:>8} {allocations:>11} {micros:>8.1}");
			}
		}
	}
}

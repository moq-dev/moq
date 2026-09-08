//! What a cached group really costs, measured against what the pool charges for it.
//!
//! `cache::ENTRY_OVERHEAD` is derived from `size_of`, which keeps it following the
//! structs but proves nothing about the heap the structs actually take. This weighs
//! the process: cache a run of groups shaped like chat traffic (one small frame each)
//! and compare the bytes the allocator handed out against the bytes the pool believes
//! it is holding. A relay is killed when those two diverge.
//!
//! A counting allocator needs the whole binary to itself, hence a test of its own.

// Every size here is pointer-width dependent, and the derivation is checked on the
// 64-bit targets a relay runs on. This also keeps the global allocator off wasm.
#![cfg(target_pointer_width = "64")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use moq_net::{Timestamp, broadcast, cache, origin};

thread_local! {
	/// Bytes *this* thread has allocated and not yet freed. Per-thread and not a
	/// global counter because the harness may run tests concurrently in one process,
	/// which would fold another test's allocations into the measurement.
	///
	/// `const` initialised, so reading it from inside the allocator cannot allocate
	/// and recurse.
	static LIVE: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// Accounting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// `try_with`, because TLS is unavailable while a thread is being torn down.
		let _ = LIVE.try_with(|live| live.set(live.get() + layout.size()));
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(layout.size())));
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Frames per group and payload bytes per frame: the chat shape, where bookkeeping
/// rather than payload is the whole cost.
const PAYLOAD: usize = 150;

/// Enough groups that the track's containers reach their amortised per-entry cost
/// and the one-off setup washes out.
const GROUPS: usize = 4096;

/// Cache `GROUPS` single-frame groups and compare the heap they really occupy against
/// what the pool charged, both in bytes per group.
fn measure() -> (usize, u64) {
	// Unbounded: nothing may be evicted underneath the measurement.
	let pool = cache::Pool::unbounded();
	let mut info = broadcast::Info::new();
	info.origin = origin::Info::default().with_pool(pool.clone());

	let mut broadcast = info.produce();
	let mut track = broadcast.create_track("chat", None).unwrap();

	// Warm up outside the measurement so the track's own one-time allocations and
	// any lazy statics aren't billed to the groups.
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, vec![0u8; PAYLOAD]).unwrap();
	group.finish().unwrap();

	let before_heap = LIVE.with(Cell::get);
	let before_charge = pool.used();

	for _ in 0..GROUPS {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, vec![0u8; PAYLOAD]).unwrap();
		group.finish().unwrap();
	}

	let heap = (LIVE.with(Cell::get) - before_heap) / GROUPS;
	let charged = (pool.used() - before_charge) / GROUPS as u64;

	// Keep the cache alive until both numbers are read.
	drop(track);
	drop(broadcast);

	(heap, charged)
}

/// The charge has to be the same order as the memory, or `MOQ_CACHE_CAPACITY` is a
/// number about something else. Undercharging is the fatal direction: the pool evicts
/// nothing until it thinks it is full, and the process dies first.
#[test]
fn charge_tracks_real_memory() {
	let (heap, charged) = measure();

	assert!(
		charged as usize >= heap / 2,
		"charged {charged} B/group against {heap} B of real heap: the pool is undercounting"
	);
	assert!(
		charged as usize <= heap * 2,
		"charged {charged} B/group against {heap} B of real heap: the pool is overcounting"
	);
}

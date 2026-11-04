//! Allocation requests for the same workload, kept out of instruction-counting builds.
use std::{
	alloc::{GlobalAlloc, Layout, System},
	sync::atomic::{AtomicUsize, Ordering},
};

struct Allocator;

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

fn record(bytes: usize) {
	ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
	BYTES.fetch_add(bytes, Ordering::Relaxed);
}

// SAFETY: forwards every allocation and deallocation to System unchanged.
unsafe impl GlobalAlloc for Allocator {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		record(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		record(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		record(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

pub(super) fn report() {
	let allocations = ALLOCATIONS.load(Ordering::Relaxed);
	let bytes = BYTES.load(Ordering::Relaxed);
	println!(
		"consumer_bytes={} allocations={allocations} allocated_bytes={bytes}",
		size_of::<moq_net::group::Consumer>()
	);
}

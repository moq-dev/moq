//! Counts this thread's allocations, so a test measures only its own work
//! while other tests run in parallel.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
	static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
		unsafe { System.realloc(ptr, layout, new_size) }
	}
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations this thread has made so far.
pub fn allocs() -> usize {
	ALLOCS.with(Cell::get)
}

//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module. Every
//! test here counts allocations, so the counting allocator is shared.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod decode_allocations;
mod window_allocations;

struct Counter;

thread_local! {
	// Per thread, so tests running alongside don't leak into the count.
	static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

#[global_allocator]
static ALLOCATOR: Counter = Counter;

fn bump() {
	// `try_with` because the allocator can run while the thread-local is being torn down.
	let _ = ALLOCATIONS.try_with(|count| count.set(count.get().map(|n| n + 1)));
}

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		bump();
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		bump();
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		bump();
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

fn count<R>(f: impl FnOnce() -> R) -> (R, usize) {
	ALLOCATIONS.with(|count| count.set(Some(0)));
	let out = f();
	let allocations = ALLOCATIONS.with(|count| count.take()).unwrap();
	(out, allocations)
}

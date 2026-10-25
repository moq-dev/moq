//! The heap moq holds, counted for host leak tests.
//!
//! Host process memory also moves with the host's own heap and allocator retention, so a
//! leak test reading it fails without any leak. Counting moq's own allocations isolates
//! the buffers the bindings must release, including ones with no handle, such as a
//! returned `String` or an error reason.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

/// Forwards to the system allocator, counting what it hands out.
struct Counting;

// Relaxed suffices: a block's dealloc happens after its alloc, so the count never dips
// below zero, and readers only compare totals between calls.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc(layout) };
		if !ptr.is_null() {
			ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
		}
		ptr
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc_zeroed(layout) };
		if !ptr.is_null() {
			ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
		}
		ptr
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) };
		ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		let new = unsafe { System.realloc(ptr, layout, new_size) };
		// A failed realloc leaves the old block allocated.
		if !new.is_null() {
			ALLOCATED.fetch_add(new_size, Ordering::Relaxed);
			ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
		}
		new
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The bytes moq's heap holds right now.
///
/// Test builds only (the `testing` feature). Rust code in the library allocates through
/// this counter; the host's heap and bundled C libraries do not.
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_allocated() -> u64 {
	ALLOCATED.load(Ordering::Relaxed) as u64
}

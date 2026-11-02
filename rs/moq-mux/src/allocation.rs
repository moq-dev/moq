//! Per-thread heap accounting for the crate's tests and the fuzz targets' allocation bound.
//!
//! [`Counting`] forwards every call to the system allocator unchanged and counts only on a thread
//! inside [`measure`], so concurrent tests are unaffected. A binary opts in by registering it as its
//! `#[global_allocator]`: this crate's unit tests do below, and each `fuzz/` target does too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The heap traffic of one [`measure`]d call.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
	/// Bytes allocated, counting a realloc as a fresh allocation of its new size.
	pub allocated: usize,
	/// Bytes freed, counting a realloc as freeing its old size.
	pub freed: usize,
	/// Calls to allocate or reallocate.
	pub allocations: usize,
}

thread_local! {
	/// `const` so the allocator can read it without allocating.
	static USAGE: Cell<Option<Usage>> = const { Cell::new(None) };
}

fn record(update: impl FnOnce(&mut Usage)) {
	// `try_with` because TLS is unavailable while a thread is being torn down.
	let _ = USAGE.try_with(|usage| {
		if let Some(mut current) = usage.get() {
			update(&mut current);
			usage.set(Some(current));
		}
	});
}

/// Counts on the measuring thread; every call forwards to the system allocator unchanged.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		record(|usage| {
			usage.allocated += layout.size();
			usage.allocations += 1;
		});
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		record(|usage| usage.freed += layout.size());
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		record(|usage| {
			usage.allocated += new_size;
			usage.freed += layout.size();
			usage.allocations += 1;
		});
		unsafe { System.realloc(ptr, layout, new_size) }
	}
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f`, returning what it did to this thread's heap. Not reentrant.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, Usage) {
	USAGE.with(|usage| usage.set(Some(Usage::default())));
	let result = f();
	let usage = USAGE.with(|usage| usage.replace(None).unwrap());
	(result, usage)
}

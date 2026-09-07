//! Thread-local allocation counting, enabled only during untimed measurement.
use std::{
	alloc::{GlobalAlloc, Layout, System},
	cell::Cell,
};
thread_local! { pub(crate) static ALLOCS: Cell<Option<(usize, usize)>> = const { Cell::new(None) }; }
struct Counting;
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn record(size: usize) {
	let _ = ALLOCS.try_with(|c| {
		if let Some((n, b)) = c.get() {
			c.set(Some((n + 1, b + size)));
		}
	});
}
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, l: Layout) -> *mut u8 {
		record(l.size());
		unsafe { System.alloc(l) }
	}
	unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
		record(l.size());
		unsafe { System.alloc_zeroed(l) }
	}
	unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
		record(n);
		unsafe { System.realloc(p, l, n) }
	}
	unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
		unsafe { System.dealloc(p, l) }
	}
}

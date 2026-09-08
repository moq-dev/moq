//! Allocation requests and live requested bytes for a single-threaded measurement pass.
use std::{
	alloc::{GlobalAlloc, Layout, System},
	cell::Cell,
};

#[derive(Clone, Copy, Default)]
pub(super) struct Counts {
	pub allocations: usize,
	pub requested: usize,
	pub live: isize,
	pub peak: isize,
}

thread_local! {
	pub(super) static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
}

struct Counting;
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn record(allocated: usize, freed: usize, allocation: bool) {
	let _ = COUNTS.try_with(|cell| {
		if let Some(mut counts) = cell.get() {
			counts.allocations += usize::from(allocation);
			counts.requested += allocated;
			counts.live += allocated as isize - freed as isize;
			counts.peak = counts.peak.max(counts.live);
			cell.set(Some(counts));
		}
	});
}

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc(layout) };
		if !ptr.is_null() {
			record(layout.size(), 0, true);
		}
		ptr
	}
	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc_zeroed(layout) };
		if !ptr.is_null() {
			record(layout.size(), 0, true);
		}
		ptr
	}
	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
		let ptr = unsafe { System.realloc(ptr, layout, size) };
		if !ptr.is_null() {
			record(size, layout.size(), true);
		}
		ptr
	}
	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		record(0, layout.size(), false);
		unsafe { System.dealloc(ptr, layout) };
	}
}

pub(super) fn snapshot() -> Counts {
	COUNTS.with(|cell| cell.get().unwrap_or_default())
}

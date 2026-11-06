//! Thread-local allocation measurements for buffer ownership tests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct TrackingAllocator;

#[derive(Clone, Copy, Default)]
pub(crate) struct Stats {
	pub operations: usize,
	pub bytes: usize,
}

thread_local! {
	static ACTIVITY: Cell<Option<Stats>> = const { Cell::new(None) };
}

unsafe impl GlobalAlloc for TrackingAllocator {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		ACTIVITY.with(|activity| {
			if let Some(mut stats) = activity.get() {
				stats.operations += 1;
				stats.bytes += layout.size();
				activity.set(Some(stats));
			}
		});
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		ACTIVITY.with(|activity| {
			if let Some(mut stats) = activity.get() {
				stats.operations += 1;
				activity.set(Some(stats));
			}
		});
		unsafe { System.dealloc(ptr, layout) };
	}
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

pub(crate) fn measure<T>(f: impl FnOnce() -> T) -> (T, Stats) {
	struct Reset;
	impl Drop for Reset {
		fn drop(&mut self) {
			ACTIVITY.with(|activity| activity.set(None));
		}
	}
	let _reset = Reset;
	ACTIVITY.with(|activity| assert!(activity.replace(Some(Stats::default())).is_none()));
	let result = f();
	(result, ACTIVITY.with(|activity| activity.replace(None).unwrap()))
}

#[cfg(feature = "capture")]
pub(crate) fn activity(f: impl FnOnce()) -> usize {
	measure(f).1.operations
}

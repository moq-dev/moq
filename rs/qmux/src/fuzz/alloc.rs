//! Heap accounting for the session under test, apart from the peer driving it in the same thread.
//!
//! A binary opts in by registering [`Counting`] as its `#[global_allocator]`: this crate's
//! tests do below, and the `fuzz/` target does too. Each allocation made while [`charge`] runs
//! is tagged in a header in front of it, so freeing it uncharges it whoever frees it, and the
//! peer's allocations never count however the two sides hand buffers to each other.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The bytes in front of each allocation that hold its tag, at least its alignment so the
/// allocation stays aligned.
const HEADER: usize = 16;

thread_local! {
	/// Whether allocations are charged right now, and the books while measuring.
	static BOOKS: Cell<Books> = const { Cell::new(Books { charging: false, measuring: false, live: 0, peak: 0 }) };
}

#[derive(Clone, Copy)]
struct Books {
	charging: bool,
	measuring: bool,
	live: usize,
	peak: usize,
}

/// A global allocator that counts what [`charge`] allocates, for [`measure`].
pub struct Counting;

impl Counting {
	fn header(layout: Layout) -> Option<(Layout, usize)> {
		let offset = HEADER.max(layout.align());
		let size = layout.size().checked_add(offset)?;
		Some((Layout::from_size_align(size, layout.align().max(HEADER)).ok()?, offset))
	}
}

// Every allocation goes to System with a header in front. Const TLS keeps the accounting from
// allocating recursively.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let Some((outer, offset)) = Self::header(layout) else {
			return std::ptr::null_mut();
		};
		let base = unsafe { System.alloc(outer) };
		if base.is_null() {
			return base;
		}
		let charged = BOOKS
			.try_with(|books| {
				let mut b = books.get();
				if !(b.charging && b.measuring) {
					return false;
				}
				b.live += layout.size();
				b.peak = b.peak.max(b.live);
				books.set(b);
				true
			})
			.unwrap_or(false);
		let ptr = unsafe { base.add(offset) };
		// The tag sits just in front of the allocation, so dealloc finds it from `ptr` alone.
		unsafe { ptr.sub(1).write(u8::from(charged)) };
		ptr
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		let (outer, offset) = Self::header(layout).expect("alloc accepted this layout");
		if unsafe { ptr.sub(1).read() } == 1 {
			let _ = BOOKS.try_with(|books| {
				let mut b = books.get();
				if b.measuring {
					b.live = b.live.saturating_sub(layout.size());
					books.set(b);
				}
			});
		}
		unsafe { System.dealloc(ptr.sub(offset), outer) }
	}
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `run` with nothing charged yet, returning what [`charge`] allocated inside it: the most it
/// held at once, and what it still holds once `run` returns. Not reentrant.
///
/// Both stay zero unless the binary registers [`Counting`].
pub(super) fn measure<T>(run: impl FnOnce() -> T) -> (T, Usage) {
	BOOKS.with(|books| {
		books.set(Books {
			charging: false,
			measuring: true,
			live: 0,
			peak: 0,
		})
	});
	let result = run();
	let books = BOOKS.with(|books| {
		let b = books.get();
		books.set(Books {
			measuring: false,
			..b
		});
		b
	});
	(
		result,
		Usage {
			peak: books.peak,
			live: books.live,
		},
	)
}

/// What [`measure`] counted.
#[derive(Debug, Clone, Copy)]
pub(super) struct Usage {
	pub peak: usize,
	pub live: usize,
}

/// Run `run` with its allocations charged to the session under test.
pub(super) fn charge<T>(run: impl FnOnce() -> T) -> T {
	let was = BOOKS.with(|books| {
		let b = books.get();
		books.set(Books { charging: true, ..b });
		b.charging
	});
	let result = run();
	BOOKS.with(|books| {
		let b = books.get();
		books.set(Books { charging: was, ..b });
	});
	result
}

//! Allocation counting for the waiter-list park/wake cycle.
//!
//! `WaiterList` sizes its inline slots against how many waiters a list can hold
//! without allocating once per wake, so that count is the whole justification for
//! the constant and nothing else observes it. A counting allocator needs the whole
//! binary to itself, hence a test of its own rather than a unit test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::task::{Context, Wake, Waker};

use kio::{Park, Waiter, WaiterList};

/// Inline slots in a `WaiterList`, which is private, so this mirrors it. A lower
/// real value fails the test rather than passing it quietly.
const INLINE_WAITERS: usize = 4;

thread_local! {
	/// Allocations made by *this* thread. Per-thread and not a global counter
	/// because the test harness runs these tests concurrently, and a shared count
	/// would fold one test's allocations into the other's measurement.
	///
	/// `const` initialised, so reading it from inside the allocator cannot allocate
	/// and recurse.
	static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// `try_with`, because TLS is unavailable while a thread is being torn down.
		let _ = ALLOCS.try_with(|allocs| allocs.set(allocs.get() + 1));
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations made by `cycles` rounds of "every waiter registers, then the list is
/// drained the production way": snapshot under the lock, wake outside it.
fn cycle_allocs(waiters: &[Waiter], cycles: usize) -> usize {
	let mut list = WaiterList::new();

	// Reach the steady state first: identities allocated, and the
	// list grown to whatever it settles at.
	for _ in 0..4 {
		for waiter in waiters {
			waiter.register(&mut list);
		}
		list.take().wake();
	}

	let before = ALLOCS.with(Cell::get);
	for _ in 0..cycles {
		for waiter in waiters {
			waiter.register(&mut list);
		}
		list.take().wake();
	}
	ALLOCS.with(Cell::get) - before
}

fn waiters(n: usize) -> Vec<Waiter> {
	(0..n).map(|_| Waiter::new(Waker::noop().clone())).collect()
}

/// Up to the inline capacity, a list parks and wakes forever without touching the
/// heap. This is what the inline slots buy: `take()` hands its spilled buffer to the
/// snapshot, which frees it, so a spilled list re-allocates on every single wake
/// rather than keeping its capacity.
#[test]
fn a_small_list_cycles_without_allocating() {
	for n in 1..=INLINE_WAITERS {
		assert_eq!(cycle_allocs(&waiters(n), 100), 0, "{n} waiters allocated per wake");
	}
}

/// Past the inline capacity the list spills, and the spill costs one allocation per
/// wake. Asserted so the boundary is a measured fact rather than a claim in a
/// comment, and so shrinking the inline slots cannot move it silently.
#[test]
fn a_spilled_list_allocates_once_per_wake() {
	assert_eq!(cycle_allocs(&waiters(INLINE_WAITERS + 1), 100), 100);
}

/// A task parked on several lists that only one of them wakes, as a relay's group
/// stream parks on its frames, its priority, and its subscription. Its park re-polls
/// with the same waiter, so a poll that re-registers everywhere allocates nothing.
#[test]
fn a_partial_wake_reuses_the_parked_waiter() {
	// A waker of its own, since `Park` reuses a waiter only for a waker that
	// `will_wake` matches, and `Waker::noop()` need not match itself.
	struct Task;
	#[expect(clippy::manual_noop_waker, reason = "the waker must match itself under will_wake")]
	impl Wake for Task {
		fn wake(self: Arc<Self>) {}
	}

	let waker = Waker::from(Arc::new(Task));
	let cx = Context::from_waker(&waker);
	let mut park = Park::default();
	let mut woken = WaiterList::new();
	let mut quiet = [WaiterList::new(), WaiterList::new()];

	let mut poll = || {
		let waiter = park.hold(&cx);
		woken.register(waiter);
		for list in &mut quiet {
			list.register(waiter);
		}
		woken.wake();
	};

	poll();
	let before = ALLOCS.with(Cell::get);
	for _ in 0..100 {
		poll();
	}
	assert_eq!(
		ALLOCS.with(Cell::get) - before,
		0,
		"a partial wake re-allocated the waiter"
	);
}

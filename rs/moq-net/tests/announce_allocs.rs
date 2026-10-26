#![cfg(feature = "async")]

//! Allocation counting for the announce hot paths: decoding the paths a peer
//! announces, and revisiting a cursor whose winning route is unchanged.
//!
//! Every route change at a prefix revisits each cursor presenting it, and on a
//! dense mesh most of those visits find the cursor's winner unchanged: a worse
//! path arriving or leaving. Such a visit must not allocate, or the cost of
//! route churn multiplies by the cursors watching. A counting allocator needs
//! the whole binary to itself, hence a test of its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use moq_net::{Hop, Hops, Pattern, Patterns, fuzz::decode_path, origin};

thread_local! {
	/// Fresh allocations made by *this* thread. Per-thread because the harness may
	/// run tests concurrently in one process.
	///
	/// `const` initialised, so reading it from inside the allocator cannot allocate
	/// and recurse.
	static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// Counting only. Every call forwards to the system allocator unchanged. A
// `realloc` grows a buffer that already exists, which amortizes, so only fresh
// allocations count.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// `try_with`, because TLS is unavailable while a thread is being torn down.
		let _ = ALLOCS.try_with(|allocs| allocs.set(allocs.get() + 1));
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		unsafe { System.realloc(ptr, layout, new_size) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A route as `peer` would have announced it: one hop, at `cost`.
fn peer_route(peer: u64, cost: u64) -> origin::Route {
	let mut hops = Hops::new();
	hops.push(Hop::new(peer).unwrap()).unwrap();
	origin::Route::default().with_hops(hops).with_cost(cost)
}

/// Allocations made by a worse route arriving at a watched prefix and leaving
/// again, with `cursors` cursors rooted at `root` watching it.
fn churn_allocs(root: &str, cursors: usize) -> usize {
	let (producer, _driver) = origin::Producer::new(origin::Config::default());
	let _winner = producer.publish("room/a", peer_route(1, 1)).unwrap();
	let all = Patterns::from(Pattern::all());
	let mut cursors: Vec<_> = (0..cursors)
		.map(|_| producer.consume().scope(root, &all).unwrap().announced())
		.collect();
	for cursor in &mut cursors {
		while cursor.try_next().is_some() {}
	}

	let cycle = || drop(producer.publish("room/a", peer_route(2, 5)).unwrap());
	// Reach the steady state first, with every table grown to what it settles at.
	for _ in 0..4 {
		cycle();
	}
	let before = ALLOCS.with(Cell::get);
	cycle();
	let allocs = ALLOCS.with(Cell::get) - before;

	for cursor in &mut cursors {
		assert!(cursor.try_next().is_none(), "the winner is unchanged");
	}
	allocs
}

/// A route that never wins costs the same however many cursors watch it, whether
/// they present its prefix beneath their root or as the root itself, which ranks
/// every route covering it.
#[test]
fn an_unchanged_winner_allocates_nothing_per_cursor() {
	for root in ["", "room", "room/a"] {
		let one = churn_allocs(root, 1);
		for cursors in [20, 100] {
			assert_eq!(
				churn_allocs(root, cursors),
				one,
				"{cursors} cursors rooted at {root:?} allocated more than one"
			);
		}
	}
}

/// Fresh allocations made while running `f`, including what it returns.
fn allocs<R>(f: impl FnOnce() -> R) -> usize {
	let before = ALLOCS.with(Cell::get);
	let out = f();
	let allocs = ALLOCS.with(Cell::get) - before;
	drop(out);
	allocs
}

/// A path decodes straight into the one shared buffer it keeps: moq-lite reads it in
/// place, and a moq-transport tuple joins its fields into one escaped string first.
#[test]
fn a_decoded_path_allocates_only_its_buffer() {
	let long = "1234/private/channel_7/stream-health-1700000000";
	for path in ["room/alice", long] {
		let mut wire = vec![path.len() as u8];
		wire.extend_from_slice(path.as_bytes());
		assert_eq!(decode_path(&wire, false).as_str(), path);
		let lite = allocs(|| decode_path(&wire, false));
		assert!(lite <= 1, "{lite} allocations for a moq-lite {path:?}");

		let parts: Vec<&str> = path.split('/').collect();
		let mut wire = vec![parts.len() as u8];
		for part in &parts {
			wire.push(part.len() as u8);
			wire.extend_from_slice(part.as_bytes());
		}
		assert_eq!(decode_path(&wire, true).as_str(), path);
		let ietf = allocs(|| decode_path(&wire, true));
		assert!(ietf <= 2, "{ietf} allocations for a moq-transport {path:?}");
	}
}

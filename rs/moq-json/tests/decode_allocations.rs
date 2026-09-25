//! Materializing a snapshot allocates only what the returned value owns.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use moq_json::snapshot::{Decoder, consumer};

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

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Entry {
	bytes: u64,
	frames: u64,
	groups: u64,
	nested: Nested,
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Nested {
	bytes: u64,
	frames: u64,
}

/// Each entry costs its key `String` plus a share of the map's nodes, and nothing per field.
/// Tracking the error path on every decode cost an allocation per key walked, ~7x this bound.
#[test]
fn decode_allocates_only_the_output() {
	const ENTRIES: usize = 1024;

	let mut doc = serde_json::Map::new();
	for index in 0..ENTRIES {
		doc.insert(
			format!("broadcast/{index:05}"),
			serde_json::json!({ "bytes": index, "frames": 2, "groups": 3, "nested": { "bytes": 4, "frames": 5 } }),
		);
	}
	let mut decoder = Decoder::<BTreeMap<String, Entry>>::new(consumer::Config::default());
	decoder
		.snapshot(&serde_json::to_vec(&serde_json::Value::Object(doc)).unwrap())
		.unwrap();

	let (value, allocations) = count(|| decoder.decode().unwrap().unwrap());
	assert_eq!(value.len(), ENTRIES);
	assert_eq!(value["broadcast/00007"].bytes, 7);
	assert!(
		allocations < ENTRIES * 2,
		"{allocations} allocations for {ENTRIES} entries"
	);
}

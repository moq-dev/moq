//! Decoding a window frame allocates only what the decoded records own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use moq_json::window::{ConsumerConfig, Decoder, Event};

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
struct Record {
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

fn record(index: usize) -> serde_json::Value {
	serde_json::json!({ "bytes": index, "frames": 2, "groups": 3, "nested": { "bytes": 4, "frames": 5 } })
}

/// The records own nothing on the heap, so a header costs only its `Vec`'s growth and a push costs
/// only the event queue's. Tracking the error path on every decode cost an allocation per key walked.
#[test]
fn decode_allocates_only_the_output() {
	const RECORDS: usize = 1024;

	let header = serde_json::to_vec(&serde_json::json!({
		"offset": 0,
		"records": (0..RECORDS).map(record).collect::<Vec<_>>(),
	}))
	.unwrap();
	let pushes: Vec<_> = (RECORDS..2 * RECORDS)
		.map(|index| serde_json::to_vec(&serde_json::json!({ "push": record(index) })).unwrap())
		.collect();

	let mut decoder = Decoder::<Record>::new(ConsumerConfig::default());
	let mut group = decoder.group();

	let ((), allocations) = count(|| group.decode(&header).unwrap());
	assert!(
		allocations < 64,
		"{allocations} allocations for a {RECORDS}-record header"
	);

	let ((), allocations) = count(|| {
		for push in &pushes {
			group.decode(push).unwrap();
		}
	});
	assert!(allocations < 64, "{allocations} allocations for {RECORDS} pushes");

	let mut events = std::iter::from_fn(|| group.next_event());
	assert!(matches!(events.next(), Some(Event::Push { index: 0, value }) if value.bytes == 0));
	assert!(
		matches!(events.last(), Some(Event::Push { index, value }) if index == 2 * RECORDS as u64 - 1 && value.bytes == index)
	);
}

/// A malformed record still names where it went wrong.
#[test]
fn decode_error_names_the_path() {
	let mut decoder = Decoder::<Record>::new(ConsumerConfig::default());
	let mut group = decoder.group();

	let header = serde_json::to_vec(&serde_json::json!({
		"offset": 0,
		"records": [record(0), { "bytes": 1, "frames": 2, "groups": 3, "nested": { "bytes": "four", "frames": 5 } }],
	}))
	.unwrap();
	let err = group.decode(&header).unwrap_err().to_string();
	assert!(err.contains("records[1].nested.bytes"), "{err}");

	group
		.decode(&serde_json::to_vec(&serde_json::json!({ "offset": 0, "records": [] })).unwrap())
		.unwrap();
	let err = group
		.decode(br#"{"push":{"bytes":1,"frames":"two","groups":3,"nested":{"bytes":4,"frames":5}}}"#)
		.unwrap_err()
		.to_string();
	assert!(err.contains("push.frames"), "{err}");
}

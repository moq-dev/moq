//! Count allocations in one warmed snapshot update, swept over document size and changed fields.
//!
//! Run `cargo bench -p moq-json --bench allocations` and compare the table across revisions.
//! Payloads and input values are built outside the counted section. A new `Bytes` frame is
//! necessarily owned; the count also includes the diff, patch merge, and optional inflation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use moq_json::snapshot::{self, Decoder, Encoder};
use serde_json::{Map, Value};

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

fn count(f: impl FnOnce()) -> usize {
	ALLOCATIONS.store(0, Ordering::Relaxed);
	COUNTING.store(true, Ordering::Relaxed);
	f();
	COUNTING.store(false, Ordering::Relaxed);
	ALLOCATIONS.load(Ordering::Relaxed)
}

fn docs(fields: usize, changed_percent: usize) -> (Value, Value) {
	let mut old = Map::new();
	let mut new = Map::new();
	let changed = fields * changed_percent / 100;
	for index in 0..fields {
		let key = format!("field_{index:05}");
		old.insert(key.clone(), Value::from(index));
		new.insert(key, Value::from(index + usize::from(index < changed)));
	}
	(Value::Object(old), Value::Object(new))
}

fn measure(fields: usize, changed_percent: usize, compressed: bool) -> (usize, usize) {
	let (old, new) = docs(fields, changed_percent);
	let compression = if compressed {
		moq_json::Compression::Deflate
	} else {
		moq_json::Compression::None
	};
	let mut config = snapshot::Config::default().with_delta_ratio(u32::MAX);
	config.compression = compression;
	let mut encoder = Encoder::<Value>::new(config);
	let mut decoder_config = snapshot::consumer::Config::default();
	decoder_config.compression = compression;
	let mut decoder = Decoder::<Value>::new(decoder_config);
	let snapshot = encoder.update(&old).unwrap().unwrap();
	decoder.snapshot(&snapshot.payload).unwrap();
	snapshot.commit();

	// Warm the reusable key buffers and both compression windows.
	if let Some(frame) = encoder.update(&new).unwrap() {
		decoder.delta(&frame.payload).unwrap();
		frame.commit();
	}
	let mut encoded = 0;
	let mut decoded = 0;
	for index in 0..32 {
		let value = if index % 2 == 0 { &old } else { &new };
		let mut payload = None;
		encoded += count(|| {
			if let Some(frame) = encoder.update(value).unwrap() {
				payload = Some(frame.payload.clone());
				frame.commit();
			}
		});
		if let Some(payload) = payload {
			decoded += count(|| decoder.delta(&payload).unwrap());
		}
	}
	(encoded / 32, decoded / 32)
}

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	println!("fields changed% compression encoder_allocs decoder_allocs");
	for compressed in [false, true] {
		for fields in [16, 128, 1024, 8192] {
			for changed in [0, 1, 10, 100] {
				let (encoder, decoder) = black_box(measure(fields, changed, compressed));
				println!("{fields:>6} {changed:>8} {compressed:>11} {encoder:>14} {decoder:>14}");
			}
		}
	}
}

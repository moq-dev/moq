//! Materializing a snapshot allocates only what the returned value owns.

use std::collections::BTreeMap;

use crate::count;
use moq_json::snapshot::{Decoder, consumer};

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

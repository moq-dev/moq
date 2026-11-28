//! One warmed snapshot update, swept over document size and changed fields, for swole to count
//! its allocations and time.
//!
//! Each call alternates between two documents, so every changed case encodes or decodes a delta.
//! A new `Bytes` frame is necessarily owned; the encoder's cost also includes the diff and patch,
//! the decoder's the merge.
//!
//! Run with `cargo bench -p moq-json --bench allocations`.

use std::fmt;

use bytes::Bytes;
use moq_json::snapshot::{self, Decoder, Encoder};
use serde_json::{Map, Value};
use swole::bench;

/// A document of `fields` fields with `changed_percent` of them changing, named `<fields>/<changed>%`.
#[derive(Clone, Copy)]
struct Case {
	fields: usize,
	changed_percent: usize,
}

impl Case {
	fn changed(&self) -> usize {
		self.fields * self.changed_percent / 100
	}

	fn docs(&self) -> [Value; 2] {
		let mut old = Map::new();
		let mut new = Map::new();
		for index in 0..self.fields {
			let key = format!("field_{index:05}");
			old.insert(key.clone(), Value::from(index));
			new.insert(key, Value::from(index + usize::from(index < self.changed())));
		}
		[Value::Object(old), Value::Object(new)]
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}%", self.fields, self.changed_percent)
	}
}

fn cases() -> Vec<Case> {
	[16, 128, 1024, 8192]
		.into_iter()
		.flat_map(|fields| {
			[0, 1, 10, 100].map(|changed_percent| Case {
				fields,
				changed_percent,
			})
		})
		.collect()
}

/// A seeded encoder and the two documents it alternates between.
struct Encoding {
	encoder: Encoder<Value>,
	docs: [Value; 2],
	next: usize,
}

fn encoding(case: Case) -> Encoding {
	let docs = case.docs();
	let mut encoder = Encoder::<Value>::new(snapshot::Config::default().with_deltas(true));
	encoder.update(&docs[0]).unwrap().unwrap().commit();
	// Warm the reusable key buffers, ending back on the first document.
	for value in [&docs[1], &docs[0]] {
		if let Some(frame) = encoder.update(value).unwrap() {
			frame.commit();
		}
	}
	Encoding { encoder, docs, next: 1 }
}

/// Restate the baseline so the group's frame cap never turns a delta into a snapshot.
fn restate(state: &mut Encoding) {
	state.encoder.snapshot().unwrap().expect("a seeded baseline").commit();
}

#[bench(args = cases(), setup = encoding, before = restate)]
fn encode(state: &mut Encoding) -> Option<Bytes> {
	let value = &state.docs[state.next];
	state.next ^= 1;
	let frame = state.encoder.update(value).unwrap()?;
	let payload = frame.payload.clone();
	frame.commit();
	Some(payload)
}

/// A decoder holding the first document and the deltas that flip it to the second and back.
struct Decoding {
	decoder: Decoder<Value>,
	deltas: [Bytes; 2],
	next: usize,
}

fn decoding(case: Case) -> Decoding {
	let [old, new] = case.docs();
	let mut encoder = Encoder::<Value>::new(snapshot::Config::default().with_deltas(true));
	let mut decoder = Decoder::<Value>::new(snapshot::consumer::Config::default());
	let snapshot = encoder.update(&old).unwrap().unwrap();
	decoder.snapshot(&snapshot.payload).unwrap();
	snapshot.commit();

	let mut delta = |value: &Value| {
		let frame = encoder.update(value).unwrap().expect("a changed document");
		assert!(!frame.keyframe);
		let payload = frame.payload.clone();
		frame.commit();
		payload
	};
	let deltas = [delta(&new), delta(&old)];
	// Warm the reusable key buffers.
	decoder.delta(&deltas[0]).unwrap();
	decoder.delta(&deltas[1]).unwrap();
	Decoding {
		decoder,
		deltas,
		next: 0,
	}
}

/// The cases with a delta to decode: an unchanged document encodes none.
fn changed_cases() -> Vec<Case> {
	cases().into_iter().filter(|case| case.changed() > 0).collect()
}

#[bench(args = changed_cases(), setup = decoding)]
fn decode(state: &mut Decoding) {
	state.decoder.delta(&state.deltas[state.next]).unwrap();
	state.next ^= 1;
}

fn main() {
	swole::main();
}

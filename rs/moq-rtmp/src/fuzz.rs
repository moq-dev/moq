//! The fuzz target body for the AMF0 receive views, plus the seeds it starts from.
//!
//! The target reads an RTMP AMF0 message through the borrowed views a session uses, walking every
//! value a peer supplied, and checks that it allocates nothing: the views keep the encoded bytes
//! instead of a node per value, so heap traffic stays independent of what the peer sends. The body
//! lives here rather than in `fuzz/fuzz_targets/` because the `rml` module is private, and so the
//! tests below replay it on stable.
//!
//! Compiled only under `cfg(test)` or the `fuzz` feature, so none of this is part of the published
//! API. See `fuzz/README.md` for the workflow.

use crate::rml::amf0::{Amf0Value, Value, allocation::measure, serialize};
use crate::rml::messages::MessagePayload;

pub use crate::rml::amf0::allocation::Counting;

/// One fuzz target body: it returns whether the input decoded.
pub type Target = fn(&[u8]) -> bool;

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[("amf0", amf0)];

/// The AMF0 message types, selected by the input's first byte: AMF3 data, AMF3 command (AMF0
/// behind a zero byte), AMF0 data, and AMF0 command.
const TYPES: &[u8] = &[15, 17, 18, 20];

/// An AMF0 message read through the session's views, every value walked, with no allocation.
///
/// The leading byte picks the message type; the rest is its body. Measures only when the binary
/// registers [`Counting`] as its global allocator; this crate's tests and the `fuzz/` target do.
pub fn amf0(data: &[u8]) -> bool {
	let Some((&selector, body)) = data.split_first() else {
		return false;
	};
	let payload = MessagePayload {
		type_id: TYPES[selector as usize % TYPES.len()],
		data: bytes::Bytes::copy_from_slice(body),
		..MessagePayload::new()
	};

	let (decoded, peak) = measure(|| {
		use crate::rml::messages::Amf0Message;

		match payload.amf0() {
			Ok(Some(Amf0Message::Command {
				name,
				object,
				arguments,
				..
			})) => {
				std::hint::black_box(name);
				walk(object);
				arguments.for_each(walk);
				true
			}
			Ok(Some(Amf0Message::Data(values))) => {
				values.for_each(walk);
				true
			}
			Ok(None) | Err(_) => false,
		}
	});
	assert_eq!(peak, 0, "reading {} AMF0 bytes allocated", body.len());
	decoded
}

/// Visit every value nested in `value`, and look a property up the way a session does.
fn walk(value: Value<'_>) {
	match value {
		Value::Object(properties) => {
			std::hint::black_box(properties.get("width"));
			for (name, value) in properties {
				std::hint::black_box(name);
				walk(value);
			}
		}
		Value::StrictArray(values) => values.for_each(walk),
		value => {
			std::hint::black_box(value);
		}
	}
}

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input, selector byte included.
	pub data: Vec<u8>,
}

/// The starting corpus: the commands and metadata a publisher sends, one per message type.
pub fn seeds() -> Vec<Seed> {
	let object = |properties: &[(&str, Amf0Value)]| {
		Amf0Value::Object(
			properties
				.iter()
				.map(|(name, value)| (name.to_string(), value.clone()))
				.collect(),
		)
	};
	let string = |value: &str| Amf0Value::Utf8String(value.into());

	let connect = vec![
		string("connect"),
		Amf0Value::Number(1.0),
		object(&[
			("app", string("live")),
			("tcUrl", string("rtmp://localhost/live")),
			("fpad", Amf0Value::Boolean(false)),
			(
				"fourCcList",
				Amf0Value::StrictArray(vec![string("av01"), string("hvc1")]),
			),
		]),
	];
	let publish = vec![
		string("publish"),
		Amf0Value::Number(5.0),
		Amf0Value::Null,
		string("stream"),
		string("live"),
	];
	let metadata = vec![
		string("@setDataFrame"),
		string("onMetaData"),
		object(&[
			("width", Amf0Value::Number(1280.0)),
			("height", Amf0Value::Number(720.0)),
			("videocodecid", Amf0Value::Number(7.0)),
			("framerate", Amf0Value::Number(30.0)),
			("encoder", string("obs")),
			("nested", object(&[("undefined", Amf0Value::Undefined)])),
		]),
	];

	let mut seeds = Vec::new();
	for (selector, values) in [
		(1u8, &connect),
		(3, &connect),
		(3, &publish),
		(0, &metadata),
		(2, &metadata),
	] {
		let mut data = vec![selector];
		// An AMF3 command wraps its AMF0 behind a zero byte.
		if TYPES[selector as usize] == 17 {
			data.push(0);
		}
		data.extend(serialize(values).expect("the seed serializes"));
		seeds.push(Seed { target: "amf0", data });
	}
	seeds
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every seed decodes, so the fuzzer starts past the first marker.
	#[test]
	fn seeds_are_accepted() {
		for (index, seed) in seeds().iter().enumerate() {
			assert!(amf0(&seed.data), "seed {index} refused");
		}
	}

	/// Replay the inputs committed under `fuzz/regressions/`, so a bug the fuzzer found stays
	/// fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has failed yet for this target; `fuzz/README.md` says what to drop in here
			// when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}

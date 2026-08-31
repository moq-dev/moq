//! The group header and wire ops, shared by the encoder and decoder.

use serde::{Deserialize, Serialize};

/// The first frame in every group, naming the complete retained window.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Header<T> {
	/// Absolute index of `records[0]`, so a reader knows which records it missed.
	pub offset: u64,
	/// The retained records, oldest first.
	pub records: Vec<T>,
}

/// An incremental frame after the group header.
///
/// A `push` takes the next index and a `pop` drops from the front, both positional against the
/// header that opened the group. That is sound because a reader never sees a subset of a group.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) enum Op<T> {
	/// Append one record to the back.
	Push(T),
	/// Drop this many records from the front.
	Pop(u64),
}

#[cfg(test)]
mod test {
	use serde_json::{Value, json};

	use super::*;

	#[derive(serde::Deserialize)]
	struct Vector {
		name: String,
		first: bool,
		frame: String,
	}

	/// The exact frame bytes for each op, shared with the TypeScript suite.
	///
	/// This file is the wire contract between the two implementations. A change here is a format
	/// change, so both suites have to move together.
	#[test]
	fn vectors_round_trip() {
		let vectors: Vec<Vector> = serde_json::from_str(include_str!("../../tests/window-vectors.json")).unwrap();

		for vector in vectors {
			let encoded = match vector.first {
				true => {
					let header: Header<Value> =
						serde_json::from_str(&vector.frame).unwrap_or_else(|err| panic!("{}: {err}", vector.name));
					serde_json::to_string(&header).unwrap()
				}
				false => {
					let op: Op<Value> =
						serde_json::from_str(&vector.frame).unwrap_or_else(|err| panic!("{}: {err}", vector.name));
					serde_json::to_string(&op).unwrap()
				}
			};
			assert_eq!(encoded, vector.frame, "{}", vector.name);
		}
	}

	#[test]
	fn frames_have_the_expected_shapes() {
		// Pinning the shapes the vectors encode, so a rename of a variant or field is a visible break
		// rather than a silently different wire.
		let header = Header {
			offset: 4,
			records: vec![json!({ "a": 1 })],
		};
		assert_eq!(
			serde_json::to_value(&header).unwrap(),
			json!({ "offset": 4, "records": [{ "a": 1 }] })
		);
		assert_eq!(
			serde_json::to_value(Op::Push(json!({ "a": 1 }))).unwrap(),
			json!({ "push": { "a": 1 } })
		);
		assert_eq!(serde_json::to_value(Op::<Value>::Pop(2)).unwrap(), json!({ "pop": 2 }));
	}
}

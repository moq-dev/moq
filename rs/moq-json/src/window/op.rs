//! The wire ops, shared by the encoder and decoder.

use serde::{Deserialize, Serialize};

/// One frame: what the publisher did to its window.
///
/// Externally tagged, so a frame is `{"reset":{...}}`, `{"push":...}`, or `{"pop":n}` and a reader
/// can tell them apart without position. Only [`Reset`](Self::Reset) carries an index; a `push`
/// takes the next one and a `pop` drops from the front, both positional against the reset that
/// opened the group. That is sound because the group-scoped DEFLATE window already makes a
/// mid-group join undecodable, so a reader never sees a subset of a group's frames.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) enum Op<T> {
	/// The window is exactly these records, the first at `offset`. Opens every group.
	Reset {
		/// Absolute index of `records[0]`, so a reader knows which records it missed.
		offset: u64,
		records: Vec<T>,
	},
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
			let op: Op<Value> =
				serde_json::from_str(&vector.frame).unwrap_or_else(|err| panic!("{}: {err}", vector.name));
			let encoded = serde_json::to_string(&op).unwrap();
			assert_eq!(encoded, vector.frame, "{}", vector.name);
		}
	}

	#[test]
	fn ops_are_externally_tagged() {
		// Pinning the shapes the vectors encode, so a rename of a variant or field is a visible break
		// rather than a silently different wire.
		let reset = Op::Reset {
			offset: 4,
			records: vec![json!({ "a": 1 })],
		};
		assert_eq!(
			serde_json::to_value(&reset).unwrap(),
			json!({ "reset": { "offset": 4, "records": [{ "a": 1 }] } })
		);
		assert_eq!(
			serde_json::to_value(Op::Push(json!({ "a": 1 }))).unwrap(),
			json!({ "push": { "a": 1 } })
		);
		assert_eq!(serde_json::to_value(Op::<Value>::Pop(2)).unwrap(), json!({ "pop": 2 }));
	}
}

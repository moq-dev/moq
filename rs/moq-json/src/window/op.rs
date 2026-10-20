//! The group header and wire ops, shared by the encoder and decoder.

use serde::{Deserialize, Serialize};

/// The first frame in every group, naming the retained window and a decodable suffix.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Header<T> {
	/// Absolute index of the oldest retained record.
	pub offset: u64,
	/// Absolute index of `records[0]`; omitted when it equals `offset`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub start: Option<u64>,
	/// A suffix of the retained records, oldest first.
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
		events: Vec<Value>,
	}

	/// The exact frame bytes and decoded events, shared with the TypeScript suite.
	///
	/// Frame strings are the wire contract between the two implementations. Changes to those
	/// strings are format changes, while expected events pin decoder semantics.
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

			let mut decoder = super::super::Decoder::<Value>::new(Default::default());
			let mut group = decoder.group();
			if !vector.first {
				group
					.decode(br#"{"offset":0,"records":[{"n":0},{"n":1},{"n":2}]}"#)
					.unwrap();
				while group.next_event().is_some() {}
			}
			group.decode(vector.frame.as_bytes()).unwrap();
			let events: Vec<_> = std::iter::from_fn(|| group.next_event())
				.map(|event| {
					use super::super::Event;
					match event {
						Event::Push { index, value } => json!({"push": {"index": index, "value": value}}),
						Event::Pop(range) => json!({"pop": {"start": range.start, "end": range.end}}),
						Event::Skip(range) => json!({"skip": {"start": range.start, "end": range.end}}),
					}
				})
				.collect();
			assert_eq!(events, vector.events, "{}", vector.name);
		}
	}

	#[test]
	fn frames_have_the_expected_shapes() {
		// Pinning the shapes the vectors encode, so a rename of a variant or field is a visible break
		// rather than a silently different wire.
		let header = Header {
			offset: 4,
			start: None,
			records: vec![json!({ "a": 1 })],
		};
		assert_eq!(
			serde_json::to_value(&header).unwrap(),
			json!({ "offset": 4, "records": [{ "a": 1 }] })
		);
		let checkpoint = Header {
			offset: 4,
			start: Some(6),
			records: vec![json!({ "a": 3 })],
		};
		assert_eq!(
			serde_json::to_value(&checkpoint).unwrap(),
			json!({ "offset": 4, "start": 6, "records": [{ "a": 3 }] })
		);
		assert_eq!(
			serde_json::to_value(Op::Push(json!({ "a": 1 }))).unwrap(),
			json!({ "push": { "a": 1 } })
		);
		assert_eq!(serde_json::to_value(Op::<Value>::Pop(2)).unwrap(), json!({ "pop": 2 }));
	}
}

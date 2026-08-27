use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How a metadata track's groups carry its payloads.
///
/// The two modes are equally primary and there is no safe default: reading an append log as a
/// latest-value document silently drops every record but the last. So a
/// [`JsonConfig`](crate::catalog::JsonConfig) or [`BinaryConfig`](crate::catalog::BinaryConfig)
/// always states its mode, and a consumer that doesn't recognize the value
/// ([`Unknown`](Self::Unknown)) must ignore the track rather than guess.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
	/// Lossy latest-value: each group carries one complete payload and supersedes the previous
	/// group, so a consumer reads only the newest.
	///
	/// JSON tracks may follow the first frame of a group with
	/// [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) merge-patch deltas; binary tracks
	/// carry a single frame per group.
	Snapshot,

	/// Lossless append log: a single group that is never rolled, one payload per frame, delivered
	/// in order with nothing superseded.
	Stream,

	/// A mode this build does not recognize, preserved verbatim.
	///
	/// A consumer MUST ignore the track: it cannot know whether skipping to the newest group
	/// would lose records.
	Unknown(String),
}

impl Mode {
	/// The mode as it appears on the wire.
	pub fn as_str(&self) -> &str {
		match self {
			Self::Snapshot => "snapshot",
			Self::Stream => "stream",
			Self::Unknown(other) => other,
		}
	}
}

impl Serialize for Mode {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Mode {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let raw = String::deserialize(deserializer)?;
		Ok(match raw.as_str() {
			"snapshot" => Self::Snapshot,
			"stream" => Self::Stream,
			_ => Self::Unknown(raw),
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn known_roundtrip() {
		for (mode, json) in [(Mode::Snapshot, r#""snapshot""#), (Mode::Stream, r#""stream""#)] {
			assert_eq!(serde_json::to_string(&mode).unwrap(), json);
			assert_eq!(serde_json::from_str::<Mode>(json).unwrap(), mode);
		}
	}

	#[test]
	fn unknown_roundtrip() {
		let parsed: Mode = serde_json::from_str(r#""future""#).unwrap();
		assert_eq!(parsed, Mode::Unknown("future".to_string()));
		assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""future""#);
	}
}

use crate::Error;

use derive_more::Display;
use std::str::FromStr;

/// How a metadata track's groups carry its payloads.
///
/// The two modes are equally primary and there is no safe default: reading an append log as a
/// latest-value document silently drops every record but the last. So a
/// [`JsonConfig`](crate::catalog::JsonConfig) or [`BinaryConfig`](crate::catalog::BinaryConfig)
/// always states its mode, and a consumer that doesn't recognize the value
/// ([`Unknown`](Self::Unknown)) must ignore the track rather than guess.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
#[non_exhaustive]
pub enum Mode {
	/// Lossy latest-value: each group carries one complete payload and supersedes the previous
	/// group, so a consumer reads only the newest.
	///
	/// JSON tracks may follow the first frame of a group with
	/// [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) merge-patch deltas; binary tracks
	/// carry a single frame per group.
	#[display("snapshot")]
	Snapshot,

	/// Lossless append log: a single group that is never rolled, one payload per frame, delivered
	/// in order with nothing superseded.
	#[display("stream")]
	Stream,

	/// A mode this build does not recognize, preserved verbatim.
	///
	/// A consumer MUST ignore the track: it cannot know whether skipping to the newest group
	/// would lose records.
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for Mode {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"snapshot" => Self::Snapshot,
			"stream" => Self::Stream,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn known_roundtrip() {
		for (mode, json) in [(Mode::Snapshot, r#""snapshot""#), (Mode::Stream, r#""stream""#)] {
			let config = crate::catalog::JsonConfig::new(mode.clone());
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["mode"].to_string(), json);
			assert_eq!(
				serde_json::from_value::<crate::catalog::JsonConfig>(encoded)
					.unwrap()
					.mode,
				mode
			);
		}
	}

	#[test]
	fn unknown_roundtrip() {
		let wire = serde_json::json!({ "mode": "future" });
		let config: crate::catalog::JsonConfig = serde_json::from_value(wire.clone()).unwrap();
		assert_eq!(config.mode, Mode::Unknown("future".to_string()));
		assert_eq!(serde_json::to_value(&config).unwrap(), wire);
	}
}

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_with::{base64::Base64, serde_as};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// JSON examples:
/// ```json
/// { "kind": "cmaf", "init": "<base64-encoded ftyp+moov>" }
/// { "kind": "loc" }
/// ```
///
/// An unrecognized `kind` decodes to [`Container::Unknown`] instead of failing, so one
/// rendition using a future container does not take down the rest of the catalog. Such a
/// rendition must be ignored by consumers.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Container {
	/// A QUIC VarInt timestamp prefix followed by the raw codec payload.
	/// Timestamps are in microseconds.
	#[default]
	Legacy,

	/// Fragmented MP4: each frame is a complete moof+mdat fragment.
	Cmaf {
		/// CMAF init segment (ftyp+moov). Encoded as base64 over the wire.
		init: Bytes,
	},

	/// Low Overhead Container (draft-ietf-moq-loc): each frame is a small
	/// property block followed by the codec payload.
	Loc,

	/// A container this build does not recognize, preserved verbatim.
	Unknown(UnknownContainer),
}

/// The raw JSON of a container whose `kind` is not recognized.
///
/// Kept intact so a relay or transcoder that reparses and republishes a catalog round-trips
/// the rendition byte-for-byte rather than corrupting it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownContainer(serde_json::Map<String, serde_json::Value>);

impl UnknownContainer {
	/// The `kind` as it appeared on the wire, or `None` if it was absent or not a string.
	pub fn kind(&self) -> Option<&str> {
		self.0.get("kind").and_then(serde_json::Value::as_str)
	}

	/// The full JSON object, including `kind`.
	pub fn fields(&self) -> &serde_json::Map<String, serde_json::Value> {
		&self.0
	}
}

/// The containers this build knows how to encode and decode.
///
/// Split out so the tagged representation stays derived while [`Container`] keeps a catch-all.
#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
enum Known {
	#[serde(rename = "legacy")]
	Legacy,
	Cmaf {
		#[serde_as(as = "Base64")]
		init: Bytes,
	},
	Loc,
}

impl Serialize for Container {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let known = match self {
			Self::Legacy => Known::Legacy,
			// Bytes is refcounted, so this clone is cheap.
			Self::Cmaf { init } => Known::Cmaf { init: init.clone() },
			Self::Loc => Known::Loc,
			Self::Unknown(unknown) => return unknown.0.serialize(serializer),
		};

		known.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Container {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let object = serde_json::Map::deserialize(deserializer)?;

		// Only route to the derived enum for kinds we know, so a malformed known container is
		// still a hard error instead of silently becoming Unknown.
		match object.get("kind").and_then(serde_json::Value::as_str) {
			Some("legacy" | "cmaf" | "loc") => {
				let known = Known::deserialize(serde_json::Value::Object(object)).map_err(de::Error::custom)?;
				Ok(match known {
					Known::Legacy => Self::Legacy,
					Known::Cmaf { init } => Self::Cmaf { init },
					Known::Loc => Self::Loc,
				})
			}
			_ => Ok(Self::Unknown(UnknownContainer(object))),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn legacy_roundtrip() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"legacy"}"#).unwrap();
		assert_eq!(parsed, Container::Legacy);
		assert_eq!(serde_json::to_string(&parsed).unwrap(), r#"{"kind":"legacy"}"#);
	}

	#[test]
	fn cmaf_roundtrip() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"cmaf","init":"AAEC"}"#).unwrap();
		assert_eq!(
			parsed,
			Container::Cmaf {
				init: Bytes::from_static(&[0, 1, 2])
			}
		);
		assert_eq!(
			serde_json::to_string(&parsed).unwrap(),
			r#"{"kind":"cmaf","init":"AAEC"}"#
		);
	}

	#[test]
	fn loc_roundtrip() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"loc"}"#).unwrap();
		assert_eq!(parsed, Container::Loc);
		assert_eq!(serde_json::to_string(&parsed).unwrap(), r#"{"kind":"loc"}"#);
	}

	#[test]
	fn unknown_roundtrip() {
		// Keys are sorted because serde_json::Map is a BTreeMap by default.
		let json = r#"{"extra":{"nested":[1,2]},"flag":true,"kind":"future"}"#;
		let parsed: Container = serde_json::from_str(json).unwrap();

		let Container::Unknown(unknown) = &parsed else {
			panic!("expected unknown: {parsed:?}");
		};
		assert_eq!(unknown.kind(), Some("future"));
		assert_eq!(unknown.fields().len(), 3);

		assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
	}

	#[test]
	fn malformed_known_kind_errors() {
		// cmaf without init is not a valid cmaf container and must not degrade to Unknown.
		serde_json::from_str::<Container>(r#"{"kind":"cmaf"}"#).unwrap_err();
	}
}

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// - "legacy": QUIC VarInt timestamp prefix followed by the raw codec payload.
///   Timestamps are in microseconds.
/// - "cmaf": Fragmented MP4 - frames contain complete moof+mdat fragments. The
///   init segment (ftyp+moov) is base64-encoded in the catalog.
/// - "loc": Low Overhead Container (draft-ietf-moq-loc). Each frame is a small
///   property block followed by the codec payload.
///
/// JSON examples:
/// ```json
/// { "kind": "cmaf", "init": "<base64-encoded ftyp+moov>" }
/// { "kind": "loc" }
/// ```
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	Cmaf {
		/// CMAF init segment (ftyp+moov). Encoded as base64 over the wire.
		#[serde_as(as = "Base64")]
		init: Bytes,

		/// Duplicates `mdhd.timescale` from `init`. Emitted for backwards
		/// compatibility with players that predate the `init` field.
		#[deprecated(note = "parse from `init` instead")]
		#[serde(default, skip_serializing_if = "Option::is_none")]
		timescale: Option<u32>,

		/// Duplicates `tkhd.track_id` from `init`. Emitted for backwards
		/// compatibility with players that predate the `init` field.
		#[deprecated(note = "parse from `init` instead")]
		#[serde(default, skip_serializing_if = "Option::is_none")]
		track_id: Option<u32>,
	},
	Loc,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn loc_roundtrip() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"loc"}"#).unwrap();
		assert_eq!(parsed, Container::Loc);

		let json = serde_json::to_string(&parsed).unwrap();
		assert_eq!(json, r#"{"kind":"loc"}"#);
	}
}

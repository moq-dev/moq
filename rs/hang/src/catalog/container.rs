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
///   property block followed by the codec payload. The catalog `timescale` is
///   the fallback used when a frame has no per-frame 0x08 timescale property.
///   Defaults to 1_000_000 (microseconds) when omitted.
///
/// JSON examples:
/// ```json
/// { "kind": "cmaf", "init": "<base64-encoded ftyp+moov>" }
/// { "kind": "loc", "timescale": 90000 }
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
	},
	Loc {
		/// Catalog-level timescale (units per second) used when a LOC frame
		/// omits its own 0x08 timescale property. Defaults to 1_000_000
		/// (microseconds).
		#[serde(default = "default_loc_timescale")]
		timescale: u64,
	},
}

fn default_loc_timescale() -> u64 {
	1_000_000
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn loc_default_timescale() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"loc"}"#).unwrap();
		assert_eq!(parsed, Container::Loc { timescale: 1_000_000 });
	}

	#[test]
	fn loc_explicit_timescale_roundtrip() {
		let parsed: Container = serde_json::from_str(r#"{"kind":"loc","timescale":90000}"#).unwrap();
		assert_eq!(parsed, Container::Loc { timescale: 90_000 });

		let json = serde_json::to_string(&parsed).unwrap();
		assert_eq!(json, r#"{"kind":"loc","timescale":90000}"#);
	}
}

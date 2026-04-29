use serde::{Deserialize, Serialize};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads.
///   Timestamps are in microseconds.
/// - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
///   The init segment (ftyp+moov) is base64-encoded in the catalog.
///
/// JSON example:
/// ```json
/// { "kind": "cmaf", "initData": "<base64-encoded ftyp+moov>" }
/// ```
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	Cmaf {
		/// Base64-encoded init segment (ftyp+moov)
		#[serde(rename = "initData")]
		init_data: String,
	},
}

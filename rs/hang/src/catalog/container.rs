use serde::{Deserialize, Serialize};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads
/// - "raw": Uses fixed u64 encoding (8 bytes, big-endian), raw frame payloads  
/// - "fmp4": Fragmented MP4 container - frames contain complete moof+mdat fragments
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	#[serde(rename = "raw")]
	Raw,
	#[serde(rename = "fmp4")]
	Fmp4,
}

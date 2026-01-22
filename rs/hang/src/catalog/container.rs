use serde::{Deserialize, Serialize};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads
/// - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments and an init track
///
/// JSON example:
/// {
///   kind: "cmaf",
///   init: "init track name",
/// }
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	Cmaf {
		init: moq_lite::Track,
	},
}

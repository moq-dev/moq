use serde::{Deserialize, Serialize};

/// Container format for frame timestamp encoding and frame payload structure.
///
/// - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads.
///   Timestamps are in microseconds.
/// - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
///   Timestamps are in timescale units.
///
/// JSON example:
/// ```json
/// { "kind": "cmaf", "timescale": 1000000, "trackId": 1 }
/// ```
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	Cmaf {
		/// Time units per second
		timescale: u64,
		/// Track ID used in the moof/mdat fragments
		#[serde(rename = "trackId")]
		track_id: u32,
	},
}

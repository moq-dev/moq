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
///
/// Marked `#[non_exhaustive]` so new container formats can be added without bumping the major
/// version. External `match`es need a wildcard arm; constructing the existing variants is
/// unaffected.
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "kind")]
#[non_exhaustive]
pub enum Container {
	#[serde(rename = "legacy")]
	#[default]
	Legacy,
	Cmaf {
		/// CMAF init segment (ftyp+moov). Encoded as base64 over the wire.
		#[serde_as(as = "Base64")]
		init: Bytes,
	},
	Loc,
}

impl Container {
	/// The `kind` tag this container serializes as, e.g. `"cmaf"`.
	///
	/// Useful in logs and errors, where the variant's payload (a whole init segment) has no place.
	pub fn kind(&self) -> &'static str {
		match self {
			Self::Legacy => "legacy",
			Self::Cmaf { .. } => "cmaf",
			Self::Loc => "loc",
		}
	}

	/// The out-of-band init segment (ftyp+moov) this container needs to decode its frames, if any.
	///
	/// `None` means frames are self-describing given the catalog's codec config, so a consumer
	/// that needs an init segment has to synthesize one.
	pub fn init(&self) -> Option<&Bytes> {
		match self {
			Self::Cmaf { init } => Some(init),
			Self::Legacy | Self::Loc => None,
		}
	}

	/// Whether each frame is a raw codec bitstream, modulo a small per-frame header.
	///
	/// True for `legacy` (VarInt timestamp prefix) and `loc` (property block). False for `cmaf`,
	/// whose frames are complete moof+mdat fragments. Consumers that re-emit raw codec payloads
	/// (MPEG-TS, MKV, FLV) can only accept a container where this holds.
	pub fn is_raw(&self) -> bool {
		match self {
			Self::Legacy | Self::Loc => true,
			Self::Cmaf { .. } => false,
		}
	}
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

	/// `kind()` has to keep matching the serde tag, since that's the whole point of it.
	#[test]
	fn kind_matches_serde_tag() {
		for container in [
			Container::Legacy,
			Container::Cmaf { init: Bytes::new() },
			Container::Loc,
		] {
			let json: serde_json::Value = serde_json::to_value(&container).unwrap();
			assert_eq!(json["kind"], container.kind());
		}
	}

	#[test]
	fn init_only_for_cmaf() {
		let init = Bytes::from_static(b"ftyp");
		assert_eq!(Container::Cmaf { init: init.clone() }.init(), Some(&init));
		assert_eq!(Container::Legacy.init(), None);
		assert_eq!(Container::Loc.init(), None);
	}

	#[test]
	fn raw_containers_carry_codec_bitstreams() {
		assert!(Container::Legacy.is_raw());
		assert!(Container::Loc.is_raw());
		assert!(!Container::Cmaf { init: Bytes::new() }.is_raw());
	}
}

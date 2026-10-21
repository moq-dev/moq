use std::str::FromStr;

use derive_more::Display;

/// How a rendition packs each eye's view into its frames.
///
/// Absent means [`Mono`](Self::Mono). An unrecognized value decodes to
/// [`Stereo::Unknown`] instead of failing, and a consumer treats it as absent.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
#[non_exhaustive]
pub enum Stereo {
	/// One view, shown to both eyes.
	#[display("mono")]
	Mono,

	/// The left eye's view in the left half of each frame, the right eye's in the right half.
	#[display("left-right")]
	LeftRight,

	/// The left eye's view in the top half of each frame, the right eye's in the bottom half.
	#[display("top-bottom")]
	TopBottom,

	/// A packing this build does not recognize, preserved verbatim.
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for Stereo {
	type Err = crate::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"mono" => Self::Mono,
			"left-right" => Self::LeftRight,
			"top-bottom" => Self::TopBottom,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::{VideoCodec, VideoConfig};

	use super::*;

	#[test]
	fn round_trips() {
		for (stereo, json) in [
			(Stereo::Mono, "mono"),
			(Stereo::LeftRight, "left-right"),
			(Stereo::TopBottom, "top-bottom"),
			(Stereo::Unknown("multiview".to_string()), "multiview"),
		] {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			config.stereo = Some(stereo.clone());
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["stereo"], json);
			assert_eq!(
				serde_json::from_value::<VideoConfig>(encoded).unwrap().stereo,
				Some(stereo)
			);
		}
	}

	#[test]
	fn non_string_is_refused() {
		let wire = serde_json::json!({ "codec": "vp8", "stereo": 2 });
		serde_json::from_value::<VideoConfig>(wire).unwrap_err();
	}
}

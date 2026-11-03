use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::millis::MillisCeil;

/// A rendition's HLS segment records, which exist only for HLS compatibility.
///
/// Optional, and only on video and audio renditions. Each rendition numbers its own segments. A
/// rendition without it is not HLS-renderable. See the [`hls`](crate::hls) module for the
/// records.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Hls {
	/// The track carrying the rendition's segment records, on the catalog's broadcast even when the
	/// media lives in a sibling. Conventionally the media track's name plus
	/// [`SUFFIX`](crate::hls::SUFFIX).
	pub track: String,

	/// The nominal segment duration, fixed for the life of the track. Serialized as whole
	/// milliseconds, rounded up.
	#[serde_as(as = "MillisCeil")]
	pub duration: Duration,
}

impl Hls {
	/// Records on `track`, with segments of nominal `duration`.
	pub fn new(track: impl Into<String>, duration: Duration) -> Self {
		Self {
			track: track.into(),
			duration,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrips_in_milliseconds() {
		let hls = Hls::new("video.hls", Duration::from_micros(4_000_500));
		let json = serde_json::to_string(&hls).unwrap();
		assert_eq!(json, r#"{"track":"video.hls","duration":4001}"#);
		assert_eq!(
			serde_json::from_str::<Hls>(&json).unwrap(),
			Hls::new("video.hls", Duration::from_millis(4001))
		);
	}
}

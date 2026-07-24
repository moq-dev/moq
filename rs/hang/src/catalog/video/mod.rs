mod av1;
mod codec;
mod h264;
mod h265;
mod vp9;

pub use av1::*;
pub use codec::*;
pub use h264::*;
pub use h265::*;
pub use vp9::*;

use std::collections::{BTreeMap, btree_map};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, DurationMilliSeconds, hex::Hex};

use crate::catalog::Container;

/// Information about a video track in the catalog.
///
/// This struct contains a map of renditions (different quality/codec options)
/// and optional metadata like detection, display settings, rotation, and flip.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without
/// bumping the major version. External callers start from [`Video::default`] and
/// fill in what they need ([`insert`](Self::insert) for renditions); struct-literal
/// construction (with or without `..base`) is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Video {
	/// A map of track name to rendition configuration.
	/// This is not an array in order for it to work with JSON Merge Patch.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, VideoConfig>,

	/// Render the video at this size in pixels.
	/// This is separate from the display aspect ratio because it does not require reinitialization.
	#[serde(default)]
	pub display: Option<Display>,

	/// The clockwise rotation of the video in degrees, normalized to the nearest multiple of 90 degrees.
	/// Default: 0
	#[serde(default)]
	pub rotation: Option<f64>,

	/// If true, the decoder will flip the video horizontally
	/// Default: false
	#[serde(default)]
	pub flip: Option<bool>,
}

/// Video presentation metadata applied to all video renditions in the catalog.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct VideoPresentation {
	/// Render the video at this final size after rotation, or clear the explicit size when absent.
	pub display: Option<Display>,

	/// Apply this clockwise rotation before rendering, or clear it when absent.
	pub rotation: Option<f64>,

	/// Flip horizontally after rotation, or clear the explicit value when absent.
	pub flip: Option<bool>,
}

impl VideoPresentation {
	fn normalized(mut self) -> crate::Result<Self> {
		self.rotation = self.rotation.map(normalize_video_rotation).transpose()?;
		Ok(self)
	}
}

fn normalize_video_rotation(rotation: f64) -> crate::Result<f64> {
	if !rotation.is_finite() {
		return Err(crate::Error::InvalidVideoRotation);
	}

	let normalized = rotation.rem_euclid(360.0);
	Ok(((normalized / 90.0).round() as u16 % 4) as f64 * 90.0)
}

impl Video {
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: VideoConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return the configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<VideoConfig> {
		self.renditions.remove(name)
	}

	/// Normalize and replace the video presentation metadata as one catalog update.
	pub fn set_presentation(&mut self, presentation: VideoPresentation) -> crate::Result<()> {
		let presentation = presentation.normalized()?;
		self.display = presentation.display;
		self.rotation = presentation.rotation;
		self.flip = presentation.flip;
		Ok(())
	}
}

/// Display size for rendering video
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Display {
	pub width: u32,
	pub height: u32,
}

/// Video decoder configuration based on WebCodecs VideoDecoderConfig.
///
/// This struct contains all the information needed to initialize a video decoder,
/// including codec-specific parameters, resolution, and optional metadata.
///
/// Reference: <https://www.w3.org/TR/webcodecs/#video-decoder-config>
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added
/// without bumping the major version. External callers build a config with
/// [`VideoConfig::new`] and then assign whichever optional fields they need;
/// struct-literal construction (with or without `..base`) is not available
/// outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VideoConfig {
	/// Optional reference to another broadcast that publishes this track, expressed
	/// relative to the broadcast that served this catalog (e.g. `../source`). If unset,
	/// the track lives in the same broadcast as the catalog.
	///
	/// This allows a transcoder to author a downstream catalog that points unchanged
	/// renditions at the source broadcast without re-publishing the bytes.
	#[serde(default)]
	pub broadcast: Option<moq_net::PathRelativeOwned>,

	/// The codec, see the registry for details:
	/// <https://w3c.github.io/webcodecs/codec_registry.html>
	#[serde_as(as = "DisplayFromStr")]
	pub codec: VideoCodec,

	/// Information used to initialize the decoder on a per-codec basis.
	///
	/// One of the best examples is H264, which needs the sps/pps to function.
	/// If not provided, this information is (automatically) inserted before each key-frame (marginally higher overhead).
	#[serde(default)]
	#[serde_as(as = "Option<Hex>")]
	pub description: Option<Bytes>,

	/// The encoded width/height of the media.
	///
	/// This is optional because it can be changed in-band for some codecs.
	/// It's primarily a hint to allocate the correct amount of memory up-front.
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,

	/// The display aspect ratio of the media.
	///
	/// This allows you to stretch/shrink pixels of the video.
	/// If not provided, the display aspect ratio is 1:1
	///
	/// The `displayRatio*` aliases decode catalogs from publishers predating the
	/// rename to `displayAspect*`; the current name is what we emit.
	#[serde(alias = "displayRatioWidth")]
	pub display_aspect_width: Option<u32>,
	#[serde(alias = "displayRatioHeight")]
	pub display_aspect_height: Option<u32>,

	// TODO color space
	/// The maximum bitrate of the video track, if known.
	#[serde(default)]
	pub bitrate: Option<u64>,

	/// The frame rate of the video track, if known.
	#[serde(default)]
	pub framerate: Option<f64>,

	/// If true, the decoder will optimize for latency.
	///
	/// Default: true
	#[serde(default)]
	pub optimize_for_latency: Option<bool>,

	/// Container format for frame encoding.
	/// Defaults to "legacy" for backward compatibility.
	#[serde(default)]
	pub container: Container,

	/// The maximum jitter before the next frame is emitted in milliseconds.
	/// The player's jitter buffer should be larger than this value.
	/// If not provided, the player should assume each frame is flushed immediately.
	///
	/// Serialized as an integer number of milliseconds (sub-ms precision is truncated).
	///
	/// ex:
	/// - If each frame is flushed immediately, this would be 1000/fps.
	/// - If there can be up to 3 b-frames in a row, this would be 3 * 1000/fps.
	/// - If frames are buffered into 2s segments, this would be 2s.
	#[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
	#[serde(default)]
	pub jitter: Option<std::time::Duration>,

	/// The companion timeline track indexing this rendition's groups, if the publisher
	/// offers one. See [`Timeline`](crate::catalog::Timeline).
	#[serde(default)]
	pub timeline: Option<crate::catalog::Timeline>,
}

impl VideoConfig {
	/// Construct a config with the required codec set and every optional
	/// field cleared. `container` defaults to [`Container::default`]. Fields
	/// are `pub`, so callers set whatever they need by assignment afterwards.
	///
	/// This is the only path external crates have to build a `VideoConfig`
	/// since the type is `#[non_exhaustive]`.
	pub fn new(codec: impl Into<VideoCodec>) -> Self {
		Self {
			broadcast: None,
			codec: codec.into(),
			description: None,
			coded_width: None,
			coded_height: None,
			display_aspect_width: None,
			display_aspect_height: None,
			bitrate: None,
			framerate: None,
			optimize_for_latency: None,
			container: Container::default(),
			jitter: None,
			timeline: None,
		}
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::{Container, H264};

	use super::*;

	#[test]
	fn display_aspect_uses_canonical_json_names() {
		let mut config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		config.display_aspect_width = Some(4);
		config.display_aspect_height = Some(3);
		config.container = Container::Legacy;

		let encoded = serde_json::to_value(config).expect("failed to encode");
		assert_eq!(encoded["displayAspectWidth"], 4);
		assert_eq!(encoded["displayAspectHeight"], 3);
		assert!(encoded.get("displayRatioWidth").is_none());
		assert!(encoded.get("displayRatioHeight").is_none());
	}

	#[test]
	fn decodes_legacy_display_ratio_keys() {
		// A catalog serialized by a pre-0.20 publisher used displayRatio*; the
		// alias keeps the aspect ratio from being silently dropped.
		let json = serde_json::json!({
			"codec": "avc1.640028",
			"displayRatioWidth": 16,
			"displayRatioHeight": 9,
		});
		let config: VideoConfig = serde_json::from_value(json).expect("failed to decode legacy keys");
		assert_eq!(config.display_aspect_width, Some(16));
		assert_eq!(config.display_aspect_height, Some(9));
	}
}

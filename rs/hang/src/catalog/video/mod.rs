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

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, hex::Hex};

use crate::catalog::Container;

/// Information about a video track in the catalog.
///
/// This struct contains a map of renditions (different quality/codec options)
/// and optional metadata like detection, display settings, rotation, and flip.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Video {
	/// A map of track name to rendition configuration.
	/// This is not an array in order for it to work with JSON Merge Patch.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, VideoConfig>,

	/// The priority of the video track, relative to other tracks in the broadcast.
	pub priority: u8,

	/// Render the video at this size in pixels.
	/// This is separate from the display aspect ratio because it does not require reinitialization.
	#[serde(default)]
	pub display: Option<Display>,

	/// The rotation of the video in degrees.
	/// Default: 0
	#[serde(default)]
	pub rotation: Option<f64>,

	/// If true, the decoder will flip the video horizontally
	/// Default: false
	#[serde(default)]
	pub flip: Option<bool>,
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
/// Reference: <https://w3c.github.io/webcodecs/#video-decoder-config>
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoConfig {
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
	pub display_ratio_width: Option<u32>,
	pub display_ratio_height: Option<u32>,

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

	/// Minimum buffer size in milliseconds required for smooth playback.
	///
	/// This represents the minimum time the player should buffer before starting playback.
	/// For HLS imports, this is typically the segment duration.
	/// For fMP4 imports, this is detected from the fragment duration.
	///
	/// The player should add additional jitter buffer on top of this value.
	#[serde(default)]
	pub min_buffer: Option<moq_lite::Time>,
}

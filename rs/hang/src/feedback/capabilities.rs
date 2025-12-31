use crate::{AudioCodec, VideoCodec};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Capabilities {
	pub video: VideoCapabilities,
	pub audio: AudioCapabilities,
}

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoCapabilities {
	/// A list of codecs/configurations that we think we can decode with hardware acceleration.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub hardware: Vec<VideoCodec>,

	/// A list of codecs/configurations that we think we can decode with software decoding.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub software: Vec<VideoCodec>,

	/// A list of codecs/configurations that we know we cannot decode.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub unsupported: Vec<VideoCodec>,
}

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioCapabilities {
	/// A list of codecs that we think we can decode with hardware acceleration.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub hardware: Vec<AudioCodec>,

	/// A list of codecs that we think we can decode with software decoding.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub software: Vec<AudioCodec>,

	/// A list of codecs that we know we cannot decode.
	#[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
	pub unsupported: Vec<AudioCodec>,
}

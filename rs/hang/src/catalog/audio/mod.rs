mod aac;
mod codec;

pub use aac::*;
pub use codec::*;

use std::collections::BTreeMap;

use bytes::Bytes;

use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, base64::Base64, hex::Hex};

use crate::catalog::container::Container;

/// Information about an audio track in the catalog.
///
/// This struct contains a map of renditions (different quality/codec options)
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Audio {
	/// A map of track name to rendition configuration.
	/// This is not an array so it will work with JSON Merge Patch.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, AudioConfig>,

	/// The priority of the audio track, relative to other tracks in the broadcast.
	pub priority: u8,
}

/// Audio decoder configuration based on WebCodecs AudioDecoderConfig.
///
/// This struct contains all the information needed to initialize an audio decoder,
/// including codec-specific parameters, sample rate, and channel configuration.
///
/// Reference: <https://www.w3.org/TR/webcodecs/#audio-decoder-config>
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
	// The codec, see the registry for details:
	// https://w3c.github.io/webcodecs/codec_registry.html
	#[serde_as(as = "DisplayFromStr")]
	pub codec: AudioCodec,

	// The sample rate of the audio in Hz
	pub sample_rate: u32,

	// The number of channels in the audio
	#[serde(rename = "numberOfChannels")]
	pub channel_count: u32,

	// The bitrate of the audio track in bits per second
	#[serde(default)]
	pub bitrate: Option<u64>,

	// Some codecs include a description so the decoder can be initialized without extra data.
	// If not provided, there may be in-band metadata (marginally higher overhead).
	#[serde(default)]
	#[serde_as(as = "Option<Hex>")]
	pub description: Option<Bytes>,

	/// Container format for frame encoding.
	/// Defaults to "native" for backward compatibility.
	pub container: Container,

	/// Init segment (ftyp+moov) for CMAF/fMP4 containers.
	///
	/// This is the initialization segment needed for MSE playback.
	/// Stored as base64-encoded bytes and embedded in the catalog (as suggested
	/// in feedback). Init segments should not be sent over data tracks or at the
	/// start of each group.
	#[serde(default)]
	#[serde_as(as = "Option<Base64>")]
	pub init_segment: Option<Bytes>,
}

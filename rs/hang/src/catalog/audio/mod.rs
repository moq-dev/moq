mod aac;
mod codec;

pub use aac::*;
pub use codec::*;

use bytes::Bytes;
use moq_lite::Track;
use serde::{Deserialize, Serialize};
use serde_with::{hex::Hex, DisplayFromStr};
use std::collections::{btree_map, BTreeMap};

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
	pub renditions: BTreeMap<Track, AudioConfig>,

	/// The priority of the audio track, relative to other tracks in the broadcast.
	///
	/// TODO: Remove this; it's for backwards compatibility only
	#[serde(default)]
	pub priority: u8,
}

impl Audio {
	// Don't serialize if there are no renditions.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}

	/// Create a new audio track with a configuration and generate a unique name.
	pub fn create(&mut self, name: &str, config: AudioConfig) -> Track {
		let mut index = 0;

		loop {
			let track = Track::from(format!("audio:{}:{}", name, index));
			match self.renditions.entry(track.clone()) {
				btree_map::Entry::Vacant(entry) => {
					entry.insert(config);
					return track;
				}
				btree_map::Entry::Occupied(_) => index += 1,
			}
		}
	}

	/// Remove a audio track from the catalog.
	pub fn remove(&mut self, track: &Track) -> Option<AudioConfig> {
		self.renditions.remove(track)
	}
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
}

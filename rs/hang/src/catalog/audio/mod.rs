mod aac;
mod codec;

pub use aac::*;
pub use codec::*;

use std::collections::{BTreeMap, btree_map};

use bytes::Bytes;

use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, hex::Hex};

use crate::catalog::Container;

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
}

impl Audio {
	/// Create a new audio track with the given extension and configuration.
	pub fn create_track(&mut self, extension: &str, config: AudioConfig) -> moq_lite::Track {
		for i in 0.. {
			let name = match extension {
				"" => format!("audio{}", i),
				extension => format!("audio{}.{}", i, extension),
			};

			if let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.clone()) {
				entry.insert(config.clone());
				// TODO: Remove priority
				return moq_lite::Track { name, priority: 2 };
			}
		}

		unreachable!("no available audio track name");
	}

	// Remove the track from the catalog and return the configuration if found.
	pub fn remove_track(&mut self, track: &moq_lite::Track) -> Option<AudioConfig> {
		self.renditions.remove(track.name.as_str())
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

	/// Container format for frame encoding.
	/// Defaults to "legacy" for backward compatibility.
	#[serde(default)]
	pub container: Container,

	/// The maximum jitter before the next frame is emitted in milliseconds.
	/// The player's jitter buffer should be larger than this value.
	/// If not provided, the player should assume each frame is flushed immediately.
	///
	/// NOTE: The audio "frame" duration depends on the codec, sample rate, etc.
	/// ex: AAC often uses 1024 samples per frame, so at 44100Hz, this would be 1024/44100 = 23ms
	#[serde(default)]
	pub jitter: Option<moq_lite::Time>,
}

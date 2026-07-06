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
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: AudioConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return the configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<AudioConfig> {
		self.renditions.remove(name)
	}
}

/// Audio decoder configuration based on WebCodecs AudioDecoderConfig.
///
/// This struct contains all the information needed to initialize an audio decoder,
/// including codec-specific parameters, sample rate, and channel configuration.
///
/// Reference: <https://www.w3.org/TR/webcodecs/#audio-decoder-config>
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added
/// without bumping the major version. External callers build a config with
/// [`AudioConfig::new`] and then assign whichever optional fields they need;
/// struct-literal construction (with or without `..base`) is not available
/// outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
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

	/// Minimum additional latency required by this track in milliseconds.
	///
	/// This is added to the subscriber's own latency target for steady playback.
	///
	/// NOTE: The audio "frame" duration depends on the codec, sample rate, etc.
	/// ex: AAC often uses 1024 samples per frame, so at 44100Hz, this would be 1024/44100 = 23ms
	#[serde(default)]
	#[serde(rename = "latencyMin")]
	pub latency_min: Option<moq_net::Time>,

	#[doc(hidden)]
	#[serde(default)]
	pub jitter: Option<moq_net::Time>,
}

impl AudioConfig {
	/// Construct a config with the required fields set and every optional
	/// field cleared. `container` defaults to [`Container::default`]. Fields
	/// are `pub`, so callers set whatever they need by assignment afterwards.
	///
	/// This is the only path external crates have to build an `AudioConfig`
	/// since the type is `#[non_exhaustive]`.
	pub fn new(codec: impl Into<AudioCodec>, sample_rate: u32, channel_count: u32) -> Self {
		Self {
			codec: codec.into(),
			sample_rate,
			channel_count,
			bitrate: None,
			description: None,
			container: Container::default(),
			latency_min: None,
			jitter: None,
		}
	}

	/// The minimum additional latency required by this track.
	pub fn latency_min(&self) -> Option<moq_net::Time> {
		self.latency_min.or(self.jitter)
	}

	/// Set the minimum additional latency required by this track.
	pub fn set_latency_min(&mut self, latency_min: Option<moq_net::Time>) {
		self.latency_min = latency_min;
		self.jitter = latency_min;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn latency_min_accepts_legacy_jitter_and_serializes_both() {
		let old: AudioConfig = serde_json::from_str(
			r#"{"codec":"opus","sampleRate":48000,"numberOfChannels":2,"container":{"kind":"legacy"},"jitter":40}"#,
		)
		.unwrap();
		assert_eq!(old.latency_min(), Some(moq_net::Time::from_millis(40).unwrap()));

		let mut new = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		new.set_latency_min(Some(moq_net::Time::from_millis(40).unwrap()));

		let json = serde_json::to_value(new).unwrap();
		assert_eq!(json["latencyMin"], 40);
		assert_eq!(json["jitter"], 40);
	}
}

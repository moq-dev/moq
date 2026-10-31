mod aac;
mod codec;
mod layout;

pub use aac::*;
pub use codec::*;
pub use layout::*;

use std::collections::{BTreeMap, btree_map};

use bytes::Bytes;

use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;

use crate::catalog::Container;
use crate::catalog::hex::Hex;
use crate::catalog::millis::MillisCeil;

/// Information about an audio track in the catalog.
///
/// This struct contains a map of renditions (different quality/codec options)
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without
/// bumping the major version. External callers start from [`Audio::default`] and
/// fill in what they need ([`insert`](Self::insert) for renditions); struct-literal
/// construction (with or without `..base`) is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
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

	/// True when there are no renditions, so the section can be omitted from the catalog.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}

	/// Iterate enabled renditions in quality order.
	pub fn enabled(&self) -> impl Iterator<Item = (&String, &AudioConfig)> {
		self.ranked().filter(|(_, config)| config.enabled)
	}

	/// Iterate the renditions best first: enabled, then highest bitrate, then sample rate, then channel count.
	///
	/// A consumer that carries one rendition takes the first it supports, so the
	/// choice doesn't depend on how the tracks are named. An unknown bitrate ranks
	/// below a known one, and exact ties keep name order.
	pub fn ranked(&self) -> impl Iterator<Item = (&String, &AudioConfig)> {
		let mut ranked: Vec<_> = self.renditions.iter().collect();
		ranked.sort_by_key(|(_, config)| {
			std::cmp::Reverse((config.enabled, config.bitrate, config.sample_rate, config.channel_count))
		});
		ranked.into_iter()
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
	/// Optional reference to another broadcast that publishes this track, expressed
	/// relative to the broadcast that served this catalog (e.g. `./source`). If unset,
	/// the track lives in the same broadcast as the catalog.
	///
	/// Resolve it with [`Path::resolve`](moq_net::Path::resolve): a reference that walks
	/// above the root names no broadcast, so the catalog is rejected.
	#[serde(default)]
	pub broadcast: Option<moq_net::path::Relative>,

	/// The track's name in its broadcast, when it differs from this entry's ID: its key in the
	/// catalog. Absent means the ID is the track name, as in every catalog before this field.
	///
	/// Set it only when the ID can't be the name, such as two renditions of same-named tracks
	/// from different broadcasts. Readers resolve the name with
	/// [`Reference::track`](crate::catalog::Reference::track); one that predates the
	/// field subscribes to the ID instead.
	#[serde(default)]
	pub track: Option<String>,

	/// The publisher instance the reference names, when the catalog knows it. Absent names
	/// whichever instance the reader resolves first, as in every catalog before this field.
	///
	/// A reader on an epoch-aware route refuses another instance rather than fall back to it;
	/// on an epochless route it is best effort, since the route cannot prove the instance.
	/// Without [`broadcast`](Self::broadcast), it names the catalog's own instance.
	#[serde(default)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub epoch: Option<moq_net::Epoch>,

	/// Human-readable rendition name for track pickers.
	#[serde(default)]
	pub label: Option<String>,

	/// The codec, see the registry for details:
	/// <https://w3c.github.io/webcodecs/codec_registry.html>
	#[serde_as(as = "DisplayFromStr")]
	pub codec: AudioCodec,

	/// The sample rate of the audio in Hz.
	pub sample_rate: u32,

	/// The number of channels in the audio.
	#[serde(rename = "numberOfChannels")]
	pub channel_count: u32,

	/// What the decoded channels carry, or the codec's own channel order when absent.
	///
	/// Live state, like video `color`.
	#[serde(default)]
	#[serde_as(as = "Option<DisplayFromStr>")]
	pub layout: Option<AudioLayout>,

	/// The maximum bitrate of the audio track in bits per second.
	#[serde(default)]
	pub bitrate: Option<u64>,

	/// Whether this rendition may be selected. When false, no frames are coming and a consumer
	/// should not select it, though one may keep it selected while no enabled alternative exists.
	/// Only written when false.
	#[serde(
		default = "crate::catalog::enabled_default",
		skip_serializing_if = "crate::catalog::enabled_skip"
	)]
	pub enabled: bool,

	/// Some codecs include a description so the decoder can be initialized without extra data.
	/// If not provided, there may be in-band metadata (marginally higher overhead).
	#[serde(default)]
	#[serde_as(as = "Option<Hex>")]
	pub description: Option<Bytes>,

	/// Container format for frame encoding.
	/// Defaults to "legacy" for backward compatibility.
	#[serde(default)]
	pub container: Container,

	/// The maximum delay between a frame being ready and the publisher flushing it.
	/// The player's jitter buffer should be larger than this value.
	/// If not provided, the player should assume each frame is flushed immediately.
	///
	/// This is measured at the publisher (encoder latency, packet packing, reordering),
	/// never on the network a consumer sees. It only ever grows over the life of a stream.
	///
	/// Serialized as a whole number of milliseconds, rounded up, so an upper bound never
	/// rounds down into a promise the publisher can't keep.
	///
	/// NOTE: The audio "frame" duration depends on the codec, sample rate, etc.
	/// ex: AAC often uses 1024 samples per frame, so at 44100Hz, this would be 1024/44100 = 24ms
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub jitter: Option<std::time::Duration>,

	/// After a non-continuous join, decode from the group start, present at start plus warmup, and join that much earlier.
	/// Serialized as whole milliseconds, rounded up so presentation never starts early.
	#[serde_as(as = "Option<MillisCeil>")]
	#[serde(default)]
	pub warmup: Option<std::time::Duration>,

	/// How far this rendition's frames reach the transport behind the broadcast's earliest
	/// rendition, measured at the publisher from each rendition's minimum flush lateness.
	/// Absent on the earliest rendition and on any rendition the publisher did not measure.
	///
	/// A consumer holds `delay + jitter` for this rendition and MUST NOT subtract one rendition's
	/// `delay` from another's: each is a lifetime maximum, so two need not share an origin. It only
	/// ever grows over the life of a stream, and is serialized like [`jitter`](Self::jitter).
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub delay: Option<std::time::Duration>,
	/// This rendition's HLS segment records, when the publisher opted in. Without it the rendition
	/// is not HLS-renderable. An undecodable value is absent, so it can never reject the media.
	#[serde(default)]
	#[serde_as(as = "serde_with::DefaultOnError<_>")]
	pub hls: Option<crate::catalog::Hls>,
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
			broadcast: None,
			track: None,
			epoch: None,
			label: None,
			codec: codec.into(),
			sample_rate,
			channel_count,
			layout: None,
			bitrate: None,
			enabled: true,
			description: None,
			container: Container::default(),
			jitter: None,
			warmup: None,
			delay: None,
			hls: None,
		}
	}

	/// Whether `other` keeps this config's track identity: the fields a consumer cannot change
	/// without reinitializing its decoder or demuxer (`codec`, `description`, `container`,
	/// `sample_rate`, and `channel_count`).
	///
	/// A track name always means the same content, so a publisher that changes any of them
	/// publishes under a new name; every other field may update in place. See the draft's
	/// [Track Identity](https://github.com/moq-dev/moq/blob/main/drafts/draft-lcurley-moq-hang.md#identity).
	pub fn same_identity(&self, other: &Self) -> bool {
		self.codec == other.codec
			&& self.description == other.description
			&& self.container == other.container
			&& self.sample_rate == other.sample_rate
			&& self.channel_count == other.channel_count
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn warmup_round_trips() {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		assert!(serde_json::to_value(&config).unwrap().get("warmup").is_none());
		for millis in [0, 80, 1_000] {
			config.warmup = Some(std::time::Duration::from_millis(millis));
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["warmup"], millis);
			let decoded: AudioConfig = serde_json::from_value(encoded).unwrap();
			assert_eq!(decoded.warmup, config.warmup);
		}
	}

	#[test]
	fn warmup_rounds_up() {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		for (nanos, millis) in [(1, 1), (499_999, 1), (333_200_000, 334)] {
			config.warmup = Some(std::time::Duration::from_nanos(nanos));
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["warmup"], millis);
			let decoded: AudioConfig = serde_json::from_value(encoded).unwrap();
			assert_eq!(decoded.warmup, Some(std::time::Duration::from_millis(millis)));
		}
	}

	#[test]
	fn ranked_orders_by_enabled_then_bitrate_then_rate_then_channels() {
		fn rendition(sample_rate: u32, channels: u32, bitrate: Option<u64>) -> AudioConfig {
			let mut config = AudioConfig::new(AudioCodec::Opus, sample_rate, channels);
			config.bitrate = bitrate;
			config
		}

		let mut audio = Audio::default();
		// Names sort the weaker renditions first, so name order alone would pick the wrong one.
		audio.insert("a", rendition(48_000, 2, None)).unwrap();
		audio.insert("b", rendition(48_000, 1, Some(64_000))).unwrap();
		audio.insert("c", rendition(16_000, 1, Some(128_000))).unwrap();
		audio.insert("d", rendition(48_000, 2, Some(64_000))).unwrap();
		audio.insert("e", rendition(48_000, 2, Some(64_000))).unwrap();
		audio.insert("f", rendition(44_100, 2, Some(64_000))).unwrap();
		audio.insert("g", rendition(16_000, 1, None)).unwrap();

		let names: Vec<_> = audio.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["c", "d", "e", "b", "f", "a", "g"]);

		// Disabled renditions rank last, so catalog transforms can retain their metadata.
		audio.renditions.get_mut("c").unwrap().enabled = false;
		let names: Vec<_> = audio.ranked().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["d", "e", "b", "f", "a", "g", "c"]);
		let names: Vec<_> = audio.enabled().map(|(name, _)| name.as_str()).collect();
		assert_eq!(names, ["d", "e", "b", "f", "a", "g"]);
	}

	#[test]
	fn disabled_renditions_are_ineligible_until_reenabled() {
		let mut catalog = Audio::default();
		let config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		catalog.renditions.insert("track".into(), config);
		assert_eq!(catalog.enabled().count(), 1);
		catalog.renditions.get_mut("track").unwrap().enabled = false;
		assert_eq!(catalog.enabled().count(), 0);
		assert_eq!(catalog.ranked().count(), 1);
		catalog.renditions.get_mut("track").unwrap().enabled = true;
		assert_eq!(catalog.enabled().count(), 1);
	}

	#[test]
	fn enabled_is_written_only_when_false() {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		assert!(serde_json::to_value(&config).unwrap().get("enabled").is_none());

		config.enabled = false;
		let encoded = serde_json::to_value(&config).expect("failed to encode");
		assert_eq!(encoded["enabled"], false);
		let decoded: AudioConfig = serde_json::from_value(encoded).expect("failed to decode");
		assert!(!decoded.enabled);
	}
}

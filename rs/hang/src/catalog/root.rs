//! This module contains the structs and functions for the MoQ catalog format
use crate::Result;
use crate::catalog::{Audio, Binary, Json, PRIORITY, Text, Video};
use serde::de::DeserializeOwned;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A catalog track, created by a broadcaster to describe the tracks available in a broadcast.
///
/// The base catalog carries the media sections (`video`, `audio`, `text`), the optional `clock`
/// (the broadcast's one wall-clock mapping), the data sections
/// (`json`, `binary`) for application tracks that aren't media, and the optional `m2ts` (a
/// transport stream carried whole), plus `stats` naming the publisher's media snapshot track and
/// `echo` soliciting viewer feedback under a relative broadcast prefix.
/// Applications extend it with their own root sections (e.g. `scte35`) through `E`. The catalog
/// does not deny unknown fields, so a base consumer ignores the extra sections and an extended
/// catalog stays wire-compatible. See the `extension_roundtrip` test.
///
/// Marked `#[non_exhaustive]` so a future base section can be added without bumping the major
/// version. External callers start from [`Catalog::default`] and fill in the sections they
/// publish; struct-literal construction (with or without `..base`) is not available outside
/// this crate.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Catalog<E = ()> {
	/// Video track information with multiple renditions.
	///
	/// Contains a map of video track renditions that the viewer can choose from
	/// based on their preferences (resolution, bitrate, codec, etc). Omitted from the
	/// wire when it has no renditions.
	pub video: Video,

	/// Audio track information with multiple renditions.
	///
	/// Contains a map of audio track renditions that the viewer can choose from
	/// based on their preferences (codec, bitrate, language, etc). Omitted from the
	/// wire when it has no renditions.
	pub audio: Audio,

	/// The broadcast's one continuous clock, if the publisher exposes one.
	///
	/// `wall` is the wall-clock time of PTS zero in `timescale` units since the moq epoch
	/// (2020-01-01); every media track and every HLS record refer to this mapping after
	/// timescale conversion. See [`Clock`](crate::catalog::Clock).
	pub clock: Option<crate::catalog::Clock>,

	/// Text (caption/subtitle) track information with multiple renditions.
	///
	/// Contains a map of text track renditions that the viewer can choose from
	/// based on their preferences (language, role). Omitted from the wire when empty, so a
	/// broadcast without captions stays byte-identical to before this section existed.
	///
	/// A `text` value that isn't a caption section decodes as empty rather than failing the
	/// catalog, since applications could carry their own `text` key before this was reserved.
	pub text: Text,

	/// JSON tracks: application data published as live JSON documents or logs.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged.
	///
	/// A `json` value that isn't a data-track section decodes as empty rather than failing the
	/// catalog, the same as [`text`](Self::text): `json` is a generic enough key that an
	/// application could have been carrying its own before this one was reserved, and losing the
	/// tracks we can't read beats losing the whole catalog.
	pub json: Json,

	/// Binary tracks: application data published as opaque payloads.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged. Decoded leniently
	/// for the same reason as [`json`](Self::json).
	pub binary: Binary,

	/// A transport stream carried whole rather than demultiplexed: the one track holding its
	/// packets, verbatim. See [`M2ts`](crate::catalog::M2ts). Omitted from the wire when absent.
	pub m2ts: Option<crate::catalog::M2ts>,

	/// The publisher's optional media snapshot track.
	///
	/// A `stats` value without a string `track` decodes as absent rather than failing the
	/// catalog, for the same reason as [`json`](Self::json).
	pub stats: Option<crate::catalog::Stats>,

	/// The optional prefix soliciting viewer feedback broadcasts.
	///
	/// An `echo` value without a string `path` decodes as absent, the same as [`stats`](Self::stats).
	pub echo: Option<crate::catalog::Echo>,

	/// Application-defined root sections, flattened alongside the base catalog sections.
	///
	/// The root `version` is reserved: serializing computes it from the base sections, and fails if
	/// the extension names a member `version`.
	pub ext: E,
}

/// The newest moq-hang draft this crate reads; a catalog whose `version` is higher is refused.
const VERSION: u64 = 4;

/// The wire shape of [`Catalog`]: its sections plus the root `version`, the oldest moq-hang draft
/// that reads the catalog correctly. An absent `version` means 3, the draft before the field.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize)]
// Field defaults rather than a container one: a remote container default would be a `Catalog`.
#[serde(remote = "Catalog", rename_all = "camelCase")]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned + Default"))]
struct Wire<E> {
	// Computed rather than stored, so a republished catalog never claims less than it uses.
	#[serde(default, getter = "Catalog::min_version", deserialize_with = "deserialize_version")]
	version: Option<u64>,

	#[serde(default, skip_serializing_if = "Video::is_empty")]
	video: Video,

	#[serde(default, skip_serializing_if = "Audio::is_empty")]
	audio: Audio,

	#[serde(default)]
	clock: Option<crate::catalog::Clock>,

	#[serde(
		default,
		skip_serializing_if = "Text::is_empty",
		deserialize_with = "crate::catalog::deserialize_section"
	)]
	text: Text,

	#[serde(
		default,
		skip_serializing_if = "Json::is_empty",
		deserialize_with = "crate::catalog::deserialize_section"
	)]
	json: Json,

	#[serde(
		default,
		skip_serializing_if = "Binary::is_empty",
		deserialize_with = "crate::catalog::deserialize_section"
	)]
	binary: Binary,

	#[serde(default)]
	m2ts: Option<crate::catalog::M2ts>,

	#[serde(default, deserialize_with = "crate::catalog::deserialize_stats")]
	stats: Option<crate::catalog::Stats>,

	#[serde(default, deserialize_with = "crate::catalog::deserialize_echo")]
	echo: Option<crate::catalog::Echo>,

	#[serde(flatten, serialize_with = "serialize_ext")]
	ext: E,
}

/// Refuse an extension member named `version`: flattened beside the computed one, it would emit a
/// duplicate key that a reader resolves to whichever comes last.
fn serialize_ext<E: Serialize, S: serde::Serializer>(ext: &E, serializer: S) -> std::result::Result<S::Ok, S::Error> {
	use serde::ser::Error;

	// Inspected as a `Value` but written from `ext` itself, which keeps its member order.
	if serde_json::to_value(ext)
		.map_err(S::Error::custom)?
		.get("version")
		.is_some()
	{
		return Err(S::Error::custom("catalog extension must not name a member `version`"));
	}
	ext.serialize(serializer)
}

// Exhaustive on both sides, so a `Catalog` field missing from `Wire` fails to compile.
impl<E> From<Wire<E>> for Catalog<E> {
	fn from(wire: Wire<E>) -> Self {
		let Wire {
			version: _,
			video,
			audio,
			clock,
			text,
			json,
			binary,
			m2ts,
			stats,
			echo,
			ext,
		} = wire;
		Self {
			video,
			audio,
			clock,
			text,
			json,
			binary,
			m2ts,
			stats,
			echo,
			ext,
		}
	}
}

impl<E: Serialize> Serialize for Catalog<E> {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
		Wire::serialize(self, serializer)
	}
}

impl<'de, E: DeserializeOwned + Default> Deserialize<'de> for Catalog<E> {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
		Wire::deserialize(deserializer)
	}
}

/// Refuse a `version` above [`VERSION`]: the newer draft changed something this reader would
/// misread, not merely added something it would miss.
fn deserialize_version<'de, D: serde::Deserializer<'de>>(
	deserializer: D,
) -> std::result::Result<Option<u64>, D::Error> {
	let version = Some(u64::deserialize(deserializer)?);
	check_version(version).map_err(serde::de::Error::custom)?;
	Ok(version)
}

impl Catalog<()> {
	/// The default name for the catalog track.
	pub const DEFAULT_NAME: &str = "catalog.json";

	/// Track properties for creating the catalog track via
	/// [`create_track`](moq_net::broadcast::Producer::create_track) at
	/// [`DEFAULT_NAME`](Self::DEFAULT_NAME).
	///
	/// Keeps the bare `moq_net` retention rather than the media one: the catalog is
	/// snapshot mode, so the useful value is the live edge, which is always kept.
	/// Timed in milliseconds, so snapshots share the broadcast clock with media, and
	/// [`compress`](moq_net::track::Info::compress)ed, since catalog JSON is small and repetitive.
	pub fn default_track_info() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_priority(PRIORITY.catalog)
			.with_compress(true)
	}

	/// The subscription preferences used for the catalog track (high priority so
	/// it preempts media tracks).
	pub fn default_subscription() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default().with_priority(PRIORITY.catalog)
	}
}

impl<E> Catalog<E> {
	/// Refuse a catalog with an ID naming both a video and an audio rendition, or with a
	/// projection or audio layout out of range.
	pub fn check_renditions(&self) -> Result<()> {
		if let Some(id) = self
			.video
			.renditions
			.keys()
			.find(|id| self.audio.renditions.contains_key(*id))
		{
			return Err(crate::Error::DuplicateRendition(id.clone()));
		}
		for config in self.video.renditions.values() {
			if let Some(projection) = &config.projection {
				projection.check()?;
			}
		}
		for config in self.audio.renditions.values() {
			if let Some(layout) = &config.layout {
				layout.check(config.channel_count)?;
			}
		}
		Ok(())
	}

	/// The oldest moq-hang draft that reads this catalog correctly, or `None` while that is 3.
	///
	/// Only a field an older reader would misread raises it; one it would merely ignore does not.
	fn min_version(&self) -> Option<u64> {
		use crate::catalog::Reference;

		fn renamed<'a, C: Reference + 'a>(entries: impl IntoIterator<Item = (&'a String, &'a C)>) -> bool {
			entries.into_iter().any(|(id, config)| config.track(id) != id)
		}

		// moq-hang-04: a reader that ignores `track` subscribes to the ID, a different track.
		let renamed = renamed(&self.video.renditions)
			|| renamed(&self.audio.renditions)
			|| renamed(&self.text.renditions)
			|| renamed(&self.json.tracks)
			|| renamed(&self.binary.tracks);

		// moq-hang-04: a reader that ignores video `warmup` assumes every group starts at a
		// keyframe, and hands its decoder one that starts at a converging picture instead.
		let warmup = (self.video.renditions.values()).any(|config| config.warmup.is_some_and(|w| !w.is_zero()));

		(renamed || warmup).then_some(4)
	}

	/// Copy the media sections into a catalog with no application extension.
	pub fn media(&self) -> Catalog<()> {
		Catalog {
			video: self.video.clone(),
			audio: self.audio.clone(),
			clock: self.clock,
			text: self.text.clone(),
			..Default::default()
		}
	}
}

impl<E: DeserializeOwned + Default> FromStr for Catalog<E> {
	type Err = crate::Error;

	/// Parse a catalog from a string.
	///
	/// Fails with [`Error::UnsupportedVersion`](crate::Error::UnsupportedVersion) if the catalog
	/// needs a newer moq-hang draft than this crate reads.
	fn from_str(s: &str) -> Result<Self> {
		Self::from_slice(s.as_bytes())
	}
}

impl<E: DeserializeOwned + Default> Catalog<E> {
	/// Subscribe to catalog updates on a broadcast.
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer) -> Result<super::Consumer<E>> {
		let track = broadcast
			.track(Catalog::DEFAULT_NAME)?
			.subscribe(Catalog::default_subscription())
			.await?;
		Ok(super::Consumer::new(track))
	}

	/// Parse a catalog from a slice of bytes.
	///
	/// Fails with [`Error::UnsupportedVersion`](crate::Error::UnsupportedVersion) if the catalog
	/// needs a newer moq-hang draft than this crate reads.
	pub fn from_slice(v: &[u8]) -> Result<Self> {
		// Decoded straight from the bytes rather than via a `Value`, which would keep only the
		// last of two `version` members instead of refusing them.
		check_version(serde_json::from_slice::<Probe>(v)?.version)?;
		Ok(serde_json::from_slice(v)?)
	}

	/// Parse a catalog from a reader.
	///
	/// Fails with [`Error::UnsupportedVersion`](crate::Error::UnsupportedVersion) if the catalog
	/// needs a newer moq-hang draft than this crate reads.
	pub fn from_reader(mut reader: impl std::io::Read) -> Result<Self> {
		let mut buf = Vec::new();
		reader.read_to_end(&mut buf).map_err(serde_json::Error::io)?;
		Self::from_slice(&buf)
	}

	/// Parse a catalog from a JSON value.
	///
	/// Fails with [`Error::UnsupportedVersion`](crate::Error::UnsupportedVersion) if the catalog
	/// needs a newer moq-hang draft than this crate reads.
	pub(crate) fn from_value(value: serde_json::Value) -> Result<Self> {
		check_version(value.get("version").and_then(serde_json::Value::as_u64))?;
		Ok(serde_json::from_value(value)?)
	}
}

/// Just the root `version`, read before any section since the newer draft may have reshaped one.
#[derive(Deserialize)]
struct Probe {
	#[serde(default)]
	version: Option<u64>,
}

fn check_version(version: Option<u64>) -> Result<()> {
	match version {
		Some(version) if version > VERSION => Err(crate::Error::UnsupportedVersion(version)),
		_ => Ok(()),
	}
}

impl<E: Serialize> Catalog<E> {
	/// Serialize the catalog to a JSON string.
	pub fn to_json(&self) -> Result<String> {
		Ok(serde_json::to_string(self)?)
	}

	/// Serialize the catalog to a pretty-printed JSON string.
	pub fn to_json_pretty(&self) -> Result<String> {
		Ok(serde_json::to_string_pretty(self)?)
	}

	/// Serialize the catalog to a vector of bytes.
	pub fn to_vec(&self) -> Result<Vec<u8>> {
		Ok(serde_json::to_vec(self)?)
	}

	/// Serialize the catalog to a writer.
	pub fn to_writer(&self, writer: impl std::io::Write) -> Result<()> {
		Ok(serde_json::to_writer(writer, self)?)
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use crate::catalog::{AudioCodec::Opus, AudioConfig, BinaryConfig, Container, H264, JsonConfig, Mode, VideoConfig};

	use super::*;

	/// The rendition count is the publisher's to choose: a catalog listing many is accepted.
	#[test]
	fn many_renditions_are_accepted() {
		let mut catalog = Catalog::<()>::default();
		for i in 0..1000 {
			catalog
				.audio
				.renditions
				.insert(format!("audio{i}"), AudioConfig::new(Opus, 48_000, 2));
		}
		catalog.check_renditions().unwrap();
		let decoded = Catalog::<()>::from_str(&catalog.to_json().unwrap()).unwrap();
		assert_eq!(decoded.audio.renditions.len(), 1000);
	}

	#[test]
	fn track_name_defaults_to_the_id() {
		use crate::catalog::Reference;

		let catalog = Catalog::<()>::from_str(
			r#"{
				"video": {"renditions": {"hd": {"codec": "avc1.64001f", "container": {"kind": "legacy"}}}},
				"audio": {"renditions": {"en": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {"kind": "legacy"}}}}
			}"#,
		)
		.unwrap();
		assert_eq!(catalog.video.renditions["hd"].track("hd"), "hd");
		assert_eq!(catalog.audio.renditions["en"].track("en"), "en");
		assert!(!catalog.to_json().unwrap().contains("\"track\""));
	}

	#[test]
	fn one_track_name_in_two_broadcasts() {
		use crate::catalog::Reference;

		let encoded = r#"{
			"video": {"renditions": {
				"left": {"broadcast": "./left", "track": "video", "codec": "avc1.64001f", "container": {"kind": "legacy"}},
				"right": {"broadcast": "./right", "track": "video", "codec": "avc1.64001f", "container": {"kind": "legacy"}}
			}}
		}"#;
		let catalog = Catalog::<()>::from_str(encoded).unwrap();
		assert!(catalog.check_renditions().is_ok());
		for id in ["left", "right"] {
			let config = &catalog.video.renditions[id];
			assert_eq!(config.track(id), "video");
			assert_eq!(config.broadcast().map(|b| b.as_str()), Some(id));
		}
		assert_eq!(Catalog::<()>::from_str(&catalog.to_json().unwrap()).unwrap(), catalog);
	}

	/// Every reference-bearing section carries an optional epoch beside its `broadcast`.
	#[test]
	fn reference_epochs_roundtrip_in_every_section() {
		use crate::catalog::Reference;

		const EPOCH: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
		let encoded = format!(
			r#"{{
				"video": {{"renditions": {{"v": {{"broadcast": "./a", "epoch": "{EPOCH}", "codec": "avc1.64001f", "container": {{"kind": "legacy"}}}}}}}},
				"audio": {{"renditions": {{"a": {{"broadcast": "./a", "epoch": "{EPOCH}", "codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {{"kind": "legacy"}}}}}}}},
				"text": {{"renditions": {{"t": {{"broadcast": "./a", "epoch": "{EPOCH}", "format": "vtt"}}}}}},
				"json": {{"tracks": {{"j": {{"broadcast": "./a", "epoch": "{EPOCH}", "mode": "snapshot"}}}}}},
				"binary": {{"tracks": {{"b": {{"broadcast": "./a", "epoch": "{EPOCH}", "mode": "snapshot"}}}}}}
			}}"#
		);
		let catalog = Catalog::<()>::from_str(&encoded).unwrap();
		let epoch: moq_net::Epoch = EPOCH.parse().unwrap();
		let references: [&dyn Reference; 5] = [
			&catalog.video.renditions["v"],
			&catalog.audio.renditions["a"],
			&catalog.text.renditions["t"],
			&catalog.json.tracks["j"],
			&catalog.binary.tracks["b"],
		];
		for reference in references {
			assert_eq!(reference.epoch(), Some(&epoch));
		}
		assert_eq!(Catalog::<()>::from_str(&catalog.to_json().unwrap()).unwrap(), catalog);

		// Omitted stays omitted, and a malformed or noncanonical epoch refuses the catalog.
		assert!(!Catalog::<()>::default().to_json().unwrap().contains("poch"));
		for bad in [
			"not-a-uuid",
			"01890A5D-AC96-774B-BCCE-B302099A8057",
			"01890a5d-ac96-474b-bcce-b302099a8057",
		] {
			let encoded = encoded.replace(EPOCH, bad);
			assert!(Catalog::<()>::from_str(&encoded).is_err(), "{bad} must be refused");
		}
	}

	#[test]
	fn cross_kind_duplicate_id_is_refused() {
		let catalog = Catalog::<()>::from_str(
			r#"{
				"video": {"renditions": {"main": {"codec": "avc1.64001f", "container": {"kind": "legacy"}}}},
				"audio": {"renditions": {"main": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {"kind": "legacy"}}}}
			}"#,
		)
		.unwrap();
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::DuplicateRendition(id)) if id == "main"
		));
	}

	#[test]
	fn spatial_fields_out_of_range_are_refused() {
		use crate::catalog::{AudioLayout, Projection, VideoCodec, VideoConfig};

		let mut catalog = Catalog::<()>::from_str(
			r#"{"audio": {"renditions": {"amb": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 6, "layout": "ambisonic", "container": {"kind": "legacy"}}}}}"#,
		)
		.unwrap();
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::InvalidAmbisonicChannels(6))
		));
		catalog.audio.renditions.get_mut("amb").unwrap().channel_count = 4;
		assert_eq!(catalog.audio.renditions["amb"].layout, Some(AudioLayout::Ambisonic));
		catalog.check_renditions().unwrap();

		// A publisher building the catalog by hand is caught before it writes the bad value.
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.projection = Some(Projection::Equirectangular { fov: Some(400.0) });
		catalog.video.insert("vr", config).unwrap();
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::InvalidFieldOfView(_))
		));
	}

	#[test]
	fn simple() {
		let mut encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"codec": "avc1.64001f",
						"codedWidth": 1280,
						"codedHeight": 720,
						"bitrate": 6000000,
						"framerate": 30.0,
						"container": {"kind": "legacy"}
					}
				}
			},
			"audio": {
				"renditions": {
					"audio": {
						"codec": "opus",
						"sampleRate": 48000,
						"numberOfChannels": 2,
						"bitrate": 128000,
						"container": {"kind": "legacy"}
					}
				}
			}
		}"#
		.to_string();

		encoded.retain(|c| !c.is_whitespace());

		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		video_config.coded_width = Some(1280);
		video_config.coded_height = Some(720);
		video_config.bitrate = Some(6_000_000);
		video_config.framerate = Some(30.0);
		video_config.container = Container::Legacy;

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert("video".to_string(), video_config);

		let mut audio_config = AudioConfig::new(Opus, 48_000, 2);
		audio_config.bitrate = Some(128_000);
		audio_config.container = Container::Legacy;

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert("audio".to_string(), audio_config);

		let mut decoded = Catalog::<()>::default();
		decoded.video.renditions = video_renditions;
		decoded.audio.renditions = audio_renditions;

		let output = Catalog::<()>::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, output, "wrong decoded output");

		let output = decoded.to_json().expect("failed to encode");
		assert_eq!(encoded, output, "wrong encoded output");
	}

	/// Lock in the on-wire shape of the jitter and delay fields: a bare integer number
	/// of milliseconds. If `Option<Duration>` ever loses the `duration_millis`
	/// serde adapter, this regresses to serde's default `{secs, nanos}` shape.
	#[test]
	fn jitter_and_delay_serialized_as_millis() {
		let mut encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"codec": "avc1.64001f",
						"container": {"kind": "legacy"},
						"jitter": 100,
						"delay": 200
					}
				}
			},
			"audio": {
				"renditions": {
					"audio": {
						"codec": "opus",
						"sampleRate": 48000,
						"numberOfChannels": 2,
						"container": {"kind": "legacy"},
						"jitter": 40
					}
				}
			}
		}"#
		.to_string();
		encoded.retain(|c| !c.is_whitespace());

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video".to_string(),
			VideoConfig {
				broadcast: None,
				track: None,
				epoch: None,
				label: None,
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: false,
				}
				.into(),
				description: None,
				coded_width: None,
				coded_height: None,
				display_aspect_width: None,
				display_aspect_height: None,
				color: None,
				projection: None,
				stereo: None,
				interlaced: false,
				bitrate: None,
				enabled: true,
				preference: 0,
				framerate: None,
				reorder: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				warmup: None,
				jitter: Some(std::time::Duration::from_millis(100)),
				delay: Some(std::time::Duration::from_millis(200)),
				hls: None,
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio".to_string(),
			AudioConfig {
				broadcast: None,
				track: None,
				epoch: None,
				label: None,
				codec: Opus,
				sample_rate: 48_000,
				channel_count: 2,
				layout: None,
				bitrate: None,
				enabled: true,
				description: None,
				container: Container::Legacy,
				warmup: None,
				jitter: Some(std::time::Duration::from_millis(40)),
				delay: None,
				hls: None,
			},
		);

		let mut catalog = Catalog::<()>::default();
		catalog.video.renditions = video_renditions;
		catalog.audio.renditions = audio_renditions;

		let decoded = Catalog::<()>::from_str(&encoded).expect("failed to decode");
		assert_eq!(catalog, decoded, "decode mismatch");

		let output = catalog.to_json().expect("failed to encode");
		assert_eq!(encoded, output, "encode mismatch");
	}

	#[test]
	fn rendition_with_broadcast_override() {
		// Decode a catalog where one rendition references a track in a sibling broadcast,
		// and verify the `broadcast` field round-trips through serde.
		let encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"broadcast": "./source",
						"codec": "avc1.64001f",
						"codedWidth": 1280,
						"codedHeight": 720,
						"container": {"kind": "legacy"}
					}
				}
			}
		}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");
		let rendition = parsed.video.renditions.get("video").expect("missing rendition");
		assert_eq!(
			rendition.broadcast.as_ref().map(|p| p.as_str()),
			Some("source"),
			"broadcast field did not deserialize"
		);

		// Full encode -> decode -> equality, so the test catches any encoder regression
		// (e.g. wrong key, double-emission, or `null` instead of skip).
		let output = parsed.to_json().expect("failed to encode");
		let reparsed = Catalog::<()>::from_str(&output).expect("failed to re-decode");
		assert_eq!(parsed, reparsed, "re-encoded catalog did not round-trip");
	}

	#[test]
	fn rendition_without_broadcast_omits_field() {
		// `broadcast: None` must NOT serialize as `"broadcast":null`, otherwise the wire
		// format silently changes for every catalog that doesn't use cross-broadcast refs.
		let mut video_config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		video_config.container = Container::Legacy;

		let mut renditions = BTreeMap::new();
		renditions.insert("video".to_string(), video_config);

		let catalog = Catalog::<()> {
			video: Video {
				renditions,
				..Default::default()
			},
			..Default::default()
		};

		let output = catalog.to_json().expect("failed to encode");
		assert!(
			!output.contains("broadcast"),
			"broadcast field leaked into JSON when None: {output}"
		);
	}

	#[test]
	fn rendition_with_empty_broadcast_normalizes() {
		// An empty-string broadcast field should normalize to an empty `path::Relative` so the
		// consumer can treat it identically to a missing field.
		let encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"broadcast": "",
						"codec": "avc1.64001f",
						"container": {"kind": "legacy"}
					}
				}
			}
		}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");
		let rendition = parsed.video.renditions.get("video").expect("missing rendition");
		assert_eq!(
			rendition.broadcast.as_ref().map(|p| p.is_empty()),
			Some(true),
			"empty broadcast should deserialize as Some(empty)"
		);
	}

	#[test]
	fn rendition_with_parent_broadcast_stays_distinct_from_empty() {
		let encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"broadcast": ".",
						"codec": "avc1.64001f",
						"container": {"kind": "legacy"}
					}
				}
			}
		}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");
		let rendition = parsed.video.renditions.get("video").expect("missing rendition");
		assert_eq!(
			rendition.broadcast.as_ref().map(|p| p.as_str()),
			Some("."),
			"parent reference should not normalize to empty"
		);
	}

	#[test]
	fn unknown_container_keeps_siblings() {
		// A rendition using a future container must not take down the rest of the catalog.
		let encoded = r#"{
			"video": {
				"renditions": {
					"future": {
						"codec": "avc1.64001f",
						"container": {"kind": "future", "magic": 7}
					},
					"legacy": {
						"codec": "avc1.64001f",
						"codedWidth": 1280,
						"codedHeight": 720,
						"container": {"kind": "legacy"}
					}
				}
			}
		}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");

		let known = parsed.video.renditions.get("legacy").expect("missing rendition");
		assert_eq!(known.container, Container::Legacy);
		assert_eq!(known.coded_width, Some(1280));

		let future = parsed.video.renditions.get("future").expect("missing rendition");
		let Container::Unknown(unknown) = &future.container else {
			panic!("expected unknown container: {:?}", future.container);
		};
		assert_eq!(unknown.kind(), Some("future"));

		// The unknown rendition survives a republish intact.
		let output = parsed.to_json().expect("failed to encode");
		let reparsed = Catalog::<()>::from_str(&output).expect("failed to re-decode");
		assert_eq!(parsed, reparsed, "re-encoded catalog did not round-trip");
		assert!(output.contains(r#""magic":7"#), "unknown fields dropped: {output}");
	}

	#[test]
	fn empty_video_and_audio_sections_omitted() {
		// A data-only broadcast must not advertise media it does not have. An old publisher's
		// empty objects still decode, and a republish drops them.
		assert_eq!(Catalog::<()>::default().to_json().expect("failed to encode"), "{}");

		let mut audio_only = Catalog::<()>::default();
		audio_only
			.audio
			.insert("audio", AudioConfig::new(Opus, 48_000, 2))
			.unwrap();
		let output = audio_only.to_json().expect("failed to encode");
		assert!(output.contains("\"audio\""), "audio section missing: {output}");
		assert!(!output.contains("\"video\""), "empty video section leaked: {output}");

		let mut video_only = Catalog::<()>::default();
		video_only.video.display = Some(crate::catalog::Display { width: 16, height: 9 });
		assert_eq!(
			video_only.to_json().expect("failed to encode"),
			"{}",
			"display without a rendition must not keep an empty video section"
		);

		let legacy = Catalog::<()>::from_str(r#"{"video":{"renditions":{}},"audio":{"renditions":{}}}"#)
			.expect("failed to decode");
		assert!(legacy.video.is_empty());
		assert!(legacy.audio.is_empty());
		assert_eq!(legacy.to_json().expect("failed to encode"), "{}");
	}

	#[test]
	fn text_section_roundtrip() {
		use crate::catalog::{Text, TextConfig, TextFormat, TextRole};

		let mut config = TextConfig::new(TextFormat::Vtt);
		config.role = TextRole::Caption;
		config.lang = Some("en".to_string());

		let mut text = Text::default();
		text.insert("captions.en", config).expect("insert");

		let catalog = Catalog::<()> {
			text,
			..Default::default()
		};

		let json = catalog.to_json().expect("failed to encode");
		assert!(json.contains("\"text\""), "text section missing: {json}");

		let decoded = Catalog::<()>::from_str(&json).expect("failed to decode");
		assert_eq!(catalog, decoded, "text section did not round-trip");
	}

	#[test]
	fn legacy_text_section_keeps_the_catalog() {
		// `text` was an ordinary application section before captions reserved it, so a value with
		// the wrong shape must cost its captions and nothing else. Audio and video keep playing.
		for legacy in [
			r#""a caption overlay""#,
			r#"["a","b"]"#,
			r#"{"overlay":{"x":1}}"#,
			r#"{"renditions":42}"#,
		] {
			let json = format!(r#"{{"video":{{"renditions":{{}}}},"audio":{{"renditions":{{}}}},"text":{legacy}}}"#);
			let catalog =
				Catalog::<()>::from_str(&json).unwrap_or_else(|e| panic!("legacy text {legacy} broke it: {e}"));
			assert!(catalog.text.is_empty(), "legacy text {legacy} decoded as captions");
		}
	}

	/// The counterpart to the above: the fallback covers someone else's key, not our own bugs. A
	/// value that *is* a text section still has to decode, or a rendition with no format would
	/// silently cost a publisher every caption it advertised.
	#[test]
	fn a_malformed_text_section_still_fails() {
		let json = r#"{"text":{"renditions":{"captions":{"container":{"kind":"legacy"}}}}}"#;
		assert!(
			Catalog::<()>::from_str(json).is_err(),
			"a format-less text rendition decoded instead of failing"
		);
	}

	#[test]
	fn unknown_text_role_keeps_the_catalog() {
		// A future `role` value must not take down the whole catalog: audio and video have to keep
		// playing even when a caption rendition is classified with a vocabulary we don't know yet.
		let json = r#"{"video":{"renditions":{}},"audio":{"renditions":{}},"text":{"renditions":{"subs":{"format":"vtt","role":"commentary"}}}}"#;

		let catalog = Catalog::<()>::from_str(json).expect("unknown role rejected the catalog");
		assert_eq!(catalog.text.renditions.len(), 1);
		let encoded = catalog.to_json().expect("failed to encode unknown role");
		assert!(
			encoded.contains(r#""role":"commentary""#),
			"unknown role was not preserved: {encoded}"
		);
		assert!(!encoded.contains("\"video\""), "empty video section leaked: {encoded}");
		assert!(!encoded.contains("\"audio\""), "empty audio section leaked: {encoded}");
	}

	/// A section name is only reserved from the version that defines it, and `json` and `binary` are
	/// generic enough that an application could already be using one. Dropping the section we can't
	/// read keeps video and audio playable, instead of failing the whole catalog over that key.
	#[test]
	fn a_foreign_data_section_does_not_fail_the_catalog() {
		for section in ["json", "binary"] {
			let wire = format!(r#"{{"video":{{"renditions":{{}}}},"{section}":{{"messages":"chat"}}}}"#);
			let catalog =
				Catalog::<()>::from_str(&wire).unwrap_or_else(|err| panic!("{section} took the catalog down: {err}"));
			assert!(catalog.json.is_empty(), "{section}");
			assert!(catalog.binary.is_empty(), "{section}");
		}
	}

	/// The counterpart to the above: the fallback covers someone else's key, not our own bugs. A
	/// value that *is* a data section still has to decode, or a mode-less track would silently cost
	/// a publisher every data track it advertised.
	#[test]
	fn a_malformed_data_section_still_fails() {
		for section in ["json", "binary"] {
			let wire = format!(r#"{{"{section}":{{"tracks":{{"chat":{{"schema":"chat"}}}}}}}}"#);
			assert!(
				Catalog::<()>::from_str(&wire).is_err(),
				"a mode-less {section} track decoded instead of failing"
			);
		}
	}

	/// `stats` and `echo` are as generic as `json`: a value without their string field is someone
	/// else's and decodes as absent, while one with it is ours.
	#[test]
	fn a_foreign_stats_or_echo_value_does_not_fail_the_catalog() {
		for foreign in ["{}", "5", r#""on""#, r#"{"track":5}"#, r#"{"path":["a"]}"#, "null"] {
			for section in ["stats", "echo"] {
				let wire = format!(r#"{{"video":{{"renditions":{{}}}},"{section}":{foreign}}}"#);
				let catalog = Catalog::<()>::from_str(&wire)
					.unwrap_or_else(|err| panic!("{section}: {foreign} took the catalog down: {err}"));
				assert!(
					catalog.stats.is_none() && catalog.echo.is_none(),
					"{section}: {foreign}"
				);
			}
		}
		let catalog = Catalog::<()>::from_str(r#"{"stats":{"track":"stats.json"},"echo":{"path":"viewers"}}"#).unwrap();
		assert_eq!(catalog.stats, Some(crate::catalog::Stats::new("stats.json")));
		assert_eq!(
			catalog.echo,
			Some(crate::catalog::Echo::new(
				moq_net::path::Relative::new("viewers").unwrap()
			))
		);
	}

	#[test]
	fn data_sections_stay_off_the_wire_when_empty() {
		// Empty sections stay off the wire, video and audio included.
		let output = Catalog::<()>::default().to_json().expect("failed to encode");
		assert_eq!(output, "{}");
	}

	#[test]
	fn clock_roundtrips_at_the_root() {
		let clock =
			crate::catalog::Clock::new(moq_net::Timestamp::from_micros(1_751_846_400_000_000).unwrap()).unwrap();
		let catalog = Catalog::<()> {
			clock: Some(clock),
			..Default::default()
		};

		let json = catalog.to_json().expect("failed to encode");
		assert!(json.contains(r#""clock":{"wall":1751846400000000"#), "{json}");
		assert_eq!(
			Catalog::<()>::from_str(&json).expect("failed to decode").clock,
			Some(clock)
		);
	}

	#[test]
	fn m2ts_roundtrips_at_the_root() {
		let mut m2ts = crate::catalog::M2ts::new("0.m2ts");
		m2ts.random_access = true;
		let catalog = Catalog::<()> {
			m2ts: Some(m2ts.clone()),
			..Default::default()
		};

		let json = catalog.to_json().expect("failed to encode");
		assert_eq!(json, r#"{"m2ts":{"track":"0.m2ts","randomAccess":true}}"#);
		assert_eq!(
			Catalog::<()>::from_str(&json).expect("failed to decode").m2ts,
			Some(m2ts)
		);
	}

	#[test]
	fn packaged_clock_fixture_decodes() {
		// The canonical shape: the root clock plus a per-rendition `hls` field. A reader maps HLS
		// record timestamps (milliseconds) through the root clock after timescale conversion.
		let json = include_str!("../../fixtures/catalog-clock.json");
		let catalog = Catalog::<()>::from_str(json).expect("the packaged fixture must decode");

		let clock = catalog.clock.expect("the fixture carries a root clock");
		assert_eq!(clock.wall.scale().as_u64(), 1_000_000);

		let hls = catalog.video.renditions["video"]
			.hls
			.clone()
			.expect("the fixture opts video into HLS");
		assert_eq!(
			hls,
			crate::catalog::Hls::new("video.hls", std::time::Duration::from_secs(2))
		);

		// Record PTS 2000 (ms) lands 2s after the wall epoch, whatever the track timescale.
		let expected = moq_clock::Wall::UNIX_EPOCH
			+ std::time::Duration::from_millis(crate::catalog::MOQ_EPOCH_UNIX_MILLIS + 1_751_846_402_000);
		assert_eq!(
			clock
				.wall_clock(moq_net::Timestamp::from_millis(2000).unwrap())
				.unwrap(),
			expected
		);
	}

	#[test]
	fn a_retired_root_archive_is_ignored() {
		let video = serde_json::json!({
			"renditions": {"video": {"codec": "avc1.64001f", "container": {"kind": "legacy"}}}
		});
		let media = Catalog::<()>::from_str(&serde_json::json!({"video": video}).to_string()).unwrap();
		let archive = serde_json::json!({
			"timelines": {"video": {"track": "video.timeline.z", "duration": 2000}},
			"timescale": 1000,
			"replay": "./recordings/clip",
			"store": "https://objects.example/rec/",
			"version": 3
		});
		let json = serde_json::json!({"video": video, "archive": archive}).to_string();
		let catalog = Catalog::<()>::from_str(&json).expect("a retired archive must not reject the catalog");
		assert_eq!(catalog, media);
		assert!(!catalog.to_json().unwrap().contains("archive"));
	}

	#[test]
	fn an_undecodable_hls_field_leaves_media_playable() {
		let rendition = serde_json::json!({"codec": "avc1.64001f", "container": {"kind": "legacy"}});
		let media =
			Catalog::<()>::from_str(&serde_json::json!({"video": {"renditions": {"video": rendition}}}).to_string())
				.unwrap();
		for hls in [
			serde_json::json!({"track": "video.hls"}),
			serde_json::json!({"duration": 4000}),
			serde_json::json!("video.hls"),
			serde_json::Value::Null,
		] {
			let mut rendition = rendition.clone();
			rendition["hls"] = hls;
			let json = serde_json::json!({"video": {"renditions": {"video": rendition}}}).to_string();
			let catalog = Catalog::<()>::from_str(&json).expect("HLS metadata must not reject media");
			assert_eq!(catalog, media);
		}
	}

	#[test]
	fn data_tracks_roundtrip() {
		let mut encoded = r#"{
			"json": {
				"tracks": {
					"chat": {
						"mode": "stream",
						"schema": "https://example.com/chat.schema.json"
					},
					"status": {
						"broadcast": "source",
						"mode": "snapshot"
					}
				}
			},
			"binary": {
				"tracks": {
					"thumbnail": {
						"mode": "snapshot",
						"mime": "image/jpeg"
					}
				}
			}
		}"#
		.to_string();
		encoded.retain(|c| !c.is_whitespace());

		let mut chat = JsonConfig::new(Mode::Stream);
		chat.schema = Some("https://example.com/chat.schema.json".to_string());

		let mut status = JsonConfig::new(Mode::Snapshot);
		status.broadcast = Some(moq_net::path::Relative::new("source").unwrap());

		let mut thumbnail = BinaryConfig::new(Mode::Snapshot);
		thumbnail.mime = Some("image/jpeg".to_string());

		let mut catalog = Catalog::<()>::default();
		catalog.json.insert("chat", chat).unwrap();
		catalog.json.insert("status", status).unwrap();
		catalog.binary.insert("thumbnail", thumbnail).unwrap();

		let decoded = Catalog::<()>::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, catalog, "decode mismatch");

		let output = catalog.to_json().expect("failed to encode");
		assert_eq!(output, encoded, "encode mismatch");
	}

	/// Data tracks carry the same optional `bitrate` and whole-millisecond `jitter` and `delay` as
	/// media.
	#[test]
	fn data_track_bitrate_and_jitter() {
		let encoded = r#"{"json":{"tracks":{"gps":{"mode":"stream","bitrate":8000,"jitter":100,"delay":250}}},"binary":{"tracks":{"frames":{"mode":"snapshot","bitrate":64000,"jitter":34}}}}"#;

		let mut gps = JsonConfig::new(Mode::Stream);
		gps.bitrate = Some(8_000);
		gps.jitter = Some(std::time::Duration::from_millis(100));
		gps.delay = Some(std::time::Duration::from_micros(249_001));

		let mut frames = BinaryConfig::new(Mode::Snapshot);
		frames.bitrate = Some(64_000);
		frames.jitter = Some(std::time::Duration::from_micros(33_334));

		let mut catalog = Catalog::<()>::default();
		catalog.json.insert("gps", gps).unwrap();
		catalog.binary.insert("frames", frames).unwrap();

		assert_eq!(
			catalog.to_json().unwrap(),
			encoded,
			"jitter rounds up to whole milliseconds"
		);

		let decoded = Catalog::<()>::from_str(encoded).unwrap();
		assert_eq!(
			decoded.binary.tracks["frames"].jitter,
			Some(std::time::Duration::from_millis(34))
		);
		assert_eq!(decoded.json.tracks["gps"].bitrate, Some(8_000));
		assert_eq!(
			decoded.json.tracks["gps"].delay,
			Some(std::time::Duration::from_millis(250))
		);
	}

	/// An application lists a data track in its own section with a nested data config, keeping config
	/// fields separate from the application's own fields.
	#[test]
	fn a_data_config_nests_in_an_application_entry() {
		#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
		struct Mavlink {
			config: BinaryConfig,
			sysid: u8,
		}

		#[derive(Serialize, Deserialize, PartialEq, Debug, Default, Clone)]
		struct Ext {
			#[serde(rename = "com.example.mavlink", default)]
			mavlink: BTreeMap<String, Mavlink>,
		}

		let encoded = r#"{"com.example.mavlink":{"telemetry":{"config":{"mode":"stream","mime":"application/mavlink"},"sysid":1}}}"#;

		let catalog = Catalog::<Ext>::from_str(encoded).unwrap();
		let entry = &catalog.ext.mavlink["telemetry"];
		assert_eq!(entry.sysid, 1);
		assert_eq!(entry.config.mode, Mode::Stream);
		assert_eq!(entry.config.mime.as_deref(), Some("application/mavlink"));
		assert_eq!(catalog.to_json().unwrap(), encoded);
	}

	/// A track using a future mode must survive a reparse-and-republish intact, so a relay doesn't
	/// corrupt what it can't read. Its siblings stay readable.
	#[test]
	fn unknown_mode_keeps_siblings() {
		let encoded = r#"{
			"json": {
				"tracks": {
					"future": {"mode": "windowed"},
					"known": {"mode": "stream"}
				}
			}
		}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");

		let known = parsed.json.tracks.get("known").expect("missing track");
		assert_eq!(known.mode, Mode::Stream);

		let future = parsed.json.tracks.get("future").expect("missing track");
		assert_eq!(future.mode, Mode::Unknown("windowed".to_string()));

		let output = parsed.to_json().expect("failed to encode");
		assert!(
			output.contains(r#""mode":"windowed""#),
			"unknown mode dropped: {output}"
		);
		let reparsed = Catalog::<()>::from_str(&output).expect("failed to re-decode");
		assert_eq!(parsed, reparsed, "re-encoded catalog did not round-trip");
	}

	/// Preserving the mode string alone is not enough: a future mode comes with fields describing
	/// it, and a relay that reparsed and republished would otherwise strip them, leaving an entry
	/// nothing can act on.
	#[test]
	fn unknown_mode_fields_round_trip() {
		let encoded = r#"{"json":{"tracks":{"future":{"mode":"windowed","window":10}}}}"#;

		let parsed = Catalog::<()>::from_str(encoded).expect("failed to decode");
		let future = parsed.json.tracks.get("future").expect("missing track");
		assert_eq!(future.mode, Mode::Unknown("windowed".to_string()));
		assert_eq!(future.extra.get("window"), Some(&serde_json::json!(10)));

		let output = parsed.to_json().expect("failed to encode");
		assert!(output.contains(r#""window":10"#), "unknown fields dropped: {output}");
		assert_eq!(Catalog::<()>::from_str(&output).expect("re-decode"), parsed);
	}

	/// There is no safe default: reading a stream as a snapshot silently drops every record but the
	/// last, so an entry without a mode is malformed rather than assumed.
	#[test]
	fn a_track_without_a_mode_is_rejected() {
		Catalog::<()>::from_str(r#"{"json":{"tracks":{"chat":{"schema":"chat"}}}}"#)
			.expect_err("a mode-less track must not decode");
	}

	#[test]
	fn duplicate_data_track_names_are_rejected() {
		let mut catalog = Catalog::<()>::default();
		catalog.json.insert("chat", JsonConfig::new(Mode::Stream)).unwrap();
		assert!(matches!(
			catalog.json.insert("chat", JsonConfig::new(Mode::Snapshot)),
			Err(crate::Error::Duplicate(_))
		));

		// The two sections are separate namespaces on the wire, so the same name in each is fine.
		catalog.binary.insert("chat", BinaryConfig::new(Mode::Stream)).unwrap();
	}

	#[test]
	fn extension_roundtrip() {
		// An application extends the catalog with its own root section by flattening Catalog.
		#[derive(Serialize, Deserialize, PartialEq, Debug)]
		struct AppCatalog {
			#[serde(flatten)]
			base: Catalog,
			#[serde(skip_serializing_if = "Option::is_none")]
			scte35: Option<Scte35>,
		}

		#[derive(Serialize, Deserialize, PartialEq, Debug)]
		struct Scte35 {
			splice_id: u32,
		}

		let app = AppCatalog {
			base: Catalog::<()>::default(),
			scte35: Some(Scte35 { splice_id: 42 }),
		};

		let json = serde_json::to_string(&app).expect("failed to encode");

		// A base consumer ignores the unknown section.
		let base = Catalog::<()>::from_str(&json).expect("failed to decode base");
		assert_eq!(base, Catalog::<()>::default());

		// The extended consumer round-trips its own section.
		let decoded: AppCatalog = serde_json::from_str(&json).expect("failed to decode app");
		assert_eq!(decoded, app);
	}

	/// A newer draft may reshape a section, so the version is refused before any section is read,
	/// wherever it sits in the object.
	#[test]
	fn a_newer_version_is_refused() {
		for json in [
			r#"{"version":5}"#,
			r#"{"video":{"renditions":{"hd":{"codec":42}}},"version":5}"#,
		] {
			assert!(
				matches!(Catalog::<()>::from_str(json), Err(crate::Error::UnsupportedVersion(5))),
				"{json}"
			);
		}

		// Plain serde refuses too, without the typed error.
		let err = serde_json::from_str::<Catalog>(r#"{"version":5}"#).unwrap_err();
		assert!(err.to_string().contains("unsupported catalog version 5"), "{err}");
	}

	/// A repeated `version` is refused rather than resolved to either copy, so a newer catalog can't
	/// slip past the check by also claiming an old version.
	#[test]
	fn a_duplicate_version_is_refused() {
		for json in [r#"{"version":5,"version":3}"#, r#"{"version":3,"version":3}"#] {
			let err = Catalog::<()>::from_str(json).unwrap_err();
			assert!(err.to_string().contains("duplicate field `version`"), "{json}: {err}");
			assert!(Catalog::<()>::from_reader(json.as_bytes()).is_err(), "{json}");
		}
	}

	#[test]
	fn a_supported_version_is_read() {
		for json in [r#"{}"#, r#"{"version":3}"#, r#"{"version":4}"#] {
			assert_eq!(Catalog::<()>::from_str(json).unwrap(), Catalog::default(), "{json}");
		}
		for malformed in [r#"{"version":"4"}"#, r#"{"version":-1}"#, r#"{"version":4.5}"#] {
			assert!(
				matches!(Catalog::<()>::from_str(malformed), Err(crate::Error::Json(_))),
				"{malformed}"
			);
		}
	}

	/// The version is the root's, so an extension never sees it, even one that passes every
	/// unknown member through.
	#[test]
	fn the_version_is_not_an_extension_member() {
		type Passthrough = serde_json::Map<String, serde_json::Value>;
		let catalog = Catalog::<Passthrough>::from_str(r#"{"version":4,"scte35":{}}"#).unwrap();
		assert!(catalog.ext.contains_key("scte35"));
		assert!(!catalog.ext.contains_key("version"));
	}

	/// An extension's own `version` would sit beside the computed one as a duplicate key, letting it
	/// override what the catalog actually needs.
	#[test]
	fn an_extension_version_is_refused() {
		#[derive(Serialize, Default)]
		struct Ext {
			version: u64,
		}

		let catalog = Catalog {
			ext: Ext { version: 3 },
			..Default::default()
		};
		let err = catalog.to_json().unwrap_err();
		assert!(err.to_string().contains("must not name a member `version`"), "{err}");

		let mut passthrough = Catalog::<serde_json::Map<String, serde_json::Value>>::default();
		passthrough.ext.insert("version".into(), 3.into());
		assert!(passthrough.to_json().is_err());

		// Any other member still flattens beside the base sections.
		let ok = Catalog {
			ext: serde_json::json!({ "scte35": {} }),
			..Default::default()
		};
		assert_eq!(ok.to_json().unwrap(), r#"{"scte35":{}}"#);
	}

	/// Only a field an older reader would misread raises the version; one it would merely ignore
	/// leaves the catalog readable by moq-hang-03.
	#[test]
	fn the_version_is_written_only_when_needed() {
		let rendition = || {
			let mut config = VideoConfig::new(H264 {
				profile: 0x64,
				constraints: 0x00,
				level: 0x1f,
				inline: false,
			});
			config.container = Container::Legacy;
			config
		};
		let version = |catalog: &Catalog| serde_json::to_value(catalog).unwrap().get("version").cloned();

		let mut catalog = Catalog::<()>::default();
		catalog.video.renditions.insert("hd".into(), rendition());
		let mut audio = AudioConfig::new(Opus, 48_000, 2);
		audio.warmup = Some(std::time::Duration::from_millis(80));
		catalog.audio.renditions.insert("en".into(), audio);
		assert_eq!(version(&catalog), None, "an older reader still plays audio warmup");

		// A `track` naming the ID is what an older reader subscribes to anyway.
		catalog.video.renditions.get_mut("hd").unwrap().track = Some("hd".into());
		assert_eq!(version(&catalog), None);

		catalog.video.renditions.get_mut("hd").unwrap().track = Some("video".into());
		assert_eq!(version(&catalog), Some(4.into()));
		let json = catalog.to_json().unwrap();
		assert!(json.starts_with(r#"{"version":4,"#), "{json}");
		assert_eq!(Catalog::<()>::from_str(&json).unwrap(), catalog);

		let mut data = Catalog::<()>::default();
		let mut chat = JsonConfig::new(Mode::Stream);
		chat.track = Some("chat.json".into());
		data.json.insert("chat", chat).unwrap();
		assert_eq!(version(&data), Some(4.into()));

		let mut warmup = Catalog::<()>::default();
		let mut config = rendition();
		config.warmup = Some(std::time::Duration::from_millis(500));
		warmup.video.renditions.insert("hd".into(), config);
		assert_eq!(version(&warmup), Some(4.into()));
	}

	/// The version is computed, never carried: republishing a catalog that claimed more than it
	/// uses writes what it actually needs.
	#[test]
	fn a_republish_recomputes_the_version() {
		let catalog = Catalog::<()>::from_str(r#"{"version":4}"#).unwrap();
		assert_eq!(catalog.to_json().unwrap(), "{}");
	}
}

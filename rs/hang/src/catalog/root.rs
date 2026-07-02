//! This module contains the structs and functions for the MoQ catalog format
use crate::Result;
use crate::catalog::{Audio, Video};
use serde::{Deserialize, Serialize};

/// A catalog track, created by a broadcaster to describe the tracks available in a broadcast.
///
/// The base catalog carries only the media sections (`video`, `audio`). Applications extend it with
/// their own root sections (e.g. `scte35`) by flattening this struct into their own with
/// `#[serde(flatten)]`. The catalog does not deny unknown fields, so a base consumer ignores the
/// extra sections and an extended catalog stays wire-compatible. See the `extension_roundtrip` test.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Catalog {
	/// Video track information with multiple renditions.
	///
	/// Contains a map of video track renditions that the viewer can choose from
	/// based on their preferences (resolution, bitrate, codec, etc).
	#[serde(default)]
	pub video: Video,

	/// Audio track information with multiple renditions.
	///
	/// Contains a map of audio track renditions that the viewer can choose from
	/// based on their preferences (codec, bitrate, language, etc).
	#[serde(default)]
	pub audio: Audio,
}

impl Catalog {
	/// The default name for the catalog track.
	pub const DEFAULT_NAME: &str = "catalog.json";

	/// The track name for the DEFLATE-compressed catalog: the `.z` sibling of [`DEFAULT_NAME`](Self::DEFAULT_NAME).
	///
	/// Carries the identical catalog JSON, compressed per group (see `moq-json`). A publisher serves
	/// both tracks; a consumer reads whichever it prefers.
	pub const COMPRESSED_NAME: &str = "catalog.json.z";

	/// Parse a catalog from a string.
	#[allow(clippy::should_implement_trait)]
	pub fn from_str(s: &str) -> Result<Self> {
		Ok(serde_json::from_str(s)?)
	}

	/// Parse a catalog from a slice of bytes.
	pub fn from_slice(v: &[u8]) -> Result<Self> {
		Ok(serde_json::from_slice(v)?)
	}

	/// Parse a catalog from a reader.
	pub fn from_reader(reader: impl std::io::Read) -> Result<Self> {
		Ok(serde_json::from_reader(reader)?)
	}

	/// Serialize the catalog to a string.
	pub fn to_string(&self) -> Result<String> {
		Ok(serde_json::to_string(self)?)
	}

	/// Serialize the catalog to a pretty string.
	pub fn to_string_pretty(&self) -> Result<String> {
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

	/// Track properties for creating the catalog track via
	/// [`create_track`](moq_net::BroadcastProducer::create_track) at
	/// [`DEFAULT_NAME`](Self::DEFAULT_NAME).
	pub fn default_track_info() -> moq_net::TrackInfo {
		moq_net::TrackInfo::default()
	}

	/// The subscription preferences used for the catalog track (high priority so
	/// it preempts media tracks).
	pub fn default_subscription() -> moq_net::Subscription {
		moq_net::Subscription::default().with_priority(100)
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use crate::catalog::{AudioCodec::Opus, AudioConfig, Container, H264, VideoConfig};

	use super::*;

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

		let decoded = Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			audio: Audio {
				renditions: audio_renditions,
			},
		};

		let output = Catalog::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, output, "wrong decoded output");

		let output = decoded.to_string().expect("failed to encode");
		assert_eq!(encoded, output, "wrong encoded output");
	}

	/// Lock in the on-wire shape of the jitter field: a bare integer number
	/// of milliseconds. If `Option<Duration>` ever loses the `duration_millis`
	/// serde adapter, this regresses to serde's default `{secs, nanos}` shape.
	#[test]
	fn jitter_serialized_as_millis() {
		let mut encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"codec": "avc1.64001f",
						"container": {"kind": "legacy"},
						"jitter": 100
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
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: Some(std::time::Duration::from_millis(100)),
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio".to_string(),
			AudioConfig {
				codec: Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: None,
				description: None,
				container: Container::Legacy,
				jitter: Some(std::time::Duration::from_millis(40)),
			},
		);

		let catalog = Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			audio: Audio {
				renditions: audio_renditions,
			},
		};

		let decoded = Catalog::from_str(&encoded).expect("failed to decode");
		assert_eq!(catalog, decoded, "decode mismatch");

		let output = catalog.to_string().expect("failed to encode");
		assert_eq!(encoded, output, "encode mismatch");
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
			base: Catalog::default(),
			scte35: Some(Scte35 { splice_id: 42 }),
		};

		let json = serde_json::to_string(&app).expect("failed to encode");

		// A base consumer ignores the unknown section.
		let base = Catalog::from_str(&json).expect("failed to decode base");
		assert_eq!(base, Catalog::default());

		// The extended consumer round-trips its own section.
		let decoded: AppCatalog = serde_json::from_str(&json).expect("failed to decode app");
		assert_eq!(decoded, app);
	}
}

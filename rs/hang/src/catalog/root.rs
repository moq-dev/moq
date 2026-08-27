//! This module contains the structs and functions for the MoQ catalog format
use crate::Result;
use crate::catalog::{Audio, Binary, Json, PRIORITY, Video};
use serde::{Deserialize, Serialize};

/// A catalog track, created by a broadcaster to describe the tracks available in a broadcast.
///
/// The base catalog carries only the media sections (`video`, `audio`). Applications extend it with
/// their own root sections (e.g. `scte35`) by flattening this struct into their own with
/// `#[serde(flatten)]`. The catalog does not deny unknown fields, so a base consumer ignores the
/// extra sections and an extended catalog stays wire-compatible. See the `extension_roundtrip` test.
///
/// Marked `#[non_exhaustive]` so a future base section can be added without bumping the major
/// version. External callers start from [`Catalog::default`] and fill in the sections they
/// publish; struct-literal construction (with or without `..base`) is not available outside
/// this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
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

	/// JSON tracks: application data published as live JSON documents or logs.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged.
	#[serde(default, skip_serializing_if = "Json::is_empty")]
	pub json: Json,

	/// Binary tracks: application data published as opaque payloads.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged.
	#[serde(default, skip_serializing_if = "Binary::is_empty")]
	pub binary: Binary,
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

	/// Track properties for creating the catalog track via
	/// [`create_track`](moq_net::broadcast::Producer::create_track) at
	/// [`DEFAULT_NAME`](Self::DEFAULT_NAME).
	pub fn default_track_info() -> moq_net::track::Info {
		moq_net::track::Info::default()
	}

	/// The subscription preferences used for the catalog track (high priority so
	/// it preempts media tracks).
	pub fn default_subscription() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default().with_priority(PRIORITY.catalog)
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use crate::catalog::{
		AudioCodec::Opus, AudioConfig, BinaryConfig, Compression, Container, H264, JsonConfig, Mode, VideoConfig,
	};

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

		let mut decoded = Catalog::default();
		decoded.video.renditions = video_renditions;
		decoded.audio.renditions = audio_renditions;

		let output = Catalog::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, output, "wrong decoded output");

		let output = decoded.to_json().expect("failed to encode");
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
				broadcast: None,
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
				bitrate: None,
				stalled: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: Some(std::time::Duration::from_millis(100)),
				timeline: None,
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio".to_string(),
			AudioConfig {
				broadcast: None,
				codec: Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: None,
				description: None,
				container: Container::Legacy,
				jitter: Some(std::time::Duration::from_millis(40)),
				timeline: None,
			},
		);

		let mut catalog = Catalog::default();
		catalog.video.renditions = video_renditions;
		catalog.audio.renditions = audio_renditions;

		let decoded = Catalog::from_str(&encoded).expect("failed to decode");
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

		let parsed = Catalog::from_str(encoded).expect("failed to decode");
		let rendition = parsed.video.renditions.get("video").expect("missing rendition");
		assert_eq!(
			rendition.broadcast.as_ref().map(|p| p.as_str()),
			Some("source"),
			"broadcast field did not deserialize"
		);

		// Full encode -> decode -> equality, so the test catches any encoder regression
		// (e.g. wrong key, double-emission, or `null` instead of skip).
		let output = parsed.to_json().expect("failed to encode");
		let reparsed = Catalog::from_str(&output).expect("failed to re-decode");
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

		let catalog = Catalog {
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
		// An empty-string broadcast field should normalize to an empty PathRelative so the
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

		let parsed = Catalog::from_str(encoded).expect("failed to decode");
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

		let parsed = Catalog::from_str(encoded).expect("failed to decode");
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

		let parsed = Catalog::from_str(encoded).expect("failed to decode");

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
		let reparsed = Catalog::from_str(&output).expect("failed to re-decode");
		assert_eq!(parsed, reparsed, "re-encoded catalog did not round-trip");
		assert!(output.contains(r#""magic":7"#), "unknown fields dropped: {output}");
	}

	#[test]
	fn data_sections_stay_off_the_wire_when_empty() {
		// A media-only catalog must serialize exactly as it did before the data sections existed,
		// or every existing publisher's bytes change.
		let output = Catalog::default().to_json().expect("failed to encode");
		assert_eq!(output, r#"{"video":{"renditions":{}},"audio":{"renditions":{}}}"#);
	}

	#[test]
	fn data_tracks_roundtrip() {
		let mut encoded = r#"{
			"video": {"renditions": {}},
			"audio": {"renditions": {}},
			"json": {
				"tracks": {
					"chat": {
						"mode": "stream",
						"compression": "deflate",
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
		chat.compression = Some(Compression::Deflate);
		chat.schema = Some("https://example.com/chat.schema.json".to_string());

		let mut status = JsonConfig::new(Mode::Snapshot);
		status.broadcast = Some(moq_net::PathRelativeOwned::new("source"));

		let mut thumbnail = BinaryConfig::new(Mode::Snapshot);
		thumbnail.mime = Some("image/jpeg".to_string());

		let mut catalog = Catalog::default();
		catalog.json.insert("chat", chat).unwrap();
		catalog.json.insert("status", status).unwrap();
		catalog.binary.insert("thumbnail", thumbnail).unwrap();

		let decoded = Catalog::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, catalog, "decode mismatch");

		let output = catalog.to_json().expect("failed to encode");
		assert_eq!(output, encoded, "encode mismatch");
	}

	/// A track using a future mode or compression must survive a reparse-and-republish intact, so a
	/// relay doesn't corrupt what it can't read. Its siblings stay readable.
	#[test]
	fn unknown_mode_and_compression_keep_siblings() {
		let encoded = r#"{
			"json": {
				"tracks": {
					"future": {"mode": "windowed", "compression": "zstd"},
					"known": {"mode": "stream", "compression": "deflate"}
				}
			}
		}"#;

		let parsed = Catalog::from_str(encoded).expect("failed to decode");

		let known = parsed.json.tracks.get("known").expect("missing track");
		assert_eq!(known.mode, Mode::Stream);
		assert_eq!(known.compression, Some(Compression::Deflate));

		let future = parsed.json.tracks.get("future").expect("missing track");
		assert_eq!(future.mode, Mode::Unknown("windowed".to_string()));
		assert_eq!(future.compression, Some(Compression::Unknown("zstd".to_string())));

		let output = parsed.to_json().expect("failed to encode");
		assert!(
			output.contains(r#""mode":"windowed""#),
			"unknown mode dropped: {output}"
		);
		let reparsed = Catalog::from_str(&output).expect("failed to re-decode");
		assert_eq!(parsed, reparsed, "re-encoded catalog did not round-trip");
	}

	/// There is no safe default: reading a stream as a snapshot silently drops every record but the
	/// last, so an entry without a mode is malformed rather than assumed.
	#[test]
	fn a_track_without_a_mode_is_rejected() {
		Catalog::from_str(r#"{"json":{"tracks":{"chat":{"compression":"deflate"}}}}"#)
			.expect_err("a mode-less track must not decode");
	}

	#[test]
	fn duplicate_data_track_names_are_rejected() {
		let mut catalog = Catalog::default();
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

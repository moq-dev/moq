//! MSF (MOQT Streaming Format) catalog support.
//!
//! This module provides types for the MSF catalog format as defined in
//! draft-ietf-moq-msf-00. It also provides conversion from the hang
//! [`Catalog`] so that an MSF catalog can be published alongside the
//! native hang catalog.
//!
//! Reference: <https://www.ietf.org/archive/id/draft-ietf-moq-msf-00.txt>

use base64::Engine;
use serde::{Deserialize, Serialize};

use super::Catalog;

/// The default track name for the MSF catalog.
pub const DEFAULT_NAME: &str = "msf.json";

/// Root MSF catalog object.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MsfCatalog {
	/// MSF version — always 1 for this draft.
	pub version: u32,

	/// Array of track descriptions.
	pub tracks: Vec<MsfTrack>,
}

/// A single track in the MSF catalog.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MsfTrack {
	/// Unique track name (case-sensitive).
	pub name: String,

	/// Packaging mode: "loc", "mediatimeline", or "eventtimeline".
	pub packaging: String,

	/// Whether new objects will be appended.
	pub is_live: bool,

	/// Content role: "video", "audio", etc.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub role: Option<String>,

	/// WebCodecs codec string.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub codec: Option<String>,

	/// Video frame width in pixels.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub width: Option<u32>,

	/// Video frame height in pixels.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub height: Option<u32>,

	/// Video frame rate.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub framerate: Option<f64>,

	/// Audio sample rate in Hz.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub samplerate: Option<u32>,

	/// Audio channel configuration.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub channel_config: Option<String>,

	/// Bitrate in bits per second.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bitrate: Option<u64>,

	/// Base64-encoded initialization data.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub init_data: Option<String>,

	/// Render group for synchronized playback.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub render_group: Option<u32>,

	/// Alternate group for quality switching.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub alt_group: Option<u32>,
}

impl MsfCatalog {
	/// Serialize the MSF catalog to a JSON string.
	pub fn to_string(&self) -> crate::Result<String> {
		Ok(serde_json::to_string(self)?)
	}
}

impl From<&Catalog> for MsfCatalog {
	fn from(catalog: &Catalog) -> Self {
		let mut tracks = Vec::new();

		// Assign all video renditions to the same render group and alt group.
		let has_multiple_video = catalog.video.renditions.len() > 1;
		for (name, config) in &catalog.video.renditions {
			let init_data = config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref()));

			tracks.push(MsfTrack {
				name: name.clone(),
				packaging: "loc".to_string(),
				is_live: true,
				role: Some("video".to_string()),
				codec: Some(config.codec.to_string()),
				width: config.coded_width,
				height: config.coded_height,
				framerate: config.framerate,
				samplerate: None,
				channel_config: None,
				bitrate: config.bitrate,
				init_data,
				render_group: Some(1),
				alt_group: if has_multiple_video { Some(1) } else { None },
			});
		}

		// Assign all audio renditions to the same render group and alt group.
		let has_multiple_audio = catalog.audio.renditions.len() > 1;
		for (name, config) in &catalog.audio.renditions {
			let init_data = config
				.description
				.as_ref()
				.map(|d| base64::engine::general_purpose::STANDARD.encode(d.as_ref()));

			tracks.push(MsfTrack {
				name: name.clone(),
				packaging: "loc".to_string(),
				is_live: true,
				role: Some("audio".to_string()),
				codec: Some(config.codec.to_string()),
				width: None,
				height: None,
				framerate: None,
				samplerate: Some(config.sample_rate),
				channel_config: Some(config.channel_count.to_string()),
				bitrate: config.bitrate,
				init_data,
				render_group: Some(1),
				alt_group: if has_multiple_audio { Some(1) } else { None },
			});
		}

		MsfCatalog { version: 1, tracks }
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use bytes::Bytes;

	use crate::catalog::{Audio, AudioCodec, AudioConfig, Container, H264, Video, VideoConfig};

	use super::*;

	#[test]
	fn convert_simple() {
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0.avc3".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: true,
				}
				.into(),
				description: None,
				coded_width: Some(1280),
				coded_height: Some(720),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: Some(6_000_000),
				framerate: Some(30.0),
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio0".to_string(),
			AudioConfig {
				codec: AudioCodec::Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: Some(128_000),
				description: None,
				container: Container::Legacy,
				jitter: None,
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
			..Default::default()
		};

		let msf = MsfCatalog::from(&catalog);

		assert_eq!(msf.version, 1);
		assert_eq!(msf.tracks.len(), 2);

		let video = &msf.tracks[0];
		assert_eq!(video.name, "video0.avc3");
		assert_eq!(video.role, Some("video".to_string()));
		assert_eq!(video.codec, Some("avc3.64001f".to_string()));
		assert_eq!(video.width, Some(1280));
		assert_eq!(video.height, Some(720));
		assert_eq!(video.framerate, Some(30.0));
		assert_eq!(video.bitrate, Some(6_000_000));
		assert!(video.init_data.is_none());

		let audio = &msf.tracks[1];
		assert_eq!(audio.name, "audio0");
		assert_eq!(audio.role, Some("audio".to_string()));
		assert_eq!(audio.codec, Some("opus".to_string()));
		assert_eq!(audio.samplerate, Some(48_000));
		assert_eq!(audio.channel_config, Some("2".to_string()));
		assert_eq!(audio.bitrate, Some(128_000));
	}

	#[test]
	fn convert_with_description() {
		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video0.m4s".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: false,
				}
				.into(),
				description: Some(Bytes::from_static(&[0x01, 0x02, 0x03])),
				coded_width: Some(1920),
				coded_height: Some(1080),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: None,
				framerate: None,
				optimize_for_latency: None,
				container: Container::Legacy,
				jitter: None,
			},
		);

		let catalog = Catalog {
			video: Video {
				renditions: video_renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		};

		let msf = MsfCatalog::from(&catalog);
		let video = &msf.tracks[0];
		assert_eq!(video.init_data, Some("AQID".to_string()));
	}

	#[test]
	fn convert_empty() {
		let catalog = Catalog::default();
		let msf = MsfCatalog::from(&catalog);
		assert_eq!(msf.version, 1);
		assert!(msf.tracks.is_empty());
	}

	#[test]
	fn roundtrip_json() {
		let catalog = Catalog::default();
		let msf = MsfCatalog::from(&catalog);
		let json = msf.to_string().expect("failed to serialize");
		let parsed: MsfCatalog = serde_json::from_str(&json).expect("failed to parse");
		assert_eq!(msf, parsed);
	}
}

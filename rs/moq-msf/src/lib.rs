//! MSF (MOQT Streaming Format) catalog types.
//!
//! This crate provides types for the MSF catalog format as defined in
//! draft-ietf-moq-msf-00, with additional support for CMAF packaging
//! from draft-ietf-moq-cmsf-00.
//!
//! References:
//! - <https://www.ietf.org/archive/id/draft-ietf-moq-msf-00.txt>
//! - <https://www.ietf.org/archive/id/draft-ietf-moq-cmsf-00.txt>

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The default track name for the MSF catalog.
pub const DEFAULT_NAME: &str = "catalog";

/// Root MSF catalog object.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
	/// MSF version — always 1 for this draft.
	pub version: u32,

	/// Array of track descriptions.
	pub tracks: Vec<Track>,
}

/// A single track in the MSF catalog.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
	/// Unique track name (case-sensitive).
	pub name: String,

	/// Packaging mode.
	pub packaging: Packaging,

	/// Whether new objects will be appended.
	pub is_live: bool,

	/// Content role.
	pub role: Option<Role>,

	/// WebCodecs codec string.
	pub codec: Option<String>,

	/// Video frame width in pixels.
	pub width: Option<u32>,

	/// Video frame height in pixels.
	pub height: Option<u32>,

	/// Video frame rate.
	pub framerate: Option<f64>,

	/// Audio sample rate in Hz.
	pub samplerate: Option<u32>,

	/// Audio channel configuration.
	pub channel_config: Option<String>,

	/// Bitrate in bits per second.
	pub bitrate: Option<u64>,

	/// Base64-encoded initialization data.
	pub init_data: Option<String>,

	/// Render group for synchronized playback.
	pub render_group: Option<u32>,

	/// Alternate group for quality switching.
	pub alt_group: Option<u32>,
}

impl Catalog {
	/// Serialize the MSF catalog to a JSON string.
	pub fn to_string(&self) -> Result<String, serde_json::Error> {
		serde_json::to_string(self)
	}

	/// Deserialize an MSF catalog from a JSON string.
	#[allow(clippy::should_implement_trait)]
	pub fn from_str(s: &str) -> Result<Self, serde_json::Error> {
		serde_json::from_str(s)
	}
}

/// Packaging mode for an MSF track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packaging {
	/// Low Overhead Container (MSF).
	Loc,
	/// CMAF fragmented MP4 (CMSF).
	Cmaf,
	/// Legacy container format (timestamp + raw codec payload).
	Legacy,
	/// Media timeline.
	MediaTimeline,
	/// Event timeline.
	EventTimeline,
	/// Unknown packaging type.
	Unknown(String),
}

impl fmt::Display for Packaging {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Packaging::Loc => write!(f, "loc"),
			Packaging::Cmaf => write!(f, "cmaf"),
			Packaging::Legacy => write!(f, "legacy"),
			Packaging::MediaTimeline => write!(f, "mediatimeline"),
			Packaging::EventTimeline => write!(f, "eventtimeline"),
			Packaging::Unknown(s) => write!(f, "{s}"),
		}
	}
}

impl FromStr for Packaging {
	type Err = std::convert::Infallible;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"loc" => Packaging::Loc,
			"cmaf" => Packaging::Cmaf,
			"legacy" => Packaging::Legacy,
			"mediatimeline" => Packaging::MediaTimeline,
			"eventtimeline" => Packaging::EventTimeline,
			other => Packaging::Unknown(other.to_string()),
		})
	}
}

impl Serialize for Packaging {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&self.to_string())
	}
}

impl<'de> Deserialize<'de> for Packaging {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		// FromStr is infallible so unwrap is safe.
		Ok(Packaging::from_str(&s).unwrap())
	}
}

/// Content role for an MSF track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
	/// Visual content.
	Video,
	/// Audio content.
	Audio,
	/// Audio description for visually impaired.
	AudioDescription,
	/// Textual representation of audio.
	Caption,
	/// Transcription of spoken dialogue.
	Subtitle,
	/// Visual track for hearing impaired.
	SignLanguage,
	/// Unknown role.
	Unknown(String),
}

impl fmt::Display for Role {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Role::Video => write!(f, "video"),
			Role::Audio => write!(f, "audio"),
			Role::AudioDescription => write!(f, "audiodescription"),
			Role::Caption => write!(f, "caption"),
			Role::Subtitle => write!(f, "subtitle"),
			Role::SignLanguage => write!(f, "signlanguage"),
			Role::Unknown(s) => write!(f, "{s}"),
		}
	}
}

impl FromStr for Role {
	type Err = std::convert::Infallible;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"video" => Role::Video,
			"audio" => Role::Audio,
			"audiodescription" => Role::AudioDescription,
			"caption" => Role::Caption,
			"subtitle" => Role::Subtitle,
			"signlanguage" => Role::SignLanguage,
			other => Role::Unknown(other.to_string()),
		})
	}
}

impl Serialize for Role {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&self.to_string())
	}
}

impl<'de> Deserialize<'de> for Role {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		// FromStr is infallible so unwrap is safe.
		Ok(Role::from_str(&s).unwrap())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn serialize_video_track() {
		let catalog = Catalog {
			version: 1,
			tracks: vec![Track {
				name: "video0".to_string(),
				packaging: Packaging::Legacy,
				is_live: true,
				role: Some(Role::Video),
				codec: Some("avc3.64001f".to_string()),
				width: Some(1280),
				height: Some(720),
				framerate: Some(30.0),
				samplerate: None,
				channel_config: None,
				bitrate: Some(6_000_000),
				init_data: None,
				render_group: Some(1),
				alt_group: None,
			}],
		};

		let json = catalog.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);

		// Verify audio fields are not present in JSON.
		let value: serde_json::Value = serde_json::from_str(&json).unwrap();
		let track = &value["tracks"][0];
		assert!(track.get("samplerate").is_none());
		assert!(track.get("channelConfig").is_none());
	}

	#[test]
	fn serialize_audio_track() {
		let catalog = Catalog {
			version: 1,
			tracks: vec![Track {
				name: "audio0".to_string(),
				packaging: Packaging::Legacy,
				is_live: true,
				role: Some(Role::Audio),
				codec: Some("opus".to_string()),
				width: None,
				height: None,
				framerate: None,
				samplerate: Some(48_000),
				channel_config: Some("2".to_string()),
				bitrate: Some(128_000),
				init_data: None,
				render_group: Some(1),
				alt_group: None,
			}],
		};

		let json = catalog.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);

		// Verify video fields are not present in JSON.
		let value: serde_json::Value = serde_json::from_str(&json).unwrap();
		let track = &value["tracks"][0];
		assert!(track.get("width").is_none());
		assert!(track.get("height").is_none());
		assert!(track.get("framerate").is_none());
	}

	#[test]
	fn packaging_roundtrip() {
		for (s, expected) in [
			("loc", Packaging::Loc),
			("cmaf", Packaging::Cmaf),
			("legacy", Packaging::Legacy),
			("mediatimeline", Packaging::MediaTimeline),
			("eventtimeline", Packaging::EventTimeline),
			("custom", Packaging::Unknown("custom".to_string())),
		] {
			let packaging: Packaging = s.parse().unwrap();
			assert_eq!(packaging, expected);
			assert_eq!(packaging.to_string(), s);
		}
	}

	#[test]
	fn role_roundtrip() {
		for (s, expected) in [
			("video", Role::Video),
			("audio", Role::Audio),
			("audiodescription", Role::AudioDescription),
			("caption", Role::Caption),
			("subtitle", Role::Subtitle),
			("signlanguage", Role::SignLanguage),
			("custom", Role::Unknown("custom".to_string())),
		] {
			let role: Role = s.parse().unwrap();
			assert_eq!(role, expected);
			assert_eq!(role.to_string(), s);
		}
	}

	#[test]
	fn roundtrip_empty() {
		let catalog = Catalog {
			version: 1,
			tracks: vec![],
		};
		let json = catalog.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);
	}

	#[test]
	fn cmaf_packaging() {
		let catalog = Catalog {
			version: 1,
			tracks: vec![Track {
				name: "hd".to_string(),
				packaging: Packaging::Cmaf,
				is_live: true,
				role: Some(Role::Video),
				codec: Some("avc1.640028".to_string()),
				width: Some(1920),
				height: Some(1080),
				framerate: Some(30.0),
				samplerate: None,
				channel_config: None,
				bitrate: Some(5_000_000),
				init_data: Some("AQID".to_string()),
				render_group: Some(1),
				alt_group: Some(1),
			}],
		};

		let json = catalog.to_string().unwrap();
		assert!(json.contains("\"packaging\":\"cmaf\""));
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);
	}
}

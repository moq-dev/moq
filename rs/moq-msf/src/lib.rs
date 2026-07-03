//! MSF (MOQT Streaming Format) catalog types.
//!
//! This crate provides types for the MSF catalog format as defined in
//! draft-ietf-moq-msf-01, with additional support for CMAF packaging
//! from draft-ietf-moq-cmsf-00.
//!
//! [`Catalog`] is a version-agnostic snapshot of tracks. The wire details are
//! hidden behind (de)serialization: parsing accepts both draft-00 (numeric
//! `version`, inline `initData`) and draft-01 (string `version`, with init data
//! held in a root `initDataList` and referenced per-track by `initRef`).
//! Serializing always emits the newest draft, and init data is resolved to
//! inline [`Track::init_data`] either way, so callers never touch the version
//! or the init-data indirection. draft-00's `generatedAt` and `isComplete`
//! fields stay available as version-neutral catalog fields.
//!
//! References:
//! - <https://www.ietf.org/archive/id/draft-ietf-moq-msf-01.txt>
//! - <https://www.ietf.org/archive/id/draft-ietf-moq-cmsf-00.txt>

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::DurationMilliSeconds;

/// The default track name for the MSF catalog.
pub const DEFAULT_NAME: &str = "catalog";

/// A snapshot of an MSF catalog: the tracks currently in a broadcast.
///
/// This is a version-agnostic view. The on-wire details (the catalog `version`
/// field, and draft-01's `initDataList`/`initRef` indirection for initialization
/// data) are handled during (de)serialization, so callers only ever see
/// resolved tracks with inline [`Track::init_data`]. Parsing accepts both
/// draft-00 and draft-01 catalogs; serializing always emits the newest draft.
///
/// Marked `#[non_exhaustive]` because the MSF drafts continue to grow optional
/// root fields. External callers build a catalog with [`Catalog::new`] or
/// [`Catalog::default`] and then assign whichever optional fields they need.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Catalog {
	/// Catalog generation time as Unix epoch milliseconds.
	pub generated_at: Option<u64>,

	/// Whether the broadcast has finished and no more tracks will appear.
	pub is_complete: bool,

	/// The tracks in this catalog snapshot.
	pub tracks: Vec<Track>,
}

/// A single track in the MSF catalog.
///
/// Marked `#[non_exhaustive]` because the CMSF/MSF drafts continue to grow
/// optional fields. External callers build a track with [`Track::new`] and
/// then assign whichever optional fields they need; struct-literal
/// construction (with or without `..base`) is not available outside this
/// crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Track {
	/// Unique track name (case-sensitive).
	pub name: String,

	/// Packaging mode.
	pub packaging: Packaging,

	/// Whether new objects will be appended.
	///
	/// draft-00 marks this required, but its own examples omit it on
	/// `mediatimeline`/`eventtimeline` tracks, so we default to `false` when
	/// absent rather than reject the whole catalog.
	#[serde(default)]
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

	/// Resolved base64 initialization data.
	///
	/// On the wire this is carried indirectly through draft-01's `initDataList` +
	/// `initRef`; [`Catalog`] (de)serialization resolves it so callers always see
	/// the inline payload here. draft-00's inline `initData` is also accepted.
	pub init_data: Option<String>,

	/// Wire-only pointer into the catalog's `initDataList` (draft-01). Populated
	/// only while (de)serializing; resolved into `init_data` on parse and never
	/// surfaced to callers.
	init_ref: Option<String>,

	/// Render group for synchronized playback.
	pub render_group: Option<u32>,

	/// Alternate group for quality switching.
	pub alt_group: Option<u32>,

	/// Maximum SAP starting type for groups (CMSF 3.5.2).
	/// A value of 1 means every group starts with a closed-GOP IDR.
	// Explicit rename to lock the wire name independent of rename_all.
	#[serde(rename = "maxGrpSapStartingType")]
	pub max_grp_sap_starting_type: Option<u8>,

	/// Maximum SAP starting type for objects (CMSF 3.5.2).
	/// A value of 1 means every object starts with a closed-GOP IDR.
	// Explicit rename to lock the wire name independent of rename_all.
	#[serde(rename = "maxObjSapStartingType")]
	pub max_obj_sap_starting_type: Option<u8>,

	/// Jitter (non-standard extension; not in the MSF/CMSF drafts).
	///
	/// Serialized as a JSON integer number of milliseconds, matching the hang
	/// catalog. Sub-ms precision isn't meaningful for jitter.
	#[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
	pub jitter: Option<Duration>,
}

impl Catalog {
	/// Construct a catalog with tracks and no root-level optional fields.
	pub fn new(tracks: Vec<Track>) -> Self {
		Self {
			generated_at: None,
			is_complete: false,
			tracks,
		}
	}

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

/// The newest MSF draft string this crate emits.
const CURRENT_VERSION: &str = "draft-01";

impl Serialize for Catalog {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		use std::collections::HashMap;

		// Hoist inline init payloads into a shared, deduplicated initDataList and
		// point each track at its entry via initRef. That's the draft-01 wire
		// shape; identical payloads across tracks collapse to one entry.
		let mut init_data_list: Vec<InitData> = Vec::new();
		let mut ids: HashMap<String, String> = HashMap::new();
		let mut tracks = Vec::with_capacity(self.tracks.len());

		for track in &self.tracks {
			let mut track = track.clone();
			if let Some(payload) = track.init_data.take() {
				let id = if let Some(id) = ids.get(&payload) {
					id.clone()
				} else {
					let id = format!("init{}", init_data_list.len());
					init_data_list.push(InitData {
						id: id.clone(),
						kind: "inline".to_string(),
						data: payload.clone(),
					});
					ids.insert(payload, id.clone());
					id
				};
				track.init_ref = Some(id);
			}
			tracks.push(track);
		}

		Wire {
			version: WireVersion,
			generated_at: self.generated_at,
			is_complete: self.is_complete,
			tracks,
			init_data_list: (!init_data_list.is_empty()).then_some(init_data_list),
		}
		.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Catalog {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		use std::collections::HashMap;

		let wire = Wire::deserialize(deserializer)?;
		let init_data_list = wire.init_data_list.unwrap_or_default();

		// id -> init data entry, built once so resolution is linear in the number
		// of tracks rather than tracks x entries.
		let by_id: HashMap<&str, &InitData> = init_data_list.iter().map(|e| (e.id.as_str(), e)).collect();

		let tracks = wire
			.tracks
			.into_iter()
			.map(|mut track| -> Result<Track, D::Error> {
				// Resolve draft-01 initRef into inline init_data so callers never
				// see the indirection. Inline init_data (draft-00) is kept as-is,
				// but a catalog that carries both forms must not disagree.
				if let Some(id) = track.init_ref.take() {
					let entry = by_id.get(id.as_str()).ok_or_else(|| {
						serde::de::Error::custom(format!(
							"MSF track {:?} references missing initData {:?}",
							track.name, id
						))
					})?;

					if entry.kind != "inline" {
						return Err(serde::de::Error::custom(format!(
							"MSF track {:?} references initData {:?} with unsupported type {:?}",
							track.name, id, entry.kind
						)));
					}

					match &track.init_data {
						Some(inline) if inline != &entry.data => {
							return Err(serde::de::Error::custom(format!(
								"MSF track {:?} carries conflicting initData and initRef {:?}",
								track.name, id
							)));
						}
						Some(_) => {}
						None => track.init_data = Some(entry.data.clone()),
					}
				}
				track.init_ref = None;
				Ok(track)
			})
			.collect::<Result<Vec<_>, _>>()?;

		Ok(Catalog {
			generated_at: wire.generated_at,
			is_complete: wire.is_complete,
			tracks,
		})
	}
}

fn is_false(value: &bool) -> bool {
	!*value
}

/// The on-wire catalog shape, carrying the bits [`Catalog`] hides from callers.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
	version: WireVersion,
	generated_at: Option<u64>,
	#[serde(default, skip_serializing_if = "is_false")]
	is_complete: bool,
	#[serde(default)]
	tracks: Vec<Track>,
	init_data_list: Option<Vec<InitData>>,
}

/// Wire encoding of the catalog version. Deserialization accepts draft-00's
/// number `1` or any draft-01 `"draft-XX"` string; serialization always emits
/// [`CURRENT_VERSION`], so callers never deal with the version on the wire.
struct WireVersion;

impl Serialize for WireVersion {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(CURRENT_VERSION)
	}
}

impl<'de> Deserialize<'de> for WireVersion {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct VersionVisitor;

		impl serde::de::Visitor<'_> for VersionVisitor {
			type Value = WireVersion;

			fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
				f.write_str("the JSON number 1 (draft-00) or a \"draft-XX\" version string")
			}

			// draft-00's only defined numeric version is 1. Accept it from any JSON
			// number type (serde_json picks u64/i64/f64 by shape, and `1.0` is a
			// valid spelling), and reject everything else.
			fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<WireVersion, E> {
				match v {
					1 => Ok(WireVersion),
					other => Err(E::custom(format!("unsupported MSF catalog version: {other}"))),
				}
			}

			fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<WireVersion, E> {
				if v == 1 {
					Ok(WireVersion)
				} else {
					Err(E::custom(format!("unsupported MSF catalog version: {v}")))
				}
			}

			fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<WireVersion, E> {
				if v == 1.0 {
					Ok(WireVersion)
				} else {
					Err(E::custom(format!("unsupported MSF catalog version: {v}")))
				}
			}

			fn visit_str<E: serde::de::Error>(self, _v: &str) -> Result<WireVersion, E> {
				// Any draft string is accepted; we always re-emit the current draft.
				Ok(WireVersion)
			}
		}

		deserializer.deserialize_any(VersionVisitor)
	}
}

/// An entry in the wire `initDataList`, referenced by a track's `initRef`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitData {
	/// Identifier, unique within the catalog, that a track's `initRef` points at.
	id: String,
	/// Reference type. draft-01 defines only `"inline"` (base64 payload in `data`).
	#[serde(rename = "type")]
	kind: String,
	/// The init payload, interpreted per `kind`. For `"inline"`, base64.
	data: String,
}

impl Track {
	/// Construct a track with the required identity fields set and every
	/// optional field cleared. Fields are `pub`, so callers set whatever they
	/// need by assignment afterwards.
	///
	/// This is the only path external crates have to build a `Track` since the
	/// type is `#[non_exhaustive]`.
	pub fn new(name: impl Into<String>, packaging: Packaging) -> Self {
		Self {
			name: name.into(),
			packaging,
			is_live: false,
			role: None,
			codec: None,
			width: None,
			height: None,
			framerate: None,
			samplerate: None,
			channel_config: None,
			bitrate: None,
			init_data: None,
			init_ref: None,
			render_group: None,
			alt_group: None,
			max_grp_sap_starting_type: None,
			max_obj_sap_starting_type: None,
			jitter: None,
		}
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

	fn video_track() -> Track {
		Track {
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
			init_ref: None,
			render_group: Some(1),
			alt_group: None,
			max_grp_sap_starting_type: None,
			max_obj_sap_starting_type: None,
			jitter: None,
		}
	}

	fn audio_track() -> Track {
		Track {
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
			init_ref: None,
			render_group: Some(1),
			alt_group: None,
			max_grp_sap_starting_type: None,
			max_obj_sap_starting_type: None,
			jitter: None,
		}
	}

	fn track_with_sap_and_jitter() -> Track {
		Track {
			name: "video0".to_string(),
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
			init_data: None,
			init_ref: None,
			render_group: Some(1),
			alt_group: None,
			max_grp_sap_starting_type: Some(1),
			max_obj_sap_starting_type: Some(2),
			jitter: Some(Duration::from_millis(15)),
		}
	}

	#[test]
	fn serialize_video_track() {
		let catalog = Catalog::new(vec![video_track()]);

		let json = catalog.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);

		// Verify audio fields are not present in JSON.
		let value: serde_json::Value = serde_json::from_str(&json).unwrap();
		let track = &value["tracks"][0];
		assert!(track.get("samplerate").is_none());
		assert!(track.get("channelConfig").is_none());

		// Verify skip_serializing_none omits the new optional fields when None.
		assert!(track.get("maxGrpSapStartingType").is_none());
		assert!(track.get("maxObjSapStartingType").is_none());
		assert!(track.get("jitter").is_none());
	}

	#[test]
	fn serialize_audio_track() {
		let catalog = Catalog::new(vec![audio_track()]);

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
		let catalog = Catalog::new(vec![]);
		let json = catalog.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);
	}

	#[test]
	fn cmaf_packaging() {
		let mut track = track_with_sap_and_jitter();
		track.name = "hd".to_string();
		track.alt_group = Some(1);
		track.max_grp_sap_starting_type = None;
		track.max_obj_sap_starting_type = None;
		track.jitter = None;
		track.init_data = Some("AQID".to_string());

		let catalog = Catalog::new(vec![track]);

		let json = catalog.to_string().unwrap();
		assert!(json.contains("\"packaging\":\"cmaf\""));
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(catalog, parsed);
		assert_eq!(parsed.tracks[0].init_data.as_deref(), Some("AQID"));
	}

	#[test]
	fn serialize_sap_fields() {
		let catalog = Catalog::new(vec![track_with_sap_and_jitter()]);

		let json = catalog.to_string().unwrap();

		// Verify wire-format field names use the explicit camelCase renames and the
		// auto-renamed jitter field.
		let value: serde_json::Value = serde_json::from_str(&json).unwrap();
		let track = &value["tracks"][0];
		assert_eq!(track.get("maxGrpSapStartingType"), Some(&serde_json::json!(1)));
		assert_eq!(track.get("maxObjSapStartingType"), Some(&serde_json::json!(2)));
		assert_eq!(track.get("jitter"), Some(&serde_json::json!(15)));

		// Snake-case names must NOT appear on the wire.
		assert!(track.get("max_grp_sap_starting_type").is_none());
		assert!(track.get("max_obj_sap_starting_type").is_none());
	}

	#[test]
	fn deserialize_without_sap_fields() {
		// Backward compatibility: catalogs produced before SAP/jitter were added
		// must still deserialize, with the new fields defaulting to None.
		let json = r#"{
			"version": 1,
			"tracks": [{
				"name": "video0",
				"packaging": "cmaf",
				"isLive": true,
				"role": "video",
				"codec": "avc1.640028",
				"width": 1920,
				"height": 1080,
				"framerate": 30.0,
				"bitrate": 5000000,
				"renderGroup": 1
			}]
		}"#;

		let catalog = Catalog::from_str(json).unwrap();
		let track = &catalog.tracks[0];
		assert_eq!(track.max_grp_sap_starting_type, None);
		assert_eq!(track.max_obj_sap_starting_type, None);
		assert_eq!(track.jitter, None);
	}

	#[test]
	fn sap_and_jitter_roundtrip() {
		let original = Catalog::new(vec![track_with_sap_and_jitter()]);

		let json = original.to_string().unwrap();
		let parsed = Catalog::from_str(&json).unwrap();
		assert_eq!(original, parsed);
		assert_eq!(parsed.tracks[0].max_grp_sap_starting_type, Some(1));
		assert_eq!(parsed.tracks[0].max_obj_sap_starting_type, Some(2));
		assert_eq!(parsed.tracks[0].jitter, Some(Duration::from_millis(15)));
	}

	#[test]
	fn serialize_emits_draft01_version() {
		// Callers never set a version; we always emit the newest draft string.
		let json = Catalog::default().to_string().unwrap();
		let value: serde_json::Value = serde_json::from_str(&json).unwrap();
		assert_eq!(value["version"], serde_json::json!("draft-01"));
	}

	#[test]
	fn draft00_numeric_version_decodes_and_normalizes() {
		// draft-00 put the JSON number 1 in `version`. It must decode, and on
		// re-serialize we normalize to the current draft string.
		let catalog = Catalog::from_str(r#"{"version":1,"tracks":[]}"#).unwrap();
		assert!(catalog.tracks.is_empty());

		let value: serde_json::Value = serde_json::from_str(&catalog.to_string().unwrap()).unwrap();
		assert_eq!(value["version"], serde_json::json!("draft-01"));
	}

	#[test]
	fn draft01_string_version_decodes() {
		let catalog = Catalog::from_str(r#"{"version":"draft-01","tracks":[]}"#).unwrap();
		assert!(catalog.tracks.is_empty());
	}

	#[test]
	fn unknown_version_string_is_accepted() {
		// A future draft we don't specifically recognize still decodes; we don't
		// expose the version, so callers are unaffected.
		assert!(Catalog::from_str(r#"{"version":"draft-99","tracks":[]}"#).is_ok());
	}

	#[test]
	fn unsupported_numeric_version_errors() {
		// Numbers other than 1 never had a defined meaning, so reject them.
		assert!(Catalog::from_str(r#"{"version":2,"tracks":[]}"#).is_err());
	}

	#[test]
	fn float_numeric_version_is_accepted() {
		// `1.0` is a valid JSON spelling of the draft-00 version; accept it so we
		// don't reject a catalog the JS decoder would happily parse.
		assert!(Catalog::from_str(r#"{"version":1.0,"tracks":[]}"#).is_ok());
		assert!(Catalog::from_str(r#"{"version":2.0,"tracks":[]}"#).is_err());
	}

	#[test]
	fn dangling_init_ref_errors() {
		// A dangling initRef would otherwise look like a track with no init_data.
		// Reject it so publishers do not silently lose decoder setup bytes.
		let json = r#"{
			"version": "draft-01",
			"initDataList": [
				{ "id": "v0", "type": "inline", "data": "AQID" }
			],
			"tracks": [
				{ "name": "a", "packaging": "cmaf", "isLive": true, "role": "video",
				  "codec": "avc1.640028", "initRef": "missing" }
			]
		}"#;

		let err = Catalog::from_str(json).expect_err("dangling initRef must fail");
		assert!(err.to_string().contains("missing initData"), "unexpected error: {err}");
	}

	#[test]
	fn unsupported_init_ref_type_errors() {
		// Only inline init data can be resolved into Track::init_data. Other
		// reference types must not become indistinguishable from absent init data.
		let json = r#"{
			"version": "draft-01",
			"initDataList": [
				{ "id": "v0", "type": "url", "data": "https://example.com/init" }
			],
			"tracks": [
				{ "name": "video0", "packaging": "cmaf", "isLive": true, "role": "video",
				  "codec": "avc1.640028", "initRef": "v0" }
			]
		}"#;

		let err = Catalog::from_str(json).expect_err("unsupported initRef type must fail");
		assert!(err.to_string().contains("unsupported type"), "unexpected error: {err}");
	}

	#[test]
	fn inline_init_data_and_init_ref_must_agree() {
		let json = r#"{
			"version": "draft-01",
			"initDataList": [
				{ "id": "v0", "type": "inline", "data": "AQID" }
			],
			"tracks": [
				{ "name": "video0", "packaging": "cmaf", "isLive": true, "role": "video",
				  "codec": "avc1.640028", "initData": "BAUG", "initRef": "v0" }
			]
		}"#;

		let err = Catalog::from_str(json).expect_err("conflicting initData/initRef must fail");
		assert!(err.to_string().contains("conflicting"), "unexpected error: {err}");
	}

	#[test]
	fn matching_inline_init_data_and_init_ref_decodes() {
		let json = r#"{
			"version": "draft-01",
			"initDataList": [
				{ "id": "v0", "type": "inline", "data": "AQID" }
			],
			"tracks": [
				{ "name": "video0", "packaging": "cmaf", "isLive": true, "role": "video",
				  "codec": "avc1.640028", "initData": "AQID", "initRef": "v0" }
			]
		}"#;

		let catalog = Catalog::from_str(json).unwrap();
		assert_eq!(catalog.tracks[0].init_data.as_deref(), Some("AQID"));
	}

	#[test]
	fn draft01_init_ref_resolves_to_inline() {
		// draft-01 carries init data in a root initDataList; tracks reference it by
		// id via initRef. Parsing must resolve that into inline init_data.
		let json = r#"{
			"version": "draft-01",
			"initDataList": [
				{ "id": "v0", "type": "inline", "data": "AQID" }
			],
			"tracks": [
				{ "name": "video0", "packaging": "cmaf", "isLive": true, "role": "video",
				  "codec": "avc1.640028", "initRef": "v0" }
			]
		}"#;

		let catalog = Catalog::from_str(json).unwrap();
		assert_eq!(catalog.tracks[0].init_data.as_deref(), Some("AQID"));
	}

	#[test]
	fn serialize_hoists_and_dedups_init_data() {
		// Two tracks sharing the same init payload must collapse to a single
		// initDataList entry, with both tracks referencing it via initRef and no
		// inline initData left on the tracks.
		let mut a = video_track();
		a.name = "a".to_string();
		a.init_data = Some("AQID".to_string());
		let mut b = video_track();
		b.name = "b".to_string();
		b.init_data = Some("AQID".to_string());

		let catalog = Catalog::new(vec![a, b]);
		let value: serde_json::Value = serde_json::from_str(&catalog.to_string().unwrap()).unwrap();

		let list = value["initDataList"].as_array().expect("initDataList present");
		assert_eq!(list.len(), 1, "identical payloads should dedup to one entry");
		assert_eq!(list[0]["data"], serde_json::json!("AQID"));
		assert_eq!(list[0]["type"], serde_json::json!("inline"));

		let id = list[0]["id"].as_str().unwrap();
		for t in value["tracks"].as_array().unwrap() {
			assert_eq!(t["initRef"], serde_json::json!(id));
			assert!(t.get("initData").is_none(), "no inline initData on the wire");
		}

		// And it round-trips back to inline init_data for both tracks.
		let parsed = Catalog::from_str(&catalog.to_string().unwrap()).unwrap();
		assert_eq!(parsed.tracks[0].init_data.as_deref(), Some("AQID"));
		assert_eq!(parsed.tracks[1].init_data.as_deref(), Some("AQID"));
	}

	#[test]
	fn draft00_example_av_decodes() {
		// Example 1 from draft-ietf-moq-msf-00: time-aligned audio/video. Exercises the
		// numeric version, integer framerate into an f64 field, preserved generatedAt,
		// and unmodeled fields (namespace, targetLatency) which must be ignored.
		let json = r#"{
			"version": 1,
			"generatedAt": 1746104606044,
			"tracks": [
				{
					"name": "1080p-video",
					"namespace": "conference.example.com/conference123/alice",
					"packaging": "loc",
					"isLive": true,
					"targetLatency": 2000,
					"role": "video",
					"renderGroup": 1,
					"codec": "av01.0.08M.10.0.110.09",
					"width": 1920,
					"height": 1080,
					"framerate": 30,
					"bitrate": 1500000
				},
				{
					"name": "audio",
					"namespace": "conference.example.com/conference123/alice",
					"packaging": "loc",
					"isLive": true,
					"targetLatency": 2000,
					"role": "audio",
					"codec": "opus",
					"samplerate": 48000,
					"channelConfig": "2",
					"bitrate": 32000
				}
			]
		}"#;

		let catalog = Catalog::from_str(json).expect("draft-00 AV catalog must decode");
		assert_eq!(catalog.generated_at, Some(1746104606044));
		assert_eq!(catalog.tracks.len(), 2);
		assert_eq!(catalog.tracks[0].framerate, Some(30.0));
		assert_eq!(catalog.tracks[1].channel_config.as_deref(), Some("2"));
	}

	#[test]
	fn draft00_example_timeline_tracks_decode() {
		// Example 8 from draft-ietf-moq-msf-00: mediatimeline/eventtimeline tracks omit
		// isLive/role/codec entirely. The whole catalog must still decode.
		let json = r#"{
			"version": 1,
			"generatedAt": 1746104606044,
			"tracks": [
				{
					"name": "history",
					"namespace": "conference.example.com/conference123/alice",
					"packaging": "mediatimeline",
					"mimetype": "application/json",
					"depends": ["1080p-video", "audio"]
				},
				{
					"name": "1080p-video",
					"namespace": "conference.example.com/conference123/alice",
					"packaging": "loc",
					"isLive": true,
					"role": "video",
					"codec": "av01.0.08M.10.0.110.09",
					"width": 1920,
					"height": 1080,
					"framerate": 30,
					"bitrate": 1500000
				}
			]
		}"#;

		let catalog = Catalog::from_str(json).expect("draft-00 timeline catalog must decode");
		assert_eq!(catalog.tracks.len(), 2);
		// The timeline track had no isLive; it must default rather than fail the parse.
		assert!(!catalog.tracks[0].is_live);
		assert_eq!(catalog.tracks[0].packaging, Packaging::MediaTimeline);
	}

	#[test]
	fn draft00_example_complete_decodes() {
		// Example 9: terminating a live broadcast (isComplete, empty tracks).
		let json = r#"{
			"version": 1,
			"generatedAt": 1746104606044,
			"isComplete": true,
			"tracks": []
		}"#;
		let catalog = Catalog::from_str(json).expect("draft-00 completion catalog must decode");
		assert_eq!(catalog.generated_at, Some(1746104606044));
		assert!(catalog.is_complete);
		assert!(catalog.tracks.is_empty());

		let value: serde_json::Value = serde_json::from_str(&catalog.to_string().unwrap()).unwrap();
		assert_eq!(value["generatedAt"], serde_json::json!(1746104606044u64));
		assert_eq!(value["isComplete"], serde_json::json!(true));
	}

	#[test]
	fn default_catalog_omits_completion_fields() {
		let value: serde_json::Value = serde_json::from_str(&Catalog::default().to_string().unwrap()).unwrap();
		assert!(value.get("generatedAt").is_none());
		assert!(value.get("isComplete").is_none());
	}
}

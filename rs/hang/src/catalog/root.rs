//! This module contains the structs and functions for the MoQ catalog format
use crate::Result;
use crate::catalog::skipped::Probe;
use crate::catalog::{Audio, Binary, Json, PRIORITY, Text, Video};
use serde::de::DeserializeOwned;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A catalog track, created by a broadcaster to describe the tracks available in a broadcast.
///
/// The base catalog carries the media sections (`video`, `audio`, `text`), the optional `sync`
/// section (the broadcast's one wall-clock mapping and join interval), the data sections
/// (`json`, `binary`) for application tracks that aren't media, plus `stats` naming the
/// publisher's media snapshot track, `echo` soliciting viewer feedback under a relative broadcast
/// prefix, and `include` naming whole broadcasts for explicit selection.
/// Applications extend it with their own root sections (e.g. `com.example.scte35`, or moq-mux's
/// `mpegts`) through `E`. The catalog
/// does not deny unknown fields, so a base consumer ignores the extra sections and an extended
/// catalog stays wire-compatible. See the `extension_roundtrip` test.
///
/// Every section this crate defines must have its shape when present, or decoding refuses the
/// whole catalog: a section that silently read as empty would hide a publisher bug. An
/// application's own root data goes under a namespaced key (`com.example.telemetry`) instead.
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

	/// The broadcast's timeline: its clock and join interval.
	///
	/// Omitted from the wire when it holds nothing. See [`Timeline`](crate::catalog::Timeline).
	pub sync: crate::catalog::Timeline,

	/// Text (caption/subtitle) track information with multiple renditions.
	///
	/// Contains a map of text track renditions that the viewer can choose from
	/// based on their preferences (language, role). Omitted from the wire when empty, so a
	/// broadcast without captions stays byte-identical to before this section existed.
	pub text: Text,

	/// JSON tracks: application data published as live JSON documents or logs.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged.
	pub json: Json,

	/// Binary tracks: application data published as opaque payloads.
	///
	/// Omitted from the wire when empty, so a media-only catalog is unchanged.
	pub binary: Binary,

	/// The publisher's optional media snapshot track.
	pub stats: Option<crate::catalog::Stats>,

	/// The optional prefix soliciting viewer feedback broadcasts.
	pub echo: Option<crate::catalog::Echo>,

	/// Whole broadcasts listed for explicit, lazy selection, keyed by include ID.
	///
	/// Each shares this catalog's PTS timeline and root settings, including `sync.clock` and
	/// `sync.interval`; its own settings are ignored. Listing one subscribes to nothing;
	/// see [`Include`](crate::catalog::Include). Omitted from the wire when empty.
	pub include: std::collections::BTreeMap<String, crate::catalog::Include>,

	/// Application-defined root sections, flattened alongside the base catalog sections.
	///
	/// The root `version` and every section name are reserved: serializing fails if the extension
	/// names one.
	pub ext: E,

	/// The entries decoding skipped because a newer moq-hang draft defines them.
	///
	/// Filled when a catalog is decoded and never written.
	pub skipped: crate::catalog::Skipped,
}

/// The wire shape of [`Catalog`]: its sections, without the root `version`.
///
/// The root `version` is the oldest moq-hang draft that reads the catalog correctly, absent while
/// that is 3. [`Probe`] checks it before any section is decoded. No moq-hang-04 field reshapes the
/// root, so this crate never writes it; a field one entry uses raises that entry's `version`.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize)]
// Field defaults rather than a container one: a remote container default would be a `Catalog`.
#[serde(remote = "Catalog", rename_all = "camelCase")]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned + Default"))]
struct Wire<E> {
	#[serde(
		default,
		skip_serializing_if = "Video::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	video: Video,

	#[serde(
		default,
		skip_serializing_if = "Audio::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	audio: Audio,

	#[serde(
		default,
		skip_serializing_if = "crate::catalog::Timeline::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	sync: crate::catalog::Timeline,

	#[serde(
		default,
		skip_serializing_if = "Text::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	text: Text,

	#[serde(
		default,
		skip_serializing_if = "Json::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	json: Json,

	#[serde(
		default,
		skip_serializing_if = "Binary::is_empty",
		deserialize_with = "crate::catalog::deserialize_object"
	)]
	binary: Binary,

	#[serde(default, deserialize_with = "crate::catalog::deserialize_some")]
	stats: Option<crate::catalog::Stats>,

	#[serde(default, deserialize_with = "crate::catalog::deserialize_some")]
	echo: Option<crate::catalog::Echo>,

	#[serde(
		default,
		skip_serializing_if = "std::collections::BTreeMap::is_empty",
		deserialize_with = "crate::catalog::deserialize_entries",
		serialize_with = "crate::catalog::serialize_entries"
	)]
	include: std::collections::BTreeMap<String, crate::catalog::Include>,

	#[serde(flatten, serialize_with = "serialize_ext")]
	ext: E,

	// Filled from `Probe` by `Catalog::from_value`.
	#[serde(skip)]
	skipped: crate::catalog::Skipped,
}

/// The root members this specification defines: `version` and each [`Wire`] section.
const RESERVED: [&str; 10] = [
	"version", "video", "audio", "sync", "text", "json", "binary", "stats", "echo", "include",
];

/// Refuse an extension member the specification reserves. One beside a section would write a
/// duplicate key that a reader resolves to either copy, and an extension `version` would claim a
/// newer draft that a reader refuses.
fn serialize_ext<E: Serialize, S: serde::Serializer>(ext: &E, serializer: S) -> std::result::Result<S::Ok, S::Error> {
	use serde::ser::Error;

	// Inspected as a `Value` but written from `ext` itself, which keeps its member order.
	let value = serde_json::to_value(ext).map_err(S::Error::custom)?;
	if let Some(key) = RESERVED.into_iter().find(|key| value.get(key).is_some()) {
		return Err(S::Error::custom(format!(
			"catalog extension must not name the reserved member `{key}`"
		)));
	}
	ext.serialize(serializer)
}

// Exhaustive on both sides, so a `Catalog` field missing from `Wire` fails to compile.
impl<E> From<Wire<E>> for Catalog<E> {
	fn from(wire: Wire<E>) -> Self {
		let Wire {
			video,
			audio,
			sync,
			text,
			json,
			binary,
			stats,
			echo,
			include,
			ext,
			skipped,
		} = wire;
		Self {
			video,
			audio,
			sync,
			text,
			json,
			binary,
			stats,
			echo,
			include,
			ext,
			skipped,
		}
	}
}

impl<E: Serialize> Serialize for Catalog<E> {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
		Wire::serialize(self, serializer)
	}
}

impl<'de, E: DeserializeOwned + Default> Deserialize<'de> for Catalog<E> {
	/// Buffered, so every `version` is read before any section, which a newer draft may have
	/// reshaped. An entry needing a newer draft is skipped and listed in [`skipped`](Self::skipped).
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
		Self::from_value(Strict::deserialize(deserializer)?.0).map_err(serde::de::Error::custom)
	}
}

/// A JSON value that refuses an object repeating a member, which a [`Value`](serde_json::Value)
/// would silently collapse to its last. Without it, `{"version":5,"version":3}` would read as 3.
struct Strict(serde_json::Value);

impl<'de> Deserialize<'de> for Strict {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
		struct Visitor;

		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = serde_json::Value;

			fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
				f.write_str("any JSON value")
			}

			fn visit_bool<E>(self, v: bool) -> std::result::Result<Self::Value, E> {
				Ok(v.into())
			}

			fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
				Ok(v.into())
			}

			fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
				Ok(v.into())
			}

			fn visit_f64<E>(self, v: f64) -> std::result::Result<Self::Value, E> {
				Ok(serde_json::Number::from_f64(v).map_or(serde_json::Value::Null, serde_json::Value::Number))
			}

			fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
				Ok(v.into())
			}

			fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E> {
				Ok(v.into())
			}

			fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
				Ok(serde_json::Value::Null)
			}

			fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
				Ok(serde_json::Value::Null)
			}

			fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
				let mut items = Vec::new();
				while let Some(Strict(item)) = seq.next_element()? {
					items.push(item);
				}
				Ok(items.into())
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
				let mut object = serde_json::Map::new();
				while let Some((key, Strict(value))) = map.next_entry::<String, Strict>()? {
					if object.insert(key.clone(), value).is_some() {
						return Err(serde::de::Error::custom(format!("duplicate member `{key}`")));
					}
				}
				Ok(object.into())
			}
		}

		deserializer.deserialize_any(Visitor).map(Self)
	}
}

impl Catalog<()> {
	/// The default name for the catalog track.
	pub const DEFAULT_NAME: &str = "catalog.json";

	/// Track properties for creating the catalog track via
	/// [`create_track`](moq_net::broadcast::Producer::create_track) at
	/// [`DEFAULT_NAME`](Self::DEFAULT_NAME).
	///
	/// Timed in milliseconds, so snapshots share the broadcast clock with media, fresh for the media's
	/// [`MAX_AGE`](crate::container::MAX_AGE), so a reader playing behind the live edge still
	/// receives each update it holds until its timestamp, and
	/// [compressed](moq_net::track::Info::compression), since catalog JSON is small and repetitive.
	pub fn default_track_info() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(crate::container::MAX_AGE)
			.with_priority(PRIORITY.catalog)
			.with_compression(moq_net::track::Compression::Deflate)
	}

	/// The subscription preferences used for the catalog track (high priority so
	/// it preempts media tracks).
	pub fn default_subscription() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default()
			.with_ranges([moq_net::track::Range::default().with_priority(PRIORITY.catalog)])
	}
}

impl<E> Catalog<E> {
	/// Refuse a catalog with an ID naming both a video and an audio rendition, or with a
	/// rendition field out of range, such as a zero sample rate or a projection out of bounds.
	///
	/// Decoding already refuses an out-of-range rendition, so this catches a catalog built by hand.
	pub fn check_renditions(&self) -> Result<()> {
		use crate::catalog::Entry;

		if let Some(id) = self
			.video
			.renditions
			.keys()
			.find(|id| self.audio.renditions.contains_key(*id))
		{
			return Err(crate::Error::DuplicateRendition(id.clone()));
		}
		for config in self.video.renditions.values() {
			config.check()?;
		}
		for config in self.audio.renditions.values() {
			config.check()?;
		}
		Ok(())
	}

	/// Copy every section into a catalog with no application extension.
	pub fn without_ext(&self) -> Catalog<()> {
		// Destructured without `..`, so a new field fails to compile until it is carried over.
		let Catalog {
			video,
			audio,
			sync,
			text,
			json,
			binary,
			stats,
			echo,
			include,
			ext: _,
			skipped,
		} = self;
		Catalog {
			video: video.clone(),
			audio: audio.clone(),
			sync: *sync,
			text: text.clone(),
			json: json.clone(),
			binary: binary.clone(),
			stats: stats.clone(),
			echo: echo.clone(),
			include: include.clone(),
			ext: (),
			skipped: skipped.clone(),
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
	/// Subscribe to catalog updates on a broadcast, reading only the newest.
	///
	/// For a reader that applies each update on arrival; one that holds updates until their
	/// timestamp builds a [`Consumer`](super::Consumer) with its playout delay.
	#[cfg(feature = "async")]
	pub async fn subscribe(broadcast: &moq_net::broadcast::Consumer) -> Result<super::Consumer<E>> {
		Ok(super::Consumer::new(
			broadcast.track(Catalog::DEFAULT_NAME)?,
			std::time::Duration::ZERO,
		))
	}

	/// Parse a catalog from a slice of bytes.
	///
	/// Fails with [`Error::UnsupportedVersion`](crate::Error::UnsupportedVersion) if the catalog
	/// needs a newer moq-hang draft than this crate reads.
	pub fn from_slice(v: &[u8]) -> Result<Self> {
		// Probed straight from the bytes first, so an unsupported root `version` is reported before
		// any other fault; `Strict` then refuses a repeated member, which a `Value` would collapse.
		serde_json::from_slice::<Probe>(v)?.check()?;
		Self::from_value(serde_json::from_slice::<Strict>(v)?.0)
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
	/// needs a newer moq-hang draft than this crate reads. Every `version` is read before any
	/// section, which the newer draft may have reshaped; an entry needing a newer draft is
	/// skipped and reported in [`skipped`](Self::skipped).
	pub(crate) fn from_value(mut value: serde_json::Value) -> Result<Self> {
		let probe = Probe::deserialize(&value)?;
		probe.check()?;
		// Checked, so taken out of what an extension would otherwise receive.
		if let Some(root) = value.as_object_mut() {
			root.remove("version");
		}
		let mut catalog: Self = Wire::deserialize(value)?;
		catalog.skipped = probe.skipped;
		Ok(catalog)
	}
}

impl<E: Serialize> Catalog<E> {
	/// Serialize the catalog to a JSON string.
	pub fn to_json(&self) -> Result<String> {
		Ok(serde_json::to_string(self)?)
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

	#[derive(Deserialize)]
	struct ParityCase {
		name: String,
		catalog: serde_json::Value,
		#[serde(default)]
		refused: bool,
		#[serde(default)]
		video: Vec<String>,
		#[serde(default)]
		audio: Vec<String>,
		#[serde(default)]
		text: Vec<String>,
		#[serde(default)]
		json: Vec<String>,
		#[serde(default)]
		binary: Vec<String>,
		#[serde(default)]
		include: Vec<String>,
		#[serde(default)]
		skipped: BTreeMap<String, BTreeMap<String, u64>>,
	}

	/// The corpus `js/hang` also loads: both languages accept the same catalogs and keep the same
	/// rendition, data track, and include IDs, and skip the same newer entries, so a divergence
	/// fails CI on whichever side drifts.
	#[test]
	fn shared_parity_fixtures() {
		let cases: Vec<ParityCase> = serde_json::from_str(include_str!("../../fixtures/catalog-parity.json")).unwrap();
		let mut failures = Vec::new();
		for case in cases {
			let parsed = Catalog::<()>::from_str(&case.catalog.to_string()).and_then(|catalog| {
				catalog.check_renditions()?;
				Ok(catalog)
			});
			match (parsed, case.refused) {
				(Err(_), true) => {}
				(Ok(catalog), true) => failures.push(format!("{}: accepted {catalog:?}", case.name)),
				(Err(err), false) => failures.push(format!("{}: refused: {err}", case.name)),
				(Ok(catalog), false) => {
					let video: Vec<_> = catalog.video.renditions.keys().cloned().collect();
					let audio: Vec<_> = catalog.audio.renditions.keys().cloned().collect();
					let text: Vec<_> = catalog.text.renditions.keys().cloned().collect();
					let json: Vec<_> = catalog.json.tracks.keys().cloned().collect();
					let binary: Vec<_> = catalog.binary.tracks.keys().cloned().collect();
					let include: Vec<_> = catalog.include.keys().cloned().collect();
					if (&video, &audio, &text, &json, &binary, &include)
						!= (
							&case.video,
							&case.audio,
							&case.text,
							&case.json,
							&case.binary,
							&case.include,
						) {
						failures.push(format!(
							"{}: kept video {video:?} audio {audio:?} text {text:?} json {json:?} binary {binary:?} include {include:?}",
							case.name
						));
					}
					let skipped = &catalog.skipped;
					let skipped: BTreeMap<String, BTreeMap<String, u64>> = [
						("video", &skipped.video),
						("audio", &skipped.audio),
						("text", &skipped.text),
						("json", &skipped.json),
						("binary", &skipped.binary),
						("include", &skipped.include),
					]
					.into_iter()
					.filter(|(_, ids)| !ids.is_empty())
					.map(|(section, ids)| (section.to_string(), ids.clone()))
					.collect();
					if skipped != case.skipped {
						failures.push(format!("{}: skipped {skipped:?}", case.name));
					}
				}
			}
		}
		assert!(failures.is_empty(), "{}", failures.join("\n"));
	}

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

	/// An include round-trips, stays off the wire when none is listed, and a malformed entry
	/// refuses the catalog rather than being dropped.
	#[test]
	fn includes_roundtrip_and_refuse_malformed_entries() {
		let json = r#"{"include":{"alice":{"broadcast":"./alice.hang"}},"video":{"renditions":{"alice":{"codec":"avc1.64001f","container":{"kind":"legacy"}}}}}"#;
		let catalog = Catalog::<()>::from_str(json).unwrap();
		assert_eq!(
			catalog.include["alice"],
			crate::catalog::Include::new(moq_net::path::Relative::new("./alice.hang").unwrap())
		);
		// Include IDs are their own namespace, so one may share a rendition's ID.
		assert!(catalog.check_renditions().is_ok());
		assert!(catalog.video.renditions.contains_key("alice"));
		assert_eq!(Catalog::<()>::from_str(&catalog.to_json().unwrap()).unwrap(), catalog);
		assert!(!Catalog::<()>::default().to_json().unwrap().contains("include"));

		for bad in [
			r#"{"include":[]}"#,
			r#"{"include":{"alice":null}}"#,
			r#"{"include":{"alice":{}}}"#,
			r#"{"include":{"alice":{"broadcast":42}}}"#,
		] {
			assert!(Catalog::<()>::from_str(bad).is_err(), "{bad} must be refused");
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

	/// A publisher building the catalog by hand is caught before it writes a value every reader refuses.
	#[test]
	fn fields_out_of_range_are_refused() {
		use crate::catalog::{AudioLayout, Projection, VideoCodec, VideoConfig};

		let mut catalog = Catalog::<()>::default();
		let mut amb = AudioConfig::new(Opus, 48_000, 6);
		amb.layout = Some(AudioLayout::Ambisonic);
		catalog.audio.insert("amb", amb).unwrap();
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::InvalidAmbisonicChannels(6))
		));
		catalog.audio.renditions.get_mut("amb").unwrap().channel_count = 4;
		catalog.check_renditions().unwrap();

		catalog.audio.renditions.get_mut("amb").unwrap().sample_rate = 0;
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::Zero("sampleRate"))
		));
		catalog.audio.renditions.get_mut("amb").unwrap().sample_rate = 48_000;

		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.coded_width = Some(0);
		catalog.video.insert("empty", config).unwrap();
		assert!(matches!(
			catalog.check_renditions(),
			Err(crate::Error::Zero("codedWidth"))
		));
		catalog.video.remove("empty");

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
				jitter: Some(std::time::Duration::from_millis(100)),
				delay: Some(std::time::Duration::from_millis(200)),
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio".to_string(),
			AudioConfig {
				broadcast: None,
				track: None,
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
	fn rendition_with_misspelled_broadcast_is_refused() {
		// Spelled like a path, so an empty segment or a lone backslash makes the rendition malformed,
		// which refuses the catalog. Mirrors the JS `rendition broadcast reference is parsed and normalized`.
		for broadcast in ["./source/", "/source", "a//b", r"a\b"] {
			let encoded = serde_json::json!({
				"video": {"renditions": {"video": {
					"broadcast": broadcast,
					"codec": "avc1.64001f",
					"container": {"kind": "legacy"}
				}}}
			})
			.to_string();

			Catalog::<()>::from_str(&encoded).expect_err(&format!("{broadcast:?} should be refused"));
		}
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
		assert_eq!(unknown.kind(), "future");

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

	#[test]
	fn data_sections_stay_off_the_wire_when_empty() {
		// Empty sections stay off the wire, video and audio included.
		let output = Catalog::<()>::default().to_json().expect("failed to encode");
		assert_eq!(output, "{}");
	}

	#[test]
	fn sync_roundtrips_at_the_root() {
		let clock =
			crate::catalog::Clock::new(moq_net::Timestamp::from_micros(1_751_846_400_000_000).unwrap()).unwrap();
		let mut catalog = Catalog::<()>::default();
		catalog.sync.clock = Some(clock);
		catalog.sync.interval = Some(std::time::Duration::from_secs(2));

		let json = catalog.to_json().expect("failed to encode");
		assert_eq!(
			json,
			r#"{"sync":{"clock":{"wall":1751846400000000,"timescale":1000000},"interval":2000}}"#
		);
		assert_eq!(Catalog::<()>::from_str(&json).expect("failed to decode"), catalog);
	}

	/// Retired shapes are someone else's keys, ignored rather than read: the root `clock` and the
	/// `media` section that each held the clock before `sync`, and a rendition `hls` record.
	#[test]
	fn a_retired_clock_media_and_hls_field_are_ignored() {
		let rendition = serde_json::json!({"codec": "avc1.64001f", "container": {"kind": "legacy"}});
		let plain =
			Catalog::<()>::from_str(&serde_json::json!({"video": {"renditions": {"video": rendition}}}).to_string())
				.unwrap();
		let mut retired = rendition.clone();
		retired["hls"] = serde_json::json!({"track": "video.hls", "duration": 2000});
		let json = serde_json::json!({
			"video": {"renditions": {"video": retired}},
			"clock": {"wall": 1000, "timescale": 1000000},
			"media": {"clock": {"wall": 1000}, "join": {"interval": 2000}},
		});
		let catalog = Catalog::<()>::from_str(&json.to_string()).expect("retired fields must not reject media");
		assert_eq!(catalog, plain);
		assert_eq!(catalog.sync, crate::catalog::Timeline::default());
		assert_eq!(catalog.to_json().unwrap(), plain.to_json().unwrap());
	}

	#[test]
	fn packaged_clock_fixture_decodes() {
		// The canonical shape: the clock and join interval under `sync`. A reader maps media
		// timestamps through the clock after timescale conversion.
		let json = include_str!("../../fixtures/catalog-clock.json");
		let catalog = Catalog::<()>::from_str(json).expect("the packaged fixture must decode");

		let clock = catalog.sync.clock.expect("the fixture carries a clock");
		assert_eq!(clock.wall.scale().map(moq_net::Timescale::as_u64), Some(1_000_000));
		assert_eq!(catalog.sync.interval, Some(std::time::Duration::from_secs(2)));

		// PTS 2000 ms lands 2s after the wall epoch, whatever the track timescale.
		let expected = moq_net::time::EPOCH + std::time::Duration::from_millis(1_751_846_402_000);
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
			// So does plain serde, at the root and in an entry.
			assert!(serde_json::from_str::<Catalog>(json).is_err(), "{json}");
		}
		let entry = r#"{"video":{"renditions":{"hd":{"version":5,"version":3,"codec":"avc1.64001f"}}}}"#;
		assert!(Catalog::<()>::from_str(entry).is_err(), "{entry}");
		assert!(serde_json::from_str::<Catalog>(entry).is_err(), "{entry}");

		// Any repeated member, not only `version`.
		let repeated = r#"{"video":{"renditions":{"hd":{"codec":"avc1.64001f","codec":"vp8"}}}}"#;
		assert!(Catalog::<()>::from_str(repeated).is_err(), "{repeated}");
		assert!(serde_json::from_str::<Catalog>(repeated).is_err(), "{repeated}");
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

	/// An extension member the specification reserves would sit beside the base member as a
	/// duplicate key: an extension `version` could override what the catalog actually needs.
	#[test]
	fn an_extension_reserved_member_is_refused() {
		#[derive(Serialize, Default)]
		struct Ext {
			version: u64,
		}

		let catalog = Catalog {
			ext: Ext { version: 3 },
			..Default::default()
		};
		let err = catalog.to_json().unwrap_err();
		assert!(err.to_string().contains("reserved member `version`"), "{err}");

		// Every section name too, which would otherwise sit beside the section as a duplicate key.
		for key in RESERVED {
			let mut passthrough = Catalog::<serde_json::Map<String, serde_json::Value>>::default();
			passthrough.ext.insert(key.into(), serde_json::json!({}));
			let err = passthrough.to_json().unwrap_err();
			assert!(err.to_string().contains(&format!("`{key}`")), "{key}: {err}");
		}

		// Any other member still flattens beside the base sections.
		let ok = Catalog {
			ext: serde_json::json!({ "scte35": {} }),
			..Default::default()
		};
		assert_eq!(ok.to_json().unwrap(), r#"{"scte35":{}}"#);
	}

	fn h264() -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1f,
			inline: false,
		});
		config.container = Container::Legacy;
		config
	}

	/// Only a field an older reader would misread raises an entry's version; one it would merely
	/// ignore leaves the entry readable by moq-hang-03. No moq-hang-04 field raises the root's.
	#[test]
	fn the_version_is_written_only_when_needed() {
		let versions = |catalog: &Catalog| {
			let value = serde_json::to_value(catalog).unwrap();
			assert_eq!(value.get("version"), None, "the root version is never written");
			let mut versions = BTreeMap::new();
			for (section, map) in [
				("video", "/video/renditions"),
				("audio", "/audio/renditions"),
				("json", "/json/tracks"),
				("include", "/include"),
			] {
				for (id, entry) in value.pointer(map).and_then(|v| v.as_object()).into_iter().flatten() {
					if let Some(version) = entry.get("version") {
						versions.insert(format!("{section}.{id}"), version.as_u64().unwrap());
					}
				}
			}
			versions
		};

		let mut catalog = Catalog::<()>::default();
		catalog.video.renditions.insert("hd".into(), h264());
		let mut audio = AudioConfig::new(Opus, 48_000, 2);
		audio.warmup = Some(std::time::Duration::from_millis(80));
		catalog.audio.renditions.insert("en".into(), audio);
		catalog.include.insert(
			"extra".into(),
			crate::catalog::Include::new(moq_net::path::Relative::new("../extra").unwrap()),
		);
		assert_eq!(
			versions(&catalog),
			BTreeMap::new(),
			"an older reader still plays audio warmup"
		);

		// A `track` naming the ID is what an older reader subscribes to anyway.
		catalog.video.renditions.get_mut("hd").unwrap().track = Some("hd".into());
		assert_eq!(versions(&catalog), BTreeMap::new());

		catalog.video.renditions.get_mut("hd").unwrap().track = Some("video".into());
		assert_eq!(versions(&catalog), BTreeMap::from([("video.hd".into(), 4)]));
		let json = catalog.to_json().unwrap();
		assert!(json.contains(r#""hd":{"version":4,"#), "{json}");
		assert_eq!(Catalog::<()>::from_str(&json).unwrap(), catalog);

		let mut chat = JsonConfig::new(Mode::Stream);
		chat.track = Some("chat.json".into());
		catalog.json.insert("chat", chat).unwrap();
		assert_eq!(
			versions(&catalog),
			BTreeMap::from([("video.hd".into(), 4), ("json.chat".into(), 4)])
		);
	}

	/// moq-hang-04 defines `warmup` for audio only, so a video rendition's is an unknown field: read
	/// past, never written back, and no reason for a `version`.
	#[test]
	fn a_video_warmup_is_ignored() {
		let json = r#"{"video":{"renditions":{"hd":{"codec":"vp8","container":{"kind":"legacy"},"warmup":500}}}}"#;
		let catalog = Catalog::<()>::from_str(json).unwrap();
		assert_eq!(catalog.video.renditions["hd"].codec, crate::catalog::VideoCodec::VP8);
		assert_eq!(
			catalog.to_json().unwrap(),
			r#"{"video":{"renditions":{"hd":{"codec":"vp8","container":{"kind":"legacy"}}}}}"#
		);
	}

	/// Versions are computed, never carried: republishing a catalog that claimed more than it
	/// uses writes what it actually needs.
	#[test]
	fn a_republish_recomputes_the_version() {
		let json = r#"{"version":4,"video":{"renditions":{"hd":{"version":4,"codec":"avc1.64001f"}}},
			"json":{"tracks":{"chat":{"version":4,"mode":"stream"}}}}"#;
		let catalog = Catalog::<()>::from_str(json).unwrap();
		assert!(
			catalog.json.tracks["chat"].extra.is_empty(),
			"the entry version is not an extension"
		);
		assert!(!catalog.to_json().unwrap().contains("version"));
	}

	/// An entry a newer draft defines is skipped and reported, even one this reader would call
	/// malformed, while the rest of the catalog plays.
	#[test]
	fn a_newer_entry_is_skipped() {
		let json = r#"{
			"video": {"renditions": {
				"compat": {"codec": "avc1.64001f"},
				"next": {"version": 5, "codec": 42}
			}},
			"audio": {"renditions": {"next": {"version": 6}}},
			"text": {"renditions": {"next": {"version": 5}}},
			"json": {"tracks": {"next": {"version": 5}}},
			"binary": {"tracks": {"next": {"version": 5, "mode": []}}},
			"include": {"next": {"version": 5, "broadcast": 7}}
		}"#;

		let check = |catalog: Catalog| {
			assert_eq!(catalog.video.renditions.keys().collect::<Vec<_>>(), ["compat"]);
			assert!(catalog.audio.is_empty() && catalog.text.is_empty() && catalog.include.is_empty());
			assert!(catalog.json.is_empty() && catalog.binary.is_empty());

			let next = |version| BTreeMap::from([("next".to_string(), version)]);
			assert_eq!(catalog.skipped.video, next(5));
			assert_eq!(catalog.skipped.audio, next(6));
			assert_eq!(catalog.skipped.text, next(5));
			assert_eq!(catalog.skipped.json, next(5));
			assert_eq!(catalog.skipped.binary, next(5));
			assert_eq!(catalog.skipped.include, next(5));
			assert_eq!(catalog.skipped.version(), Some(6));

			// Never written, so a republish lists only what this reader read.
			let republished = Catalog::<()>::from_str(&catalog.to_json().unwrap()).unwrap();
			assert!(republished.skipped.is_empty());
			assert_eq!(republished.skipped.version(), None);
		};
		check(Catalog::from_str(json).unwrap());
		check(serde_json::from_str(json).unwrap());

		// The root version still refuses the whole catalog.
		let root = json.replacen('{', r#"{"version":5,"#, 1);
		assert!(matches!(
			Catalog::<()>::from_str(&root),
			Err(crate::Error::UnsupportedVersion(5))
		));

		// Even when the newer draft reshaped an entry's own `version`, wherever the root's sits.
		for entry in [r#"{"version":"new-shape"}"#, r#"{"version":5,"version":3}"#] {
			for json in [
				format!(r#"{{"version":5,"video":{{"renditions":{{"hd":{entry}}}}}}}"#),
				format!(r#"{{"video":{{"renditions":{{"hd":{entry}}}}},"version":5}}"#),
			] {
				assert!(
					matches!(Catalog::<()>::from_str(&json), Err(crate::Error::UnsupportedVersion(5))),
					"{json}"
				);
			}
		}
	}

	/// An entry readable by this draft is read whatever version it claims.
	#[test]
	fn a_supported_entry_version_is_read() {
		for version in [0, 3, 4] {
			let json =
				format!(r#"{{"video":{{"renditions":{{"hd":{{"version":{version},"codec":"avc1.64001f"}}}}}}}}"#);
			let catalog = Catalog::<()>::from_str(&json).unwrap();
			assert!(catalog.video.renditions.contains_key("hd"), "{json}");
			assert!(catalog.skipped.is_empty(), "{json}");
		}
	}

	/// An entry's version is refused, with the catalog, when malformed or repeated, as the root's is:
	/// otherwise a newer entry could slip past the check by also claiming an old version.
	#[test]
	fn a_malformed_entry_version_is_refused() {
		for version in [r#""5""#, "-1", "4.5", "null", "[]"] {
			let json = format!(r#"{{"json":{{"tracks":{{"chat":{{"version":{version},"mode":"stream"}}}}}}}}"#);
			assert!(Catalog::<()>::from_str(&json).is_err(), "{json}");
			assert!(serde_json::from_str::<Catalog>(&json).is_err(), "{json}");
		}

		for versions in [r#""version":5,"version":3"#, r#""version":3,"version":3"#] {
			let json = format!(r#"{{"video":{{"renditions":{{"hd":{{{versions},"codec":"avc1.64001f"}}}}}}}}"#);
			let err = Catalog::<()>::from_str(&json).unwrap_err();
			assert!(err.to_string().contains("duplicate field `version`"), "{json}: {err}");
			assert!(Catalog::<()>::from_reader(json.as_bytes()).is_err(), "{json}");
		}
	}

	/// A data track extension member named `version` would sit beside the computed one.
	#[test]
	fn an_entry_extension_version_is_refused() {
		let mut catalog = Catalog::<()>::default();
		let mut chat = JsonConfig::new(Mode::Stream);
		chat.extra.insert("version".into(), 3.into());
		catalog.json.insert("chat", chat).unwrap();
		let err = catalog.to_json().unwrap_err();
		assert!(err.to_string().contains("must not carry a member `version`"), "{err}");
	}

	/// `without_ext` clears only the extension: every other field, including those a decoder fills,
	/// survives the copy.
	#[test]
	fn without_ext_keeps_every_other_field() {
		let mut catalog = Catalog::<serde_json::Map<String, serde_json::Value>> {
			echo: Some(crate::catalog::Echo::new(moq_net::path::Relative::new("echo").unwrap())),
			stats: Some(crate::catalog::Stats::new("stats")),
			..Default::default()
		};
		catalog.ext.insert("app".into(), 1.into());
		catalog.video.renditions.insert(
			"video".into(),
			VideoConfig::new(H264 {
				profile: 0x64,
				constraints: 0,
				level: 0x28,
				inline: false,
			}),
		);
		catalog
			.audio
			.renditions
			.insert("audio".into(), AudioConfig::new(Opus, 48_000, 2));
		catalog.sync.interval = Some(std::time::Duration::from_secs(2));
		catalog.json.insert("chat", JsonConfig::new(Mode::Stream)).unwrap();
		catalog
			.binary
			.insert("blob", BinaryConfig::new(Mode::Snapshot))
			.unwrap();
		catalog.include.insert(
			"other".into(),
			crate::catalog::Include::new(moq_net::path::Relative::new("other").unwrap()),
		);
		catalog.skipped.video.insert("future".into(), 9);

		let stripped = catalog.without_ext();
		let expected = Catalog::<()> {
			video: catalog.video.clone(),
			audio: catalog.audio.clone(),
			sync: catalog.sync,
			text: catalog.text.clone(),
			json: catalog.json.clone(),
			binary: catalog.binary.clone(),
			stats: catalog.stats.clone(),
			echo: catalog.echo.clone(),
			include: catalog.include.clone(),
			ext: (),
			skipped: catalog.skipped.clone(),
		};
		assert_eq!(stripped, expected);
		assert!(!stripped.json.tracks.is_empty() && !stripped.skipped.video.is_empty());
		assert!(stripped.stats.is_some() && stripped.echo.is_some() && !stripped.include.is_empty());
	}
}

mod format;

pub use format::*;

use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;

use crate::catalog::Container;

/// Information about the text (caption/subtitle) tracks in the catalog.
///
/// This struct contains a map of renditions, typically one per language.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without
/// bumping the major version. External callers start from [`Text::default`] and
/// fill in what they need ([`insert`](Self::insert) for renditions); struct-literal
/// construction (with or without `..base`) is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Text {
	/// A map of track name to rendition configuration.
	/// This is not an array so it will work with JSON Merge Patch.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, TextConfig>,
}

impl Text {
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: TextConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return the configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<TextConfig> {
		self.renditions.remove(name)
	}

	/// True when there are no renditions, so the section can be omitted from the catalog.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}
}

/// Timed-text (caption/subtitle) track configuration.
///
/// Unlike audio and video there is no WebCodecs decoder for text: a consumer parses the cue
/// payload directly (see [`TextFormat`]) and renders it as an overlay. Each frame is one cue (or a
/// self-contained segment of cues), and the frame timestamp is the cue's start time on the shared
/// media clock.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers build a config with [`TextConfig::new`] and then assign whichever
/// optional fields they need; struct-literal construction (with or without `..base`) is not
/// available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TextConfig {
	/// Optional reference to another broadcast that publishes this track, expressed relative to the
	/// broadcast that served this catalog (e.g. `../source`). If unset, the track lives in the same
	/// broadcast as the catalog.
	#[serde(default)]
	pub broadcast: Option<moq_net::PathRelativeOwned>,

	/// The serialization format of each cue payload.
	#[serde_as(as = "DisplayFromStr")]
	pub format: TextFormat,

	/// The accessibility role of the track. Defaults to [`TextRole::Subtitle`].
	#[serde(default)]
	pub role: TextRole,

	/// The BCP-47 language tag of the track (e.g. `en`, `es-419`), if known.
	pub lang: Option<String>,

	/// A human-readable label for a track picker, when `lang` alone is ambiguous (e.g. distinguishing
	/// "English" from "English (SDH)").
	pub label: Option<String>,

	/// Container format for frame encoding. Defaults to "legacy" (a microsecond timestamp prefix
	/// followed by the cue payload).
	#[serde(default)]
	pub container: Container,

	/// The companion timeline track indexing this rendition's groups, if the publisher offers one.
	/// See [`Timeline`](crate::catalog::Timeline).
	#[serde(default)]
	pub timeline: Option<crate::catalog::Timeline>,
}

impl TextConfig {
	/// Construct a config with the required format set and every optional field cleared. `role`
	/// defaults to [`TextRole::Subtitle`] and `container` to [`Container::default`]. Fields are
	/// `pub`, so callers set whatever they need by assignment afterwards.
	///
	/// This is the only path external crates have to build a `TextConfig` since the type is
	/// `#[non_exhaustive]`.
	pub fn new(format: TextFormat) -> Self {
		Self {
			broadcast: None,
			format,
			role: TextRole::default(),
			lang: None,
			label: None,
			container: Container::default(),
			timeline: None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrip() {
		let mut encoded = r#"{
			"renditions": {
				"captions.en": {
					"format": "vtt",
					"role": "caption",
					"lang": "en",
					"label": "SDH",
					"container": {"kind": "legacy"}
				}
			}
		}"#
		.to_string();
		encoded.retain(|c| !c.is_whitespace());

		let mut config = TextConfig::new(TextFormat::Vtt);
		config.role = TextRole::Caption;
		config.lang = Some("en".to_string());
		config.label = Some("SDH".to_string());
		config.container = Container::Legacy;

		let mut text = Text::default();
		text.insert("captions.en", config).unwrap();

		let decoded: Text = serde_json::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, text, "wrong decoded output");

		let output = serde_json::to_string(&text).expect("failed to encode");
		assert_eq!(encoded, output, "wrong encoded output");
	}

	#[test]
	fn optionals_omitted() {
		// A minimal config omits the optional lang/label/broadcast so the wire stays small, while
		// role (like container) always serializes and round-trips.
		let mut text = Text::default();
		text.insert("subs", TextConfig::new(TextFormat::Vtt)).unwrap();

		let output = serde_json::to_string(&text).unwrap();
		assert!(!output.contains("lang"), "empty lang leaked: {output}");
		assert!(!output.contains("label"), "empty label leaked: {output}");
		assert!(!output.contains("broadcast"), "empty broadcast leaked: {output}");

		let decoded: Text = serde_json::from_str(&output).unwrap();
		assert_eq!(decoded.renditions["subs"].role, TextRole::Subtitle);
	}
}

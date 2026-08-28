use std::ops::{Deref, DerefMut};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// An application's catalog extension: a plain serde struct of extra root sections that are
/// serialized as a flat union with the base media sections.
///
/// Implement it (no methods) on a struct of your own sections, then publish/consume a
/// [`Catalog<YourExt>`]:
///
/// ```
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct Scte35Ext {
///     #[serde(skip_serializing_if = "Option::is_none")]
///     scte35: Option<Scte35>,
/// }
///
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct Scte35 {
///     splice_id: u32,
/// }
///
/// impl moq_mux::catalog::hang::CatalogExt for Scte35Ext {}
/// ```
///
/// The unit type `()` is the no-extension case, so [`Catalog<()>`] is just the base media catalog.
pub trait CatalogExt: Serialize + DeserializeOwned + Default + Clone + Send + Unpin + 'static {}

impl CatalogExt for () {}

/// The untyped catalog extension: arbitrary top-level JSON sections beyond the base
/// `video`/`audio`/`text` media sections and shared `timeline`, captured and republished verbatim.
///
/// This is the extension a caller reaches for when the section names aren't known at
/// compile time, e.g. across the FFI/C boundary where a typed [`CatalogExt`] struct can't
/// cross. Publish/consume a [`Catalog<Extra>`] and use [`set`](Self::set)/[`get`](Self::get).
/// The default extension stays `()` (unknown sections dropped); opt into `Extra` explicitly.
///
/// `video`, `audio`, `text`, `timeline`, `json`, and `binary` are reserved for the base sections,
/// so [`set`](Self::set) rejects them to keep the wire JSON free of duplicate keys.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(transparent)]
pub struct Extra(serde_json::Map<String, serde_json::Value>);

impl CatalogExt for Extra {}

impl Extra {
	/// Look up a section by name.
	pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
		self.0.get(name)
	}

	/// Iterate over every section as `(name, value)` pairs, sorted by name.
	pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
		self.0.iter()
	}

	/// The number of sections.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether there are no sections.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Set (or replace) a section. Errors if `name` collides with a reserved base
	/// section (`video`/`audio`/`text`/`timeline`/`json`/`binary`).
	pub fn set(&mut self, name: impl Into<String>, value: serde_json::Value) -> crate::Result<()> {
		let name = name.into();
		if matches!(
			name.as_str(),
			"video" | "audio" | "text" | "timeline" | "json" | "binary"
		) {
			return Err(crate::Error::ReservedSection(name));
		}
		self.0.insert(name, value);
		Ok(())
	}

	/// Remove a section, returning its previous value if present.
	pub fn remove(&mut self, name: &str) -> Option<serde_json::Value> {
		self.0.remove(name)
	}
}

/// The base sections plus an application extension `E` (defaulting to `()` for none), serialized
/// as a flat union: the `video`/`audio`/`text` media sections, the shared `timeline`, the
/// `json`/`binary` data sections, and the extension's sections share one JSON object on the wire.
///
/// The data sections (`json`/`binary`) carry application tracks that aren't media. Every base
/// section is a direct field (`catalog.video`), and the catalog derefs to the extension so its
/// sections are reachable directly too (`catalog.scte35`, or `catalog.ext.scte35` explicitly). A
/// consumer reading a different extension (or none) ignores sections it doesn't know.
///
/// Marked `#[non_exhaustive]` so a future base section can be added without breaking callers, which
/// is what [`hang::catalog::Catalog`] already does. Build one with
/// [`default`](Default::default) and set the fields you need.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned"))]
#[non_exhaustive]
pub struct Catalog<E: CatalogExt = ()> {
	#[serde(default)]
	pub video: hang::catalog::Video,

	#[serde(default)]
	pub audio: hang::catalog::Audio,

	/// The broadcast's timeline track (its aligned segment index), if the publisher offers one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeline: Option<hang::catalog::Timeline>,

	/// Caption/subtitle renditions. Omitted from the wire when empty, so a broadcast without
	/// captions stays byte-identical to before this section existed.
	///
	/// Decoded leniently: an application could carry its own `text` section through [`Extra`] before
	/// this one was reserved, and that must not take the whole catalog down.
	#[serde(
		default,
		skip_serializing_if = "hang::catalog::Text::is_empty",
		deserialize_with = "hang::catalog::deserialize_text"
	)]
	pub text: hang::catalog::Text,

	/// JSON tracks: application data published as live JSON documents or logs. Omitted from the
	/// wire when empty, so a media-only catalog is unchanged.
	///
	/// Decoded leniently for the same reason as [`text`](Self::text): `json` is a generic enough
	/// key that an application could have been carrying its own through [`Extra`] before this one
	/// was reserved.
	#[serde(
		default,
		skip_serializing_if = "hang::catalog::Json::is_empty",
		deserialize_with = "hang::catalog::deserialize_section"
	)]
	pub json: hang::catalog::Json,

	/// Binary tracks: application data published as opaque payloads. Omitted from the wire when
	/// empty, so a media-only catalog is unchanged. Decoded leniently for the same reason as
	/// [`json`](Self::json).
	#[serde(
		default,
		skip_serializing_if = "hang::catalog::Binary::is_empty",
		deserialize_with = "hang::catalog::deserialize_section"
	)]
	pub binary: hang::catalog::Binary,

	#[serde(flatten)]
	pub ext: E,
}

impl<E: CatalogExt> Catalog<E> {
	/// The JSON track named `name`, or `None` if the catalog doesn't list one.
	///
	/// The returned [`Entry`](crate::catalog::Entry) carries the name along with the config, so
	/// reading the track is one call that can't mismatch the two:
	/// `catalog.json_track("chat")?.subscribe::<Message>(&source).await?`.
	pub fn json_track(&self, name: &str) -> Option<crate::catalog::Entry<'_, hang::catalog::JsonConfig>> {
		let (name, config) = self.json.tracks.get_key_value(name)?;
		Some(crate::catalog::Entry::new(name, config))
	}

	/// Every JSON track the catalog lists, in name order.
	///
	/// This is the discovery path: the catalog is the only thing that announces a data track, so a
	/// consumer finds them by walking this.
	pub fn json_tracks(&self) -> impl Iterator<Item = crate::catalog::Entry<'_, hang::catalog::JsonConfig>> {
		self.json
			.tracks
			.iter()
			.map(|(name, config)| crate::catalog::Entry::new(name, config))
	}

	/// The binary track named `name`, or `None` if the catalog doesn't list one.
	///
	/// See [`json_track`](Self::json_track).
	pub fn binary_track(&self, name: &str) -> Option<crate::catalog::Entry<'_, hang::catalog::BinaryConfig>> {
		let (name, config) = self.binary.tracks.get_key_value(name)?;
		Some(crate::catalog::Entry::new(name, config))
	}

	/// Every binary track the catalog lists, in name order.
	///
	/// See [`json_tracks`](Self::json_tracks).
	pub fn binary_tracks(&self) -> impl Iterator<Item = crate::catalog::Entry<'_, hang::catalog::BinaryConfig>> {
		self.binary
			.tracks
			.iter()
			.map(|(name, config)| crate::catalog::Entry::new(name, config))
	}

	/// The base catalog carrying just the media sections, used to derive the MSF track.
	///
	/// MSF describes media only, so the data sections are deliberately left out.
	pub(crate) fn media(&self) -> hang::Catalog {
		let mut catalog = hang::Catalog::default();
		catalog.video = self.video.clone();
		catalog.audio = self.audio.clone();
		catalog.timeline = self.timeline.clone();
		catalog.text = self.text.clone();
		catalog
	}
}

impl Catalog<Extra> {
	/// Look up an application catalog section by name, returning its raw JSON value.
	pub fn section(&self, name: &str) -> Option<&serde_json::Value> {
		self.ext.get(name)
	}

	/// Iterate over the application catalog sections as `(name, value)` pairs.
	pub fn sections(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
		self.ext.iter()
	}
}

// Deref to the extension so its sections are reachable directly (the base media sections are
// already real fields, so they shadow this and stay accessible as `catalog.video`/`catalog.audio`).
impl<E: CatalogExt> Deref for Catalog<E> {
	type Target = E;

	fn deref(&self) -> &E {
		&self.ext
	}
}

impl<E: CatalogExt> DerefMut for Catalog<E> {
	fn deref_mut(&mut self) -> &mut E {
		&mut self.ext
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde::{Deserialize, Serialize};

	use super::*;

	#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
	struct Scte35Ext {
		#[serde(skip_serializing_if = "Option::is_none")]
		scte35: Option<Scte35>,
	}

	#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
	struct Scte35 {
		splice_id: u32,
	}

	impl CatalogExt for Scte35Ext {}

	#[test]
	fn legacy_text_section_keeps_the_catalog() {
		// `Extra` is exactly the mechanism an application would have used to carry its own `text`
		// section before captions reserved the name, so an unreadable one must cost its captions
		// and nothing else rather than failing the whole decode.
		let json =
			r#"{"video":{"renditions":{}},"audio":{"renditions":{}},"text":{"overlay":"hi"},"scte35":{"spliceId":7}}"#;

		let catalog: Catalog<Extra> = serde_json::from_str(json).expect("legacy text section broke the catalog");
		assert!(catalog.text.is_empty());
		assert!(catalog.ext.get("scte35").is_some(), "unrelated sections still decode");
	}

	#[test]
	fn extension_roundtrip() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut producer =
			crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Scte35Ext>::default()).unwrap();
		let mut consumer = producer.consume().unwrap();

		// The media pipeline sets a base section (flat field); the app adds its own extension.
		// Sequential locks compose because each starts from the producer's retained catalog.
		producer.lock().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer.lock().scte35 = Some(Scte35 { splice_id: 42 }); // flat, via deref to the extension

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert!(catalog.audio.renditions.contains_key("audio0"));
		assert_eq!(catalog.scte35, Some(Scte35 { splice_id: 42 }));
	}

	#[test]
	fn untyped_extra_roundtrip() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut producer = crate::catalog::Producer::<Extra>::with_catalog(&mut broadcast, Catalog::default()).unwrap();
		let mut consumer = producer.consume().unwrap();

		// A media section (flat field) coexists with an arbitrary untyped application section.
		producer.lock().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer
			.lock()
			.set_section("transcript", serde_json::json!({ "track": "transcript.json" }))
			.unwrap();

		// Reserved media keys can't be smuggled in as application sections.
		assert!(matches!(
			producer.lock().set_section("video", serde_json::json!({})),
			Err(crate::Error::ReservedSection(_))
		));

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert!(catalog.audio.renditions.contains_key("audio0"));
		assert_eq!(
			catalog.section("transcript"),
			Some(&serde_json::json!({ "track": "transcript.json" }))
		);
		assert_eq!(catalog.sections().count(), 1);
	}
}

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
pub trait CatalogExt: Serialize + DeserializeOwned + Default + Clone + Send + 'static {}

impl CatalogExt for () {}

/// The untyped catalog extension: arbitrary top-level JSON sections beyond the base
/// `video`/`audio` media sections, captured and republished verbatim.
///
/// This is the default extension (so a plain [`Catalog`] preserves sections it doesn't
/// recognize instead of dropping them, matching the permissive JS catalog schema). Reach
/// for a typed [`CatalogExt`] struct instead when you want compile-time fields; reach for
/// `()` when you explicitly want unknown sections dropped.
///
/// `video` and `audio` are reserved for the base media sections, so [`set`](Self::set)
/// rejects them to keep the wire JSON free of duplicate keys.
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

	/// Set (or replace) a section. Errors if `name` collides with a reserved media
	/// section (`video`/`audio`).
	pub fn set(&mut self, name: impl Into<String>, value: serde_json::Value) -> crate::Result<()> {
		let name = name.into();
		if matches!(name.as_str(), "video" | "audio") {
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

/// The base media sections plus an application extension `E` (defaulting to [`Extra`], the
/// untyped JSON passthrough), serialized as a flat union: the `video`/`audio` sections and the
/// extension's sections share one JSON object on the wire.
///
/// `video` and `audio` are direct fields (`catalog.video`), and the catalog derefs to the extension
/// so its sections are reachable directly too (`catalog.scte35`, or `catalog.ext.scte35`
/// explicitly). A consumer reading a different extension (or none) ignores sections it doesn't know.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned"))]
pub struct Catalog<E: CatalogExt = Extra> {
	#[serde(default)]
	pub video: hang::catalog::Video,

	#[serde(default)]
	pub audio: hang::catalog::Audio,

	#[serde(flatten)]
	pub ext: E,
}

impl<E: CatalogExt> Catalog<E> {
	/// The base catalog carrying just the media sections, used to derive the MSF track.
	pub(crate) fn media(&self) -> hang::Catalog {
		hang::Catalog {
			video: self.video.clone(),
			audio: self.audio.clone(),
		}
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
	fn extension_roundtrip() {
		let mut broadcast = moq_net::Broadcast::new().produce();
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
		let mut broadcast = moq_net::Broadcast::new().produce();
		let mut producer = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut consumer = producer.consume().unwrap();

		// A media section coexists with an arbitrary, untyped application section.
		producer.lock().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer
			.set_section("transcript", serde_json::json!({ "track": "transcript.json" }))
			.unwrap();

		// Reserved media keys can't be smuggled in as application sections.
		assert!(matches!(
			producer.set_section("video", serde_json::json!({})),
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

	#[test]
	fn untyped_extra_serializes_flat_and_omits_when_empty() {
		// An empty extension is byte-identical to the no-extension catalog (wire compatibility).
		let empty = Catalog::<Extra>::default();
		assert_eq!(
			serde_json::to_value(&empty).unwrap(),
			serde_json::to_value(Catalog::<()>::default()).unwrap()
		);

		// A set section lands as a flat top-level key alongside `video`/`audio`.
		let mut catalog = Catalog::<Extra>::default();
		catalog.ext.set("scte35", serde_json::json!({ "spliceId": 7 })).unwrap();
		let json = serde_json::to_value(&catalog).unwrap();
		assert_eq!(json["scte35"], serde_json::json!({ "spliceId": 7 }));
	}
}

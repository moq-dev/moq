use serde::{Deserialize, Serialize};

/// An application's catalog extension: a plain serde struct of extra root sections that are
/// serialized as a flat union with the base media sections.
///
/// Implement it (no methods) on a struct of your own sections, then publish/consume a
/// [`hang::Catalog<YourExt>`]:
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
/// The unit type `()` is the no-extension case, so [`hang::Catalog<()>`] is just the base media catalog.
///
/// The same extension rides the MSF catalog track, so this requires [`moq_msf::CatalogExt`];
/// that trait is blanket-implemented, so one `impl CatalogExt` is still all a caller writes.
pub trait CatalogExt: moq_msf::CatalogExt {}

impl CatalogExt for () {}

/// The untyped catalog extension: arbitrary top-level JSON sections beyond the base
/// `video`/`audio`/`text` media sections and shared `archive`, captured and republished verbatim.
///
/// This is the extension a caller reaches for when the section names aren't known at
/// compile time, e.g. across the FFI/C boundary where a typed [`CatalogExt`] struct can't
/// cross. Publish/consume a [`hang::Catalog<Extra>`] and use [`set`](Self::set)/[`get`](Self::get).
/// The default extension stays `()` (unknown sections dropped); opt into `Extra` explicitly.
///
/// `video`, `audio`, `text`, `archive`, `clock`, `json`, `binary`, and the retired `timeline`
/// key are reserved, so decoding and [`set`](Self::set) reject them to keep the wire JSON free of
/// duplicate or retired keys.
#[derive(Serialize, Clone, Default, Debug, PartialEq)]
#[serde(transparent)]
pub struct Extra(serde_json::Map<String, serde_json::Value>);

impl CatalogExt for Extra {}

impl<'de> Deserialize<'de> for Extra {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let sections = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
		if let Some(name) = sections.keys().find(|name| reserved(name)) {
			return Err(serde::de::Error::custom(format!("reserved catalog section: {name}")));
		}
		Ok(Self(sections))
	}
}

fn reserved(name: &str) -> bool {
	matches!(
		name,
		"video" | "audio" | "text" | "archive" | "clock" | "json" | "binary" | "timeline"
	) || moq_msf::reserved_root(name)
}

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

	/// Set (or replace) a section. Errors if `name` collides with a reserved member of either
	/// catalog: hang's base sections (`video`/`audio`/`text`/`archive`/`clock`/`json`/`binary`),
	/// the retired `timeline` key that the wire format forbids beside `archive`, or a root member
	/// MSF defines ([`moq_msf::reserved_root`]).
	///
	/// The same sections ride both catalog tracks, so a name reserved on either is refused on
	/// both. Without the MSF half a colliding name reaches that track as a duplicate JSON key,
	/// which serde emits without complaint.
	pub fn set(&mut self, name: impl Into<String>, value: serde_json::Value) -> crate::Result<()> {
		let name = name.into();
		if reserved(&name) {
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

#[cfg(test)]
mod test {
	use std::task::Poll;

	use hang::Catalog;
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
	fn retired_timeline_section_is_rejected() {
		let json = r#"{"timeline":{"track":"timeline.json"}}"#;

		let error = serde_json::from_str::<Catalog<Extra>>(json).expect_err("retired timeline section was accepted");
		assert!(error.to_string().contains("reserved catalog section: timeline"));
	}

	#[test]
	fn extension_roundtrip() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = crate::catalog::Config::default().with_catalog(Catalog::<Scte35Ext>::default());
		let mut producer = crate::catalog::Producer::new(&mut broadcast, config).unwrap();
		let mut consumer = producer.consume().unwrap();

		// The media pipeline sets a base section (flat field); the app adds its own extension.
		// Sequential locks compose because each starts from the producer's retained catalog.
		producer.modify().unwrap().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer.modify().unwrap().ext.scte35 = Some(Scte35 { splice_id: 42 });

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert!(catalog.audio.renditions.contains_key("audio0"));
		assert_eq!(catalog.ext.scte35, Some(Scte35 { splice_id: 42 }));
	}

	#[test]
	fn untyped_extra_roundtrip() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = crate::catalog::Config::default().with_catalog(Catalog::<Extra>::default());
		let mut producer = crate::catalog::Producer::<Extra>::new(&mut broadcast, config).unwrap();
		let mut consumer = producer.consume().unwrap();

		// A media section (flat field) coexists with an arbitrary untyped application section.
		producer.modify().unwrap().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer
			.modify()
			.unwrap()
			.set_section("transcript", serde_json::json!({ "track": "transcript.json" }))
			.unwrap();

		// Reserved media keys can't be smuggled in as application sections.
		assert!(matches!(
			producer.modify().unwrap().set_section("video", serde_json::json!({})),
			Err(crate::Error::ReservedSection(_))
		));
		assert!(matches!(
			producer
				.modify()
				.unwrap()
				.set_section("timeline", serde_json::json!({})),
			Err(crate::Error::ReservedSection(_))
		));
		assert!(matches!(
			producer.modify().unwrap().set_section("clock", serde_json::json!({})),
			Err(crate::Error::ReservedSection(_))
		));

		// Nor can a member MSF defines: the same sections ride the MSF track, where a
		// collision would serialize as a duplicate JSON key rather than an error.
		for name in ["version", "generatedAt", "isComplete", "tracks", "initDataList"] {
			assert!(
				matches!(
					producer.modify().unwrap().set_section(name, serde_json::json!("nope")),
					Err(crate::Error::ReservedSection(_))
				),
				"{name} must be refused",
			);
		}

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert!(catalog.audio.renditions.contains_key("audio0"));
		assert_eq!(
			catalog.ext.get("transcript"),
			Some(&serde_json::json!({ "track": "transcript.json" }))
		);
		assert_eq!(catalog.ext.iter().count(), 1);
	}
}

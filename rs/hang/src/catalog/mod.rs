//! The catalog describes the tracks a broadcast publishes.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! It describes the available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata, plus the `json` and `binary` sections
//! listing the data tracks that aren't media.

mod audio;
mod binary;
mod clock;
mod consumer;
mod container;
mod echo;
mod hex;
mod include;
mod json;
mod m2ts;
mod media;
mod millis;
mod mode;
mod priority;
mod reference;
mod root;
mod skipped;
mod stats;
mod text;
mod video;

pub use audio::*;
pub use binary::*;
pub use clock::*;
pub use consumer::*;
pub use container::*;
pub use echo::*;
pub use include::*;
pub use json::*;
pub use m2ts::*;
pub use media::*;
pub use mode::*;
pub use priority::*;
pub use reference::*;
pub use root::*;
pub use skipped::Skipped;
pub use stats::*;
pub use text::*;
pub use video::*;

/// The newest moq-hang draft this crate reads. A catalog whose root `version` is higher is refused,
/// and an entry whose `version` is higher is skipped.
pub(crate) const VERSION: u64 = 4;

/// Serde default for a rendition's `enabled` field.
pub(crate) fn enabled_default() -> bool {
	true
}

/// Skip serializing `enabled` unless it is false, so only a disabled rendition writes it.
pub(crate) fn enabled_skip(enabled: &bool) -> bool {
	*enabled
}

pub(crate) fn deserialize_timescale_or_default<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::Deserialize;
	let value = u32::deserialize(deserializer)?;
	if value == 0 {
		return Err(serde::de::Error::custom("invalid timescale: 0"));
	}
	Ok(value)
}

/// Parse a codec string field of digits in `radix`, refusing the empty field, sign, and whitespace
/// `from_str_radix` alone would take.
pub(crate) fn codec_number<T: TryFrom<u32>>(field: &str, radix: u32) -> Result<T, crate::Error> {
	if field.is_empty() || !field.chars().all(|c| c.is_digit(radix)) {
		return Err(crate::Error::InvalidCodec);
	}
	let value = u32::from_str_radix(field, radix).map_err(|_| crate::Error::InvalidCodec)?;
	T::try_from(value).map_err(|_| crate::Error::InvalidCodec)
}

/// Decode a struct only from a JSON object.
///
/// serde also reads a struct from an array of its fields in order, which would let an array section
/// such as the retired `video: [{track, config}]` form decode as something else entirely.
pub(crate) fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
	D: serde::Deserializer<'de>,
	T: serde::de::DeserializeOwned,
{
	use serde::Deserialize;
	let object = serde_json::Map::deserialize(deserializer)?;
	serde_json::from_value(serde_json::Value::Object(object)).map_err(serde::de::Error::custom)
}

/// Decode the root `media` section, falling back to an empty one when the value is not an object.
///
/// `media` is a generic enough key that an application could have carried its own before it was
/// reserved, so a non-object is someone else's, for the same reason as [`deserialize_section`]. An
/// object is ours and must decode.
pub(crate) fn deserialize_media<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Media, D::Error> {
	use serde::Deserialize;
	let value = serde_json::Value::deserialize(deserializer)?;
	if !value.is_object() {
		return Ok(Media::default());
	}
	serde_json::from_value(value).map_err(serde::de::Error::custom)
}

/// An entry of a catalog map keyed by ID (a rendition, data track, or include), checked as it
/// decodes and versioned as it encodes.
pub(crate) trait Entry: serde::de::DeserializeOwned + serde::Serialize {
	/// Refuse a field out of the range its type allows.
	fn check(&self) -> crate::Result<()> {
		Ok(())
	}

	/// The oldest moq-hang draft that reads this entry correctly given its ID, or `None` while that
	/// is 3. Written as the entry's `version`.
	///
	/// Only a field an older reader would misread raises it; one it would merely ignore does not.
	fn version(&self, _id: &str) -> Option<u64> {
		None
	}

	/// Whether the entry keeps an unrecognized member named `version`, which would sit beside the
	/// computed one as a duplicate key.
	fn claims_version(&self) -> bool {
		false
	}
}

/// Decode a catalog map keyed by ID, skipping an entry newer than this reader and refusing the
/// catalog if any other entry is malformed.
///
/// An entry's `version` is read before the rest of it, since a newer draft may have reshaped the
/// entry; one above [`VERSION`] is skipped, and [`Catalog::skipped`](crate::catalog::Catalog::skipped)
/// reports it. Every other entry must be an object whose fields are in range; the error names the
/// entry. An unknown container or extension field is not malformed, so it never refuses the catalog.
pub(crate) fn deserialize_entries<'de, D, T>(deserializer: D) -> Result<std::collections::BTreeMap<String, T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Entry,
{
	use serde::Deserialize;
	let entries = std::collections::BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;

	let decode = |mut value: serde_json::Value| -> crate::Result<Option<T>> {
		// Taken out rather than left to the entry: written versions are computed, never carried.
		if let Some(version) = value.as_object_mut().and_then(|entry| entry.remove("version"))
			&& serde_json::from_value::<u64>(version)? > VERSION
		{
			return Ok(None);
		}
		let config: T = deserialize_object(value)?;
		config.check()?;
		Ok(Some(config))
	};

	let mut decoded = std::collections::BTreeMap::new();
	for (id, value) in entries {
		match decode(value) {
			Ok(Some(config)) => {
				decoded.insert(id, config);
			}
			Ok(None) => {}
			Err(err) => return Err(serde::de::Error::custom(format!("malformed entry {id}: {err}"))),
		}
	}
	Ok(decoded)
}

/// Encode a catalog map keyed by ID, writing each entry's computed `version` ahead of its fields.
pub(crate) fn serialize_entries<S, T>(
	entries: &std::collections::BTreeMap<String, T>,
	serializer: S,
) -> Result<S::Ok, S::Error>
where
	S: serde::Serializer,
	T: Entry,
{
	#[derive(serde::Serialize)]
	struct Versioned<'a, T> {
		#[serde(skip_serializing_if = "Option::is_none")]
		version: Option<u64>,
		#[serde(flatten)]
		config: &'a T,
	}

	if let Some(id) = entries
		.iter()
		.find_map(|(id, config)| config.claims_version().then_some(id))
	{
		return Err(serde::ser::Error::custom(format!(
			"catalog entry {id} must not carry a member `version`"
		)));
	}

	serializer.collect_map(entries.iter().map(|(id, config)| {
		let version = config.version(id);
		(id, Versioned { version, config })
	}))
}

/// A catalog section: a map of track name to config, published under one well-known root key.
///
/// The associated [`MAP`](Self::MAP) is what tells a real section apart from an application's own
/// value under the same name. See [`deserialize_section`].
pub(crate) trait Section: Default + serde::de::DeserializeOwned {
	/// The key holding the section's map of tracks.
	const MAP: &'static str;
}

/// Decode a catalog section, falling back to an empty one when the value isn't a section at all.
///
/// A section name is only reserved from the version that defines it, so an application may already
/// be carrying its own key under that name (through [`Extra`](crate::catalog::Catalog), or in a
/// catalog this build has never seen). Dropping the one key we can't read keeps the rest of the
/// catalog readable, rather than failing the whole document and taking video and audio down with it.
///
/// The fallback is deliberately narrow: only a value with no [`Section::MAP`] is treated as someone
/// else's. A value that *is* a section but carries a malformed entry still fails, since swallowing
/// that would hide a publisher bug (a data track with no `mode`) behind the silence that exists to
/// protect an unrelated key.
pub(crate) fn deserialize_section<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Section,
{
	use serde::Deserialize;
	let value = serde_json::Value::deserialize(deserializer)?;

	if !value.get(T::MAP).is_some_and(serde_json::Value::is_object) {
		return Ok(T::default());
	}

	serde_json::from_value(value).map_err(serde::de::Error::custom)
}

/// Decode an optional section identified by one string field (`stats.track`, `echo.path`).
///
/// A value without that string field is someone else's key and decodes as absent, for the same
/// reason as [`deserialize_section`]; a value with it is ours and must decode.
fn deserialize_named<'de, D, T>(deserializer: D, key: &str) -> Result<Option<T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: serde::de::DeserializeOwned,
{
	use serde::Deserialize;
	let value = serde_json::Value::deserialize(deserializer)?;

	if !value.get(key).is_some_and(serde_json::Value::is_string) {
		return Ok(None);
	}

	serde_json::from_value(value)
		.map(Some)
		.map_err(serde::de::Error::custom)
}

pub(crate) fn deserialize_stats<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Stats>, D::Error> {
	deserialize_named(deserializer, "track")
}

pub(crate) fn deserialize_echo<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Echo>, D::Error> {
	deserialize_named(deserializer, "path")
}

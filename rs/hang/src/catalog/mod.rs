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
mod contribution;
mod echo;
mod hex;
mod hls;
mod json;
mod m2ts;
mod millis;
mod mode;
mod priority;
mod reference;
mod root;
mod stats;
mod text;
mod video;

pub use audio::*;
pub use binary::*;
pub use clock::*;
pub use consumer::*;
pub use container::*;
pub use contribution::*;
pub use echo::*;
pub use hls::*;
pub use json::*;
pub use m2ts::*;
pub use mode::*;
pub use priority::*;
pub use reference::*;
pub use root::*;
pub use stats::*;
pub use text::*;
pub use video::*;

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

/// A media rendition config, checked as it decodes.
pub(crate) trait Rendition: serde::de::DeserializeOwned {
	/// Refuse a field out of the range its type allows.
	fn check(&self) -> crate::Result<()>;
}

/// Decode a media section's renditions, refusing the catalog if any one is malformed.
///
/// Each rendition must be an object whose fields are in range; the error names the rendition.
/// An unknown container or extension field is not malformed, so it never refuses the catalog.
pub(crate) fn deserialize_renditions<'de, D, T>(
	deserializer: D,
) -> Result<std::collections::BTreeMap<String, T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Rendition,
{
	use serde::Deserialize;
	let renditions = std::collections::BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;

	let decode = |value: serde_json::Value| -> crate::Result<T> {
		let config: T = deserialize_object(value)?;
		config.check()?;
		Ok(config)
	};

	renditions
		.into_iter()
		.map(|(id, value)| match decode(value) {
			Ok(config) => Ok((id, config)),
			Err(err) => Err(serde::de::Error::custom(format!("malformed rendition {id}: {err}"))),
		})
		.collect()
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

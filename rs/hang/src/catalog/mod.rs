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

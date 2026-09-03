//! The catalog describes the tracks a broadcast publishes.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! It describes the available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata, plus the `json` and `binary` sections
//! listing the data tracks that aren't media.

mod audio;
mod binary;
mod compression;
mod container;
mod hex;
mod json;
mod mode;
mod priority;
mod root;
mod text;
mod timeline;
mod video;

pub use audio::*;
pub use binary::*;
pub use compression::*;
pub use container::*;
pub use json::*;
pub use mode::*;
pub use priority::*;
pub use root::*;
pub use text::*;
pub use timeline::*;
pub use video::*;

/// A catalog section: a map of track name to config, published under one well-known root key.
///
/// The associated [`MAP`](Self::MAP) is what tells a real section apart from an application's own
/// value under the same name. See [`deserialize_section`].
pub trait Section: Default + serde::de::DeserializeOwned {
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
pub fn deserialize_section<'de, D, T>(deserializer: D) -> Result<T, D::Error>
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

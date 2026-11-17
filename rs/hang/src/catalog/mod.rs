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

/// Decode an optional section or member, which must be an object when present.
///
/// `null` is not absent: a publisher leaves the section off the wire instead.
pub(crate) fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: serde::de::DeserializeOwned,
{
	deserialize_object(deserializer).map(Some)
}

/// A [`serde_with`] adapter for an optional field that is absent or holds a value, never `null`.
///
/// serde reads `null` as `None`, but the draft refuses a catalog with a `null` field it defines.
/// Pair it with `#[serde(default)]` so an absent field is still `None`.
pub(crate) struct NotNull<T = serde_with::Same>(std::marker::PhantomData<T>);

impl<T, U: serde_with::SerializeAs<T>> serde_with::SerializeAs<Option<T>> for NotNull<U> {
	fn serialize_as<S: serde::Serializer>(source: &Option<T>, serializer: S) -> Result<S::Ok, S::Error> {
		match source {
			Some(value) => U::serialize_as(value, serializer),
			None => serializer.serialize_none(),
		}
	}
}

impl<'de, T, U: serde_with::DeserializeAs<'de, T>> serde_with::DeserializeAs<'de, Option<T>> for NotNull<U> {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
		U::deserialize_as(deserializer).map(Some)
	}
}

/// An entry of a catalog map keyed by ID (a rendition, data track, or include), checked as it
/// decodes.
pub(crate) trait Entry: serde::de::DeserializeOwned {
	/// Refuse a field out of the range its type allows.
	fn check(&self) -> crate::Result<()> {
		Ok(())
	}
}

/// Decode a catalog map keyed by ID, refusing the catalog if any entry is malformed.
///
/// Each entry must be an object whose fields are in range; the error names the entry.
/// An unknown container or extension field is not malformed, so it never refuses the catalog.
pub(crate) fn deserialize_entries<'de, D, T>(deserializer: D) -> Result<std::collections::BTreeMap<String, T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Entry,
{
	use serde::Deserialize;
	let entries = std::collections::BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;

	let decode = |value: serde_json::Value| -> crate::Result<T> {
		let config: T = deserialize_object(value)?;
		config.check()?;
		Ok(config)
	};

	entries
		.into_iter()
		.map(|(id, value)| match decode(value) {
			Ok(config) => Ok((id, config)),
			Err(err) => Err(serde::de::Error::custom(format!("malformed entry {id}: {err}"))),
		})
		.collect()
}

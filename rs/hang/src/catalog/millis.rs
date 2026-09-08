//! Whole-millisecond encoding for the catalog's duration fields.

use std::time::Duration;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};
use serde_with::{DeserializeAs, SerializeAs};

/// A [`serde_with`] adapter encoding a [`Duration`] as whole milliseconds, rounded up.
///
/// Every duration on the wire here is an upper bound a consumer sizes a buffer against, so
/// rounding down is the one direction that breaks it. A 44.1 kHz AAC frame is 23.2 ms, and
/// truncating advertises 23; anything under a millisecond truncates to 0, which reads as
/// "flushed immediately" rather than "a little".
pub(crate) struct MillisCeil;

impl SerializeAs<Duration> for MillisCeil {
	fn serialize_as<S: Serializer>(source: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
		let millis = source.as_nanos().div_ceil(1_000_000);
		// A duration over 584 million years is nonsense either way; clamping keeps this total.
		u64::try_from(millis).unwrap_or(u64::MAX).serialize(serializer)
	}
}

impl<'de> DeserializeAs<'de, Duration> for MillisCeil {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
		Ok(Duration::from_millis(u64::deserialize(deserializer)?))
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::Catalog;

	/// A sub-millisecond jitter must not reach the wire as 0: 0 means "flushed immediately",
	/// which is the one thing a consumer must not believe about a track that buffers.
	#[test]
	fn rounds_up() {
		let mut catalog = Catalog::default();
		let mut config = crate::catalog::AudioConfig::new(crate::catalog::AudioCodec::Opus, 48_000, 2);

		// A 44.1 kHz AAC frame: 1024/44100 = 23.2 ms.
		config.jitter = Some(std::time::Duration::from_micros(23_220));
		catalog.audio.renditions.insert("audio".to_string(), config.clone());
		assert!(catalog.to_json().unwrap().contains(r#""jitter":24"#));

		config.jitter = Some(std::time::Duration::from_micros(500));
		catalog.audio.renditions.insert("audio".to_string(), config.clone());
		assert!(catalog.to_json().unwrap().contains(r#""jitter":1"#));

		// A whole number of milliseconds is left alone.
		config.jitter = Some(std::time::Duration::from_millis(20));
		catalog.audio.renditions.insert("audio".to_string(), config);
		assert!(catalog.to_json().unwrap().contains(r#""jitter":20"#));
	}
}

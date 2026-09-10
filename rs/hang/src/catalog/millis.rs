//! Whole-millisecond encoding for the catalog's duration fields.

use std::time::Duration;

use serde::de::Deserializer;
use serde::ser::Error as _;
use serde::{Deserialize, Serialize, Serializer};
use serde_with::{DeserializeAs, SerializeAs};

/// A [`serde_with`] adapter encoding an optional [`Duration`] as whole milliseconds, rounded up.
///
/// Every duration on the wire here is an upper bound a consumer sizes a buffer against, so
/// rounding down is the one direction that breaks it. A 44.1 kHz AAC frame is 23.2 ms, and
/// truncating advertises 23; anything under a millisecond truncates to 0, which reads as
/// "flushed immediately" rather than "a little".
///
/// So `0` is not a value the field carries: a track that flushes each frame immediately omits it.
/// Reading one is how a publisher that truncated says "immediately", so it decodes as absent;
/// writing one is a caller stating something the field can't mean, so it's refused.
pub(crate) struct MillisCeil;

impl SerializeAs<Option<Duration>> for MillisCeil {
	fn serialize_as<S: Serializer>(source: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error> {
		let Some(duration) = source else {
			return serializer.serialize_none();
		};

		if duration.is_zero() {
			return Err(S::Error::custom(
				"a duration of 0 has no meaning here; omit the field for a track flushed immediately",
			));
		}

		let millis = duration.as_nanos().div_ceil(1_000_000);
		u64::try_from(millis)
			.map_err(|_| S::Error::custom("a duration too long to express in milliseconds"))?
			.serialize(serializer)
	}
}

impl<'de> DeserializeAs<'de, Option<Duration>> for MillisCeil {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
		Ok(Option::<u64>::deserialize(deserializer)?
			.filter(|millis| *millis != 0)
			.map(Duration::from_millis))
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::Catalog;

	fn audio(jitter: Option<std::time::Duration>) -> Catalog {
		let mut catalog = Catalog::default();
		let mut config = crate::catalog::AudioConfig::new(crate::catalog::AudioCodec::Opus, 48_000, 2);
		config.jitter = jitter;
		catalog.audio.renditions.insert("audio".to_string(), config);
		catalog
	}

	/// A sub-millisecond jitter must not reach the wire as 0: 0 means "flushed immediately",
	/// which is the one thing a consumer must not believe about a track that buffers.
	#[test]
	fn rounds_up() {
		// A 44.1 kHz AAC frame: 1024/44100 = 23.2 ms.
		let json = audio(Some(std::time::Duration::from_micros(23_220))).to_json().unwrap();
		assert!(json.contains(r#""jitter":24"#), "{json}");

		let json = audio(Some(std::time::Duration::from_micros(500))).to_json().unwrap();
		assert!(json.contains(r#""jitter":1"#), "{json}");

		// A whole number of milliseconds is left alone.
		let json = audio(Some(std::time::Duration::from_millis(20))).to_json().unwrap();
		assert!(json.contains(r#""jitter":20"#), "{json}");
	}

	/// Zero is not a value the field carries. A publisher stating it is refused; a catalog
	/// carrying it (a publisher that truncated a sub-millisecond value) reads as absent, which is
	/// the same thing it meant to say.
	#[test]
	fn zero_is_absent() {
		let err = audio(Some(std::time::Duration::ZERO))
			.to_json()
			.unwrap_err()
			.to_string();
		assert!(err.contains("omit the field"), "{err}");

		let json =
			r#"{"audio":{"renditions":{"audio":{"codec":"opus","sampleRate":48000,"numberOfChannels":2,"jitter":0}}}}"#;
		let catalog = Catalog::from_str(json).unwrap();
		assert_eq!(catalog.audio.renditions["audio"].jitter, None);

		// So it round-trips as an omission rather than failing to re-encode.
		assert!(!catalog.to_json().unwrap().contains("jitter"));
	}

	/// A duration past what `u64` milliseconds can carry is refused rather than clamped, so a
	/// consumer is never handed a bound smaller than the one the publisher meant.
	#[test]
	fn too_long_is_refused() {
		let err = audio(Some(std::time::Duration::MAX)).to_json().unwrap_err().to_string();
		assert!(err.contains("too long"), "{err}");
	}
}

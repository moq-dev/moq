//! JSON adapters that preserve existing wall-time encodings.

use crate::Wall;
use ::serde::{Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs, TimestampSeconds};

/// Encode a wall time as whole Unix seconds, matching serde_with's integer timestamp adapter.
pub struct UnixSeconds;

impl SerializeAs<Wall> for UnixSeconds {
	fn serialize_as<S: Serializer>(value: &Wall, serializer: S) -> Result<S::Ok, S::Error> {
		let since = value.saturating_duration_since(Wall::UNIX_EPOCH);
		// The integer adapter rounds to the nearest second. Refuse a rounding
		// carry past Wall's range rather than write a time our reader cannot read.
		if since.as_secs() == u64::MAX / 1_000_000_000 && since.subsec_nanos() >= 500_000_000 {
			return Err(::serde::ser::Error::custom(
				"rounded Unix seconds exceed the wall time range",
			));
		}
		TimestampSeconds::<i64>::serialize_as(&std::time::SystemTime::from(*value), serializer)
	}
}

impl<'de> DeserializeAs<'de, Wall> for UnixSeconds {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Wall, D::Error> {
		let value: std::time::SystemTime = TimestampSeconds::<i64>::deserialize_as(deserializer)?;
		Wall::try_from(value).map_err(::serde::de::Error::custom)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ::serde::{Deserialize, Serialize};
	use std::time::Duration;

	#[serde_with::serde_as]
	#[derive(Debug, Serialize, Deserialize, PartialEq)]
	struct Value {
		#[serde_as(as = "Option<UnixSeconds>")]
		wall: Option<Wall>,
	}

	#[test]
	fn preserves_integer_seconds_and_null() {
		for json in [
			r#"{"wall":0}"#,
			r#"{"wall":1700000000}"#,
			r#"{"wall":18446744073}"#,
			r#"{"wall":null}"#,
		] {
			let value: Value = serde_json::from_str(json).unwrap();
			assert_eq!(serde_json::to_string(&value).unwrap(), json);
		}
		for nanos in [1, 499_999_999, 500_000_000, 999_999_999] {
			let wall = Wall::UNIX_EPOCH + Duration::new(1_700_000_000, nanos);
			let actual = serde_json::to_string(&Value { wall: Some(wall) }).unwrap();
			let expected = serde_with::As::<TimestampSeconds<i64>>::serialize(
				&std::time::SystemTime::from(wall),
				serde_json::value::Serializer,
			)
			.unwrap();
			assert_eq!(
				serde_json::from_str::<serde_json::Value>(&actual).unwrap()["wall"],
				expected
			);
		}
	}

	#[test]
	fn refuses_rounding_past_the_wall_range() {
		let seconds = u64::MAX / 1_000_000_000;
		let last = Wall::UNIX_EPOCH + Duration::new(seconds, 499_999_999);
		let json = serde_json::to_string(&Value { wall: Some(last) }).unwrap();
		assert_eq!(
			serde_json::from_str::<Value>(&json).unwrap().wall,
			Some(Wall::UNIX_EPOCH + Duration::from_secs(seconds))
		);
		for nanos in [500_000_000, (u64::MAX % 1_000_000_000) as u32] {
			let wall = Wall::UNIX_EPOCH + Duration::new(seconds, nanos);
			assert!(serde_json::to_string(&Value { wall: Some(wall) }).is_err());
		}
	}

	#[test]
	fn refuses_values_outside_the_wall_range() {
		for seconds in [-1, 18_446_744_074, i64::MAX] {
			assert!(serde_json::from_str::<Value>(&format!(r#"{{"wall":{seconds}}}"#)).is_err());
		}
	}
}

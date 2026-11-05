//! JSON adapters that preserve existing wall-time encodings.

use crate::clock::Wall;
use ::serde::{Deserialize, Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs, TimestampSeconds};

/// Encode a wall time as whole Unix seconds, matching serde_with's integer timestamp adapter.
///
/// Decoding saturates: before 1970 reads as the epoch, and past 2554 as Wall's last time.
pub struct UnixSeconds;

impl SerializeAs<Wall> for UnixSeconds {
	fn serialize_as<S: Serializer>(value: &Wall, serializer: S) -> Result<S::Ok, S::Error> {
		TimestampSeconds::<i64>::serialize_as(&std::time::SystemTime::from(*value), serializer)
	}
}

impl<'de> DeserializeAs<'de, Wall> for UnixSeconds {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Wall, D::Error> {
		let seconds = i64::deserialize(deserializer)?;
		let since = std::time::Duration::from_secs(u64::try_from(seconds).unwrap_or(0));
		Ok(Wall::UNIX_EPOCH.saturating_add(since))
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
	fn saturates_outside_the_wall_range() {
		let last = Wall::UNIX_EPOCH.saturating_add(Duration::MAX);
		for (seconds, expected) in [
			(i64::MIN, Wall::UNIX_EPOCH),
			(-1, Wall::UNIX_EPOCH),
			(18_446_744_074, last),
			(i64::MAX, last),
		] {
			let value: Value = serde_json::from_str(&format!(r#"{{"wall":{seconds}}}"#)).unwrap();
			assert_eq!(value.wall, Some(expected));
		}
		// The last wall time rounds up past the range and reads back as itself.
		let json = serde_json::to_string(&Value { wall: Some(last) }).unwrap();
		assert_eq!(json, r#"{"wall":18446744074}"#);
		assert_eq!(serde_json::from_str::<Value>(&json).unwrap().wall, Some(last));
	}
}

//! Patterns as strings on the wire and in config; unions as lists.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{Pattern, Patterns};

impl Serialize for Pattern {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Pattern {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(de::Error::custom)
	}
}

impl Serialize for Patterns {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_seq(self.iter())
	}
}

impl<'de> Deserialize<'de> for Patterns {
	/// Reads a list and reduces it, so a persisted union is canonical after a round trip.
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(Vec::<Pattern>::deserialize(deserializer)?.into_iter().collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pattern_round_trips_as_text() {
		let pattern: Pattern = "a/*/**".parse().unwrap();
		let json = serde_json::to_string(&pattern).unwrap();
		assert_eq!(json, "\"a/*/**\"");
		assert_eq!(serde_json::from_str::<Pattern>(&json).unwrap(), pattern);
		assert!(serde_json::from_str::<Pattern>("\"a//b\"").is_err());
	}

	#[test]
	fn union_round_trips_reduced() {
		let set: Patterns = serde_json::from_str(r#"["a/b", "a/*", "**/c"]"#).unwrap();
		assert_eq!(serde_json::to_string(&set).unwrap(), r#"["**/c","a/*"]"#);
	}
}

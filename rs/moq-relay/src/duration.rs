//! Human-readable duration adapters used only at the CLI and serde boundary.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Duration(std::time::Duration);

impl Duration {
	pub(crate) const fn into_std(self) -> std::time::Duration {
		self.0
	}
}

impl FromStr for Duration {
	type Err = humantime::DurationError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		humantime::parse_duration(value).map(Self)
	}
}

impl fmt::Display for Duration {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		humantime::format_duration(self.0).fmt(f)
	}
}

impl Serialize for Duration {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for Duration {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		value.parse().map_err(serde::de::Error::custom)
	}
}

pub(crate) mod serde_duration {
	use serde::{Deserialize, Deserializer, Serializer};

	pub(crate) fn serialize<S>(value: &std::time::Duration, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(&humantime::format_duration(*value))
	}

	pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<std::time::Duration, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		humantime::parse_duration(&value).map_err(serde::de::Error::custom)
	}
}

pub(crate) mod serde_option {
	use serde::{Deserialize, Deserializer, Serializer};

	pub(crate) fn serialize<S>(value: &Option<std::time::Duration>, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match value {
			Some(value) => serializer.collect_str(&humantime::format_duration(*value)),
			None => serializer.serialize_none(),
		}
	}

	pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<std::time::Duration>, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = Option::<String>::deserialize(deserializer)?;
		value
			.map(|value| humantime::parse_duration(&value).map_err(serde::de::Error::custom))
			.transpose()
	}
}

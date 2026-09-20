//! Human-readable command-line durations.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::time::Duration as StdDuration;

use ::serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration that parses human-readable command-line values such as `500ms` or `2m`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Duration {
	value: StdDuration,
	explicit: bool,
}

impl Duration {
	/// Returns the wrapped standard-library duration.
	pub const fn into_std(self) -> StdDuration {
		self.value
	}

	/// A parser default, which yields to a standing typed value during an update.
	pub(crate) const fn fallback(value: StdDuration) -> Self {
		Self { value, explicit: false }
	}

	/// Resolve an optional parser value against the public typed field.
	pub(crate) fn resolve(value: Option<Self>, configured: StdDuration) -> StdDuration {
		match value {
			Some(value) if value.explicit || configured.is_zero() => value.value,
			_ => configured,
		}
	}
}

impl Deref for Duration {
	type Target = StdDuration;

	fn deref(&self) -> &Self::Target {
		&self.value
	}
}

impl From<StdDuration> for Duration {
	fn from(value: StdDuration) -> Self {
		Self { value, explicit: true }
	}
}

impl From<Duration> for StdDuration {
	fn from(value: Duration) -> Self {
		value.value
	}
}

impl PartialEq<StdDuration> for Duration {
	fn eq(&self, other: &StdDuration) -> bool {
		self.value == *other
	}
}

impl FromStr for Duration {
	type Err = humantime::DurationError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		humantime::parse_duration(value).map(Into::into)
	}
}

impl fmt::Display for Duration {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		humantime::format_duration(self.value).fmt(f)
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
		value.parse().map_err(::serde::de::Error::custom)
	}
}

pub(crate) mod serde_duration {
	use ::serde::{Deserialize, Deserializer, Serializer};
	use std::time::Duration;

	pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(&humantime::format_duration(*value))
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		humantime::parse_duration(&value).map_err(::serde::de::Error::custom)
	}
}

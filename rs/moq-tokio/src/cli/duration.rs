//! Human-readable command-line durations.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration that parses human-readable command-line values such as `500ms` or `2m`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Duration(StdDuration);

impl Duration {
	/// Returns the wrapped standard-library duration.
	pub const fn into_std(self) -> StdDuration {
		self.0
	}
}

impl Deref for Duration {
	type Target = StdDuration;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl From<StdDuration> for Duration {
	fn from(value: StdDuration) -> Self {
		Self(value)
	}
}

impl From<Duration> for StdDuration {
	fn from(value: Duration) -> Self {
		value.0
	}
}

impl PartialEq<StdDuration> for Duration {
	fn eq(&self, other: &StdDuration) -> bool {
		self.0 == *other
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

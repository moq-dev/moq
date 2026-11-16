//! A duration limit that can be turned off.

use std::fmt;
use std::str::FromStr;

/// A limit written as a duration (`5s`, `0s`), or `none` to turn it off.
///
/// An unset flag or key takes the knob's default instead, so a setting has three
/// states: unset, `none`, and a duration, where zero means zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde_with::SerializeDisplay, serde_with::DeserializeFromStr)]
pub struct Bound(Option<std::time::Duration>);

impl Bound {
	/// No limit, written `none`.
	pub const NONE: Self = Self(None);
}

impl From<std::time::Duration> for Bound {
	fn from(value: std::time::Duration) -> Self {
		Self(Some(value))
	}
}

impl From<Option<std::time::Duration>> for Bound {
	fn from(value: Option<std::time::Duration>) -> Self {
		Self(value)
	}
}

impl From<Bound> for Option<std::time::Duration> {
	fn from(value: Bound) -> Self {
		value.0
	}
}

impl FromStr for Bound {
	type Err = humantime::DurationError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"none" => Ok(Self::NONE),
			value => humantime::parse_duration(value).map(Into::into),
		}
	}
}

impl fmt::Display for Bound {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.0 {
			Some(value) => humantime::format_duration(value).fmt(f),
			None => f.write_str("none"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_none_zero_and_a_duration() {
		assert_eq!("none".parse::<Bound>().unwrap(), Bound::NONE);
		assert_eq!("0s".parse::<Bound>().unwrap(), Bound::from(std::time::Duration::ZERO));
		assert_eq!(
			"1m 5s".parse::<Bound>().unwrap(),
			Bound::from(std::time::Duration::from_secs(65))
		);
		assert!("never".parse::<Bound>().is_err());
	}

	/// What a config file holds reads back as the same bound.
	#[test]
	fn round_trips_through_toml() {
		#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
		struct File {
			bound: Bound,
		}
		for bound in [
			Bound::NONE,
			std::time::Duration::ZERO.into(),
			std::time::Duration::from_millis(1500).into(),
		] {
			let file = File { bound };
			let text = toml::to_string(&file).unwrap();
			assert_eq!(toml::from_str::<File>(&text).unwrap(), file, "{text}");
		}
	}
}

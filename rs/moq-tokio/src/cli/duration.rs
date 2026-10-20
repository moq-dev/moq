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
}

impl Duration {
	/// Returns the wrapped standard-library duration.
	pub const fn into_std(self) -> StdDuration {
		self.value
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
		Self { value }
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
	/// Human-readable serialization for an optional duration.
	pub mod option {
		use ::serde::{Deserialize, Deserializer, Serializer};
		use std::time::Duration;

		pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: Serializer,
		{
			match value {
				Some(value) => serializer.collect_str(&humantime::format_duration(*value)),
				None => serializer.serialize_none(),
			}
		}

		pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
		where
			D: Deserializer<'de>,
		{
			Option::<String>::deserialize(deserializer)?
				.map(|value| humantime::parse_duration(&value).map_err(::serde::de::Error::custom))
				.transpose()
		}
	}
}

// Exercise the same precedence contract for each duration config, including its
// own interpretation of zero (disabled, unlimited, or a minimum pacing floor).
#[cfg(test)]
macro_rules! test_config_duration {
	($name:ident, $config:ty, $field:ident, $flag:literal, $resolve:expr, $expected:expr) => {
		#[test]
		fn $name() {
			#[derive(usage_rs::Cli)]
			#[usage(unknown_flags = "error", args_override_self = false)]
			#[usage(settings)]
			struct Wrapper {
				#[usage(flatten)]
				config: $config,
			}
			let parse = |args: &[&str]| {
				let args = args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>();
				Wrapper::parse_from(&args).unwrap().config
			};
			let merged = |config: &$config| -> $config { toml::Value::try_from(config).unwrap().try_into().unwrap() };
			let resolve = $resolve;
			let expected = $expected;
			let default = resolve(&<$config>::default());
			assert_eq!(resolve(&parse(&[])), default, "parsed default");
			assert_eq!(resolve(&merged(&parse(&[]))), default, "merged default");
			let mut unset = <$config>::default();
			unset.$field = None;
			assert_eq!(resolve(&unset), default, "unset uses the default");
			assert_eq!(resolve(&merged(&unset)), default, "unset round-trip");
			for value in [std::time::Duration::ZERO, std::time::Duration::from_secs(3)] {
				for mut config in [parse(&[]), merged(&parse(&[])), <$config>::default()] {
					config.$field = value.into();
					assert_eq!(resolve(&config), expected(value), "typed {value:?}");
					assert_eq!(resolve(&merged(&config)), expected(value), "round-trip {value:?}");
				}
				let flag_value = if value.is_zero() { "3s" } else { "0s" };
				let flag_duration = humantime::parse_duration(flag_value).unwrap();
				for mut config in [parse(&[$flag, flag_value]), merged(&parse(&[$flag, flag_value]))] {
					config.$field = value.into();
					assert_eq!(resolve(&config), expected(flag_duration), "explicit flag");
				}
			}
		}
	};
}
#[cfg(test)]
pub(crate) use test_config_duration;

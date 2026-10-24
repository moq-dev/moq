//! Human-readable command-line durations.

use std::fmt;
use std::str::FromStr;

/// A duration written as `500ms` or `2m` on the command line and in config files.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Duration(#[serde(with = "humantime_serde")] std::time::Duration);

impl From<std::time::Duration> for Duration {
	fn from(value: std::time::Duration) -> Self {
		Self(value)
	}
}

impl From<Duration> for std::time::Duration {
	fn from(value: Duration) -> Self {
		value.0
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

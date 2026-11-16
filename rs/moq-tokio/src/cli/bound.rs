//! A duration limit that can be turned off.

use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

/// TODO(next release): delete. A zero backoff `max` or `timeout` turned the bound off
/// until `none` took that job. Set when a flag or config key spells a zero, so
/// [`warn_zero`] warns a setting written for the old meaning. Parsing runs before
/// logging is up, so the warning waits for the client; an API caller's deliberate
/// zero is never parsed and stays quiet.
static ZERO_PARSED: AtomicBool = AtomicBool::new(false);

/// Warn once per process, if a zero was parsed, that the `zero` keys no longer turn their bound off.
pub(crate) fn warn_zero(zero: &[String]) {
	if !zero.is_empty() && ZERO_PARSED.swap(false, Ordering::Relaxed) {
		let zero = zero.join(" and ");
		tracing::warn!("a zero {zero} no longer turns the bound off; write \"none\" for no bound");
	}
}

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
			value => {
				let value = humantime::parse_duration(value)?;
				// TODO(next release): delete, with `ZERO_PARSED`.
				if value.is_zero() {
					ZERO_PARSED.store(true, Ordering::Relaxed);
				}
				Ok(value.into())
			}
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

use std::str::FromStr;

use rand::RngExt;
use serde::{Deserialize, Serialize};

/// An inclusive `[min, max]` range that is sampled per connection.
///
/// Accepts several spellings so configs stay terse:
/// - a scalar `30` (both bounds equal)
/// - a string `"24:60"` or `"24-60"` (CLI flags and TOML strings)
/// - a table `{ min = 24, max = 60 }` (TOML sections)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RangeRepr")]
pub struct Range {
	pub min: u64,
	pub max: u64,
}

impl Range {
	pub const fn new(min: u64, max: u64) -> Self {
		Self { min, max }
	}

	/// Roll a value in `[min, max]`. Bounds are clamped if min > max.
	pub fn sample(&self, rng: &mut impl RngExt) -> u64 {
		let (lo, hi) = if self.min <= self.max {
			(self.min, self.max)
		} else {
			(self.max, self.min)
		};
		if lo == hi { lo } else { rng.random_range(lo..=hi) }
	}
}

impl From<u64> for Range {
	fn from(value: u64) -> Self {
		Self::new(value, value)
	}
}

impl FromStr for Range {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let s = s.trim();
		// Try the range separators in order, falling back to a single scalar.
		let sep = ["..", ":", "-"].into_iter().find(|sep| s.contains(sep));
		match sep {
			Some(sep) => {
				let (min, max) = s.split_once(sep).expect("separator present");
				let min = min.trim().parse().map_err(|_| format!("invalid min in range: {s:?}"))?;
				let max = max.trim().parse().map_err(|_| format!("invalid max in range: {s:?}"))?;
				Ok(Self::new(min, max))
			}
			None => {
				let value = s.parse().map_err(|_| format!("invalid value: {s:?}"))?;
				Ok(Self::new(value, value))
			}
		}
	}
}

/// Parses the various accepted spellings of a [`Range`] on input only.
#[derive(Deserialize)]
#[serde(untagged)]
enum RangeRepr {
	Scalar(u64),
	Text(String),
	Pair { min: u64, max: u64 },
}

impl TryFrom<RangeRepr> for Range {
	type Error = String;

	fn try_from(repr: RangeRepr) -> Result<Self, Self::Error> {
		match repr {
			RangeRepr::Scalar(value) => Ok(Self::new(value, value)),
			RangeRepr::Text(text) => text.parse(),
			RangeRepr::Pair { min, max } => Ok(Self::new(min, max)),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_scalar_and_ranges() {
		assert_eq!("30".parse::<Range>().unwrap(), Range::new(30, 30));
		assert_eq!("24:60".parse::<Range>().unwrap(), Range::new(24, 60));
		assert_eq!("24-60".parse::<Range>().unwrap(), Range::new(24, 60));
		assert_eq!("24..60".parse::<Range>().unwrap(), Range::new(24, 60));
		assert!("nope".parse::<Range>().is_err());
	}

	#[test]
	fn deserialize_all_forms() {
		#[derive(Deserialize)]
		struct Wrap {
			v: Range,
		}
		assert_eq!(toml::from_str::<Wrap>("v = 30").unwrap().v, Range::new(30, 30));
		assert_eq!(toml::from_str::<Wrap>(r#"v = "24:60""#).unwrap().v, Range::new(24, 60));
		assert_eq!(
			toml::from_str::<Wrap>("v = { min = 24, max = 60 }").unwrap().v,
			Range::new(24, 60)
		);
	}

	#[test]
	fn sample_within_bounds() {
		let mut rng = rand::rng();
		let range = Range::new(5, 10);
		for _ in 0..100 {
			let v = range.sample(&mut rng);
			assert!((5..=10).contains(&v));
		}
		// Inverted bounds (min > max) are normalized, not panicked on.
		for _ in 0..100 {
			let v = Range::new(10, 5).sample(&mut rng);
			assert!((5..=10).contains(&v));
		}
		assert_eq!(Range::new(7, 7).sample(&mut rng), 7);
	}
}

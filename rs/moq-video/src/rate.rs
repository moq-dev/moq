//! Exact video frame rates.

use std::cmp::Ordering;
use std::fmt;
use std::time::Duration;

/// The largest supported video rate, in frames per second.
pub const MAX_FRAMES_PER_SECOND: u32 = 1_000_000;

/// An invalid video frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RateError {
	/// The numerator or denominator was zero.
	#[error("frame rate numerator and denominator must be non-zero")]
	Zero,
	/// The ratio exceeds the supported range.
	#[error("frame rate must not exceed {MAX_FRAMES_PER_SECOND} frames per second")]
	TooLarge,
	/// A floating-point catalog rate was not finite and positive.
	#[error("frame rate must be finite and positive")]
	InvalidFloat,
}

/// An exact positive video frame rate, in frames per second.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Rate {
	numerator: u32,
	denominator: u32,
}

impl Rate {
	/// Construct an exact rate from a numerator and denominator.
	pub fn new(numerator: u32, denominator: u32) -> Result<Self, RateError> {
		if numerator == 0 || denominator == 0 {
			return Err(RateError::Zero);
		}
		if u64::from(numerator) > u64::from(MAX_FRAMES_PER_SECOND) * u64::from(denominator) {
			return Err(RateError::TooLarge);
		}
		let divisor = gcd(numerator, denominator);
		Ok(Self {
			numerator: numerator / divisor,
			denominator: denominator / divisor,
		})
	}

	/// Approximate a finite catalog rate using 32-bit rational components.
	pub fn from_f64(value: f64) -> Result<Self, RateError> {
		if !value.is_finite() || value <= 0.0 {
			return Err(RateError::InvalidFloat);
		}
		if value > f64::from(MAX_FRAMES_PER_SECOND) {
			return Err(RateError::TooLarge);
		}
		// Continued fractions recover conventional rates such as 30000/1001 from
		// their JSON floating-point representation without inventing a decimal timebase.
		let (mut input, mut n0, mut d0, mut n1, mut d1) = (value, 0u64, 1u64, 1u64, 0u64);
		loop {
			let whole = input.floor() as u64;
			let Some(n2) = whole.checked_mul(n1).and_then(|v| v.checked_add(n0)) else {
				break;
			};
			let Some(d2) = whole.checked_mul(d1).and_then(|v| v.checked_add(d0)) else {
				break;
			};
			if n2 > u64::from(u32::MAX) || d2 > u64::from(u32::MAX) {
				break;
			}
			(n0, d0, n1, d1) = (n1, d1, n2, d2);
			let fraction = input - whole as f64;
			if fraction < 1e-12 || (n1 as f64 / d1 as f64 - value).abs() < 1e-12 {
				break;
			}
			input = 1.0 / fraction;
		}
		Self::new(n1 as u32, d1 as u32)
	}

	/// The frames-per-second numerator.
	pub fn numerator(self) -> u32 {
		self.numerator
	}

	/// The frames-per-second denominator.
	pub fn denominator(self) -> u32 {
		self.denominator
	}

	/// Convert the exact rate to a floating-point catalog value.
	pub fn as_f64(self) -> f64 {
		f64::from(self.numerator) / f64::from(self.denominator)
	}

	/// Round to the nearest whole frame rate for integer-only platform APIs.
	pub fn rounded(self) -> u32 {
		let numerator = u64::from(self.numerator);
		let denominator = u64::from(self.denominator);
		u32::try_from((numerator + denominator / 2) / denominator)
			.unwrap_or(MAX_FRAMES_PER_SECOND)
			.max(1)
	}

	/// Number of frames in `duration`, rounded to the nearest frame.
	pub fn frames(self, duration: Duration) -> u32 {
		let nanos = duration.as_nanos();
		let scaled = nanos.saturating_mul(u128::from(self.numerator));
		let divisor = 1_000_000_000u128 * u128::from(self.denominator);
		u32::try_from((scaled + divisor / 2) / divisor).unwrap_or(u32::MAX)
	}

	#[cfg(feature = "capture")]
	pub(crate) const fn integer(value: u32) -> Self {
		Self {
			numerator: value,
			denominator: 1,
		}
	}
}

impl Ord for Rate {
	fn cmp(&self, other: &Self) -> Ordering {
		(u64::from(self.numerator) * u64::from(other.denominator))
			.cmp(&(u64::from(other.numerator) * u64::from(self.denominator)))
	}
}

impl PartialOrd for Rate {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl fmt::Display for Rate {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}/{}", self.numerator, self.denominator)
	}
}

const fn gcd(mut left: u32, mut right: u32) -> u32 {
	while right != 0 {
		let remainder = left % right;
		left = right;
		right = remainder;
	}
	left
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn preserves_broadcast_rates() {
		assert_eq!(
			Rate::new(30_000, 1_001).unwrap(),
			Rate::from_f64(30_000.0 / 1_001.0).unwrap()
		);
		assert_eq!(
			Rate::new(60_000, 1_001).unwrap(),
			Rate::from_f64(60_000.0 / 1_001.0).unwrap()
		);
	}

	#[test]
	fn validates_components_and_range() {
		assert_eq!(Rate::new(30, 0), Err(RateError::Zero));
		assert_eq!(Rate::new(0, 1), Err(RateError::Zero));
		assert_eq!(Rate::new(MAX_FRAMES_PER_SECOND + 1, 1), Err(RateError::TooLarge));
	}

	#[test]
	fn rejects_invalid_floats() {
		assert_eq!(Rate::from_f64(f64::NAN), Err(RateError::InvalidFloat));
		assert_eq!(Rate::from_f64(f64::INFINITY), Err(RateError::InvalidFloat));
		assert_eq!(Rate::from_f64(f64::NEG_INFINITY), Err(RateError::InvalidFloat));
		assert_eq!(Rate::from_f64(0.0), Err(RateError::InvalidFloat));
		assert_eq!(Rate::from_f64(-30.0), Err(RateError::InvalidFloat));
		assert_eq!(
			Rate::from_f64(f64::from(MAX_FRAMES_PER_SECOND) + 1.0),
			Err(RateError::TooLarge)
		);
	}
}

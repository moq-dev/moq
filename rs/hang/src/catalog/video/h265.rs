use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::Error;

/// H.265/HEVC codec mimetype.
///
/// This struct contains the profile, tier, level, and constraint information
/// needed to identify a specific H.265 variant. The `in_band` flag determines
/// whether parameter sets are included in-band (hev1) or out-of-band (hvc1).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct H265 {
	/// If true (hev1), then the SPS/PPS/etc are in the same NAL unit as the IDR.
	/// If false (hvc1), then the SPS/PPS/etc are in the description.
	pub in_band: bool,

	/// Profile space (0 for main profile space, 1-3 for other spaces)
	/// If 0, then no character. Otherwise, A for 1, B for 2, C for 3, etc.
	pub profile_space: u8,
	/// Profile IDC identifying the profile
	pub profile_idc: u8,

	/// The raw 32-bit `general_profile_compatibility_flags` in big-endian byte order,
	/// exactly as the bitstream and the `hvcC` box carry them (`flag[0]` is the most
	/// significant bit of the first byte).
	///
	/// RFC 6381 renders this bit-reversed, so raw `[0x60, 0, 0, 0]` prints as `6`.
	pub profile_compatibility_flags: [u8; 4],

	/// Tier flag: false = 'L' (Low), true = 'H' (High)
	/// 0 = 'L', 1 = 'H'
	pub tier_flag: bool,
	/// Level IDC identifying the level
	pub level_idc: u8,

	/// Constraint indicator flags (hex encoded, trailing zeros may be omitted)
	/// Hex encoded, trailing zeros may be omitted.
	pub constraint_flags: [u8; 6],
}

impl fmt::Display for H265 {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		// RFC 6381 section 3.3: the 32 bits of general_profile_compatibility_flags in
		// REVERSE BIT order (flag[31] first), hex encoded, leading zeros omitted. That
		// is a bit reversal of the whole 32-bit value, not a byte swap.
		let compatibility = format!(
			"{:X}",
			u32::from_be_bytes(self.profile_compatibility_flags).reverse_bits()
		);

		// Constraint flags: hex encoded, trailing-zero bytes omitted. When every
		// byte is zero the component is omitted ENTIRELY (carry its own leading
		// '.' only when non-empty); otherwise the string ends in a dangling '.'
		// and an empty final field that `FromStr` rejects ("expected int"). This
		// happens for streams whose general_constraint_indicator_flags are all
		// zero (e.g. some IP-camera HEVC encoders).
		let significant = self.constraint_flags.iter().rev().skip_while(|b| **b == 0).count();
		let constraints = if significant == 0 {
			String::new()
		} else {
			let body = self
				.constraint_flags
				.iter()
				.take(significant)
				.map(|b| format!("{b:X}"))
				.collect::<Vec<_>>()
				.join(".");
			format!(".{body}")
		};

		write!(
			f,
			"{}.{}{}.{}.{}{}{}",
			match self.in_band {
				true => "hev1",
				false => "hvc1",
			},
			match self.profile_space {
				0 => "".to_string(),
				n => (b'A'.saturating_add(n).saturating_sub(1) as char).to_string(),
			},
			self.profile_idc,
			compatibility,
			match self.tier_flag {
				true => "H",
				false => "L",
			},
			self.level_idc,
			constraints,
		)
	}
}

impl FromStr for H265 {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut parts = s.split('.');

		let in_band = match parts.next() {
			Some("hev1") => true,
			Some("hvc1") => false,
			_ => return Err(Error::InvalidCodec),
		};

		let profile = parts.next().ok_or(Error::InvalidCodec)?;
		let profile_space = match profile.as_bytes().first().ok_or(Error::InvalidCodec)? {
			b'A'..=b'Z' => 1 + profile.as_bytes()[0] - b'A',
			_ => 0,
		};
		let profile_idc = (if profile_space > 0 { &profile[1..] } else { profile }).parse::<u8>()?;

		let compatibility = parts.next().ok_or(Error::InvalidCodec)?;
		let profile_compatibility_flags = u32::from_str_radix(compatibility, 16)?.reverse_bits().to_be_bytes();

		let level = parts.next().ok_or(Error::InvalidCodec)?;

		let tier_flag = match level.as_bytes().first() {
			Some(b'H') => true,
			Some(b'L') => false,
			_ => return Err(Error::InvalidCodec),
		};

		let level_idc = level[1..].parse::<u8>()?;

		let mut constraint_flags = [0u8; 6];

		let parts = parts.enumerate();
		for (i, constraint) in parts {
			// Tolerate an empty trailing field (a dangling '.') emitted by older
			// encoders/muxers so we still parse their streams once this is fixed.
			if constraint.is_empty() {
				continue;
			}
			if i >= 6 {
				return Err(Error::InvalidCodec);
			}

			constraint_flags[i] = u8::from_str_radix(constraint, 16)?;
		}

		Ok(Self {
			in_band,
			profile_space,
			profile_idc,
			profile_compatibility_flags,
			tier_flag,
			level_idc,
			constraint_flags,
		})
	}
}

#[cfg(test)]
mod tests {
	use crate::catalog::VideoCodec;

	use super::*;

	#[test]
	fn test_h265() {
		// Raw 0x60000000 (Main + Main10 compatible) renders bit-reversed as "6".
		let encoded = "hev1.1.6.L93.B0";
		let decoded = H265 {
			in_band: true,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0x60, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0xB0, 0, 0, 0, 0, 0],
		}
		.into();

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_h265_long() {
		let encoded = "hev1.A4.41.H120.B0.23";
		let decoded = H265 {
			in_band: true,
			profile_space: 1,
			profile_idc: 4,
			// Raw 0x82000000 renders bit-reversed as "41".
			profile_compatibility_flags: [0x82, 0, 0, 0],
			tier_flag: true,
			level_idc: 120,
			constraint_flags: [0xB0, 0x23, 0, 0, 0, 0],
		};

		let output = H265::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_h265_out_of_band() {
		let encoded = "hvc1.A1.6.H93.B0";
		let output = H265::from_str(encoded).unwrap();
		assert!(!output.in_band);
		assert_eq!(output.profile_compatibility_flags, [0x60, 0, 0, 0]);

		let output = output.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_h265_zero_constraints() {
		// All-zero general_constraint_indicator_flags (common in live HEVC from
		// hardware encoders): the constraint component must be omitted entirely.
		// No trailing '.', and it must round-trip.
		let decoded = H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0x60, 0, 0, 0],
			tier_flag: false,
			level_idc: 150,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		};
		assert_eq!(decoded.to_string(), "hvc1.1.6.L150");
		assert_eq!(H265::from_str("hvc1.1.6.L150").unwrap(), decoded);
		// And tolerate the dangling-'.' form older muxers emitted for the same stream.
		assert_eq!(H265::from_str("hvc1.1.6.L150.").unwrap(), decoded);
	}

	#[test]
	fn all_zero_compatibility_round_trips() {
		// Placeholder configs carry no flags at all. The component must still be a
		// parseable "0" rather than empty, or we emit a string we can't read back.
		let decoded = H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		};
		assert_eq!(decoded.to_string(), "hvc1.1.0.L93");
		assert_eq!(H265::from_str("hvc1.1.0.L93").unwrap(), decoded);
	}

	#[test]
	fn multi_byte_compatibility_round_trips() {
		// Flags spanning more than one byte: the reversal is over the whole 32-bit
		// value, and intra-value zero nibbles must survive.
		let decoded = H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0x60, 0x80, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		};
		assert_eq!(decoded.to_string(), "hvc1.1.106.L93");
		assert_eq!(H265::from_str("hvc1.1.106.L93").unwrap(), decoded);
	}
}

use serde::{Deserialize, Serialize};

use crate::Error;

/// VP9 codec configuration.
///
/// This struct contains profile, level, bit depth, and color space information
/// for VP9 video streams. VP9 supports flexible color formats and bit depths.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VP9 {
	/// VP9 profile (0-3, determines feature support)
	pub profile: u8,
	/// VP9 level (determines resolution and bitrate constraints)
	pub level: u8,
	/// Bit depth (8, 10, or 12 bits per sample)
	pub bit_depth: u8,
	/// Chroma subsampling format
	pub chroma_subsampling: u8,
	/// Color primaries specification
	pub color_primaries: u8,
	/// Transfer characteristics (gamma curve)
	pub transfer_characteristics: u8,
	/// Matrix coefficients for color conversion
	pub matrix_coefficients: u8,
	/// Whether video uses full range (true) or limited range (false)
	pub full_range: bool,
}

// vp09.<profile>.<level>.<bitDepth>.<chromaSubsampling>.
// <colourPrimaries>.<transferCharacteristics>.<matrixCoefficients>.<videoFullRangeFlag>
impl std::fmt::Display for VP9 {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "vp09.{:02}.{:02}.{:02}", self.profile, self.level, self.bit_depth)?;

		let short = VP9 {
			profile: self.profile,
			level: self.level,
			bit_depth: self.bit_depth,
			..Default::default()
		};

		if self == &short {
			return Ok(());
		}

		write!(
			f,
			".{:02}.{:02}.{:02}.{:02}.{:02}",
			self.chroma_subsampling,
			self.color_primaries,
			self.transfer_characteristics,
			self.matrix_coefficients,
			self.full_range as u8,
		)
	}
}

impl std::str::FromStr for VP9 {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let parts = s
			.strip_prefix("vp09.")
			.ok_or(Error::InvalidCodec)?
			.split('.')
			.map(u8::from_str)
			.collect::<Result<Vec<_>, _>>()?;

		if parts.len() < 3 {
			return Err(Error::InvalidCodec);
		}

		let mut vp9 = VP9 {
			profile: parts[0],
			level: parts[1],
			bit_depth: parts[2],
			..Default::default()
		};

		if parts.len() == 3 {
			return Ok(vp9);
		} else if parts.len() != 8 {
			return Err(Error::InvalidCodec);
		}

		vp9.chroma_subsampling = parts[3];
		vp9.color_primaries = parts[4];
		vp9.transfer_characteristics = parts[5];
		vp9.matrix_coefficients = parts[6];
		vp9.full_range = parts[7] == 1;

		Ok(vp9)
	}
}

impl Default for VP9 {
	fn default() -> Self {
		Self {
			profile: 0,
			level: 0,
			bit_depth: 0,
			chroma_subsampling: 1,
			color_primaries: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		}
	}
}

#[cfg(test)]
mod test {
	use std::str::FromStr;

	use crate::VideoCodec;

	use super::*;

	#[test]
	fn test_vp9() {
		let encoded = "vp09.02.10.10.01.09.16.09.01";
		let decoded = VP9 {
			profile: 2,
			level: 10,
			bit_depth: 10,
			chroma_subsampling: 1,
			color_primaries: 9,
			transfer_characteristics: 16,
			matrix_coefficients: 9,
			full_range: true,
		}
		.into();

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_vp9_short() {
		let encoded = "vp09.00.41.08";
		let decoded = VP9 {
			profile: 0,
			level: 41,
			bit_depth: 8,
			..Default::default()
		}
		.into();

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}
}

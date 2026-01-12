use serde::{Deserialize, Serialize};

use crate::Error;

/// AV1 codec mimetype.
///
/// This struct contains profile, level, tier, bit depth, and color space information
/// for AV1 video streams. AV1 is a modern codec supporting high efficiency and
/// advanced features like HDR and wide color gamuts.
///
/// Reference: <https://aomediacodec.github.io/av1-isobmff/#codecsparam>
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AV1 {
	/// AV1 profile (0-2, determines feature support)
	pub profile: u8,
	/// AV1 level (determines resolution and bitrate constraints)
	pub level: u8,
	/// Tier ('M' for Main, 'H' for High)
	pub tier: char,
	/// Bit depth (8, 10, or 12 bits per sample)
	pub bitdepth: u8,
	/// Whether the stream is monochrome (grayscale)
	pub mono_chrome: bool,
	/// Horizontal chroma subsampling
	pub chroma_subsampling_x: bool,
	/// Vertical chroma subsampling
	pub chroma_subsampling_y: bool,
	/// Chroma sample position
	pub chroma_sample_position: u8,
	/// Color primaries specification
	pub color_primaries: u8,
	/// Transfer characteristics (gamma curve)
	pub transfer_characteristics: u8,
	/// Matrix coefficients for color conversion
	pub matrix_coefficients: u8,
	/// Whether video uses full range (true) or limited range (false)
	pub full_range: bool,
}

impl AV1 {
	pub const PREFIX: &'static str = "av01";
}

// av01.<profile>.<level><tier>.<bitDepth>.<monochrome>.<chromaSubsampling>.
// <colorPrimaries>.<transferCharacteristics>.<matrixCoefficients>.<videoFullRangeFlag>
impl std::fmt::Display for AV1 {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"av01.{:01}.{:02}{}.{:02}",
			self.profile, self.level, self.tier, self.bitdepth
		)?;

		let short = AV1 {
			profile: self.profile,
			level: self.level,
			tier: self.tier,
			bitdepth: self.bitdepth,
			..Default::default()
		};

		if self == &short {
			return Ok(());
		}

		write!(
			f,
			".{:01}.{}{}{}.{:02}.{:02}.{:02}.{:01}",
			self.mono_chrome as u8,
			self.chroma_subsampling_x as u8,
			self.chroma_subsampling_y as u8,
			{ self.chroma_sample_position },
			self.color_primaries,
			self.transfer_characteristics,
			self.matrix_coefficients,
			self.full_range as u8,
		)
	}
}

lazy_static::lazy_static! {
	static ref AV1_REGEX: regex::Regex = regex::Regex::new(&r#"
		^av01
		\.(?<profile>[01])
		\.(?<level>\d{2})(?<tier>\w)
		\.(?<bitdepth>\d{2})
		(?<extra>
			\.(?<mono>\d)
			\.(?<chroma_subsampling_x>[01])(?<chroma_subsampling_y>[01])(?<chroma_sample_position>[0-3])
			\.(?<color>\d{2})
			\.(?<transfer>\d{2})
			\.(?<matrix>\d{2})
			\.(?<full>[01])
		)?
		$"#.replace(['\n', ' ', '\t'], "")
	).unwrap();
}

impl std::str::FromStr for AV1 {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let captures = AV1_REGEX.captures(s).ok_or(Error::InvalidCodec)?;

		let mut av1 = AV1 {
			profile: u8::from_str(&captures["profile"])?,
			level: u8::from_str(&captures["level"])?,
			tier: captures["tier"].chars().next().unwrap(),
			bitdepth: u8::from_str(&captures["bitdepth"])?,
			..Default::default()
		};

		if captures.name("extra").is_none() {
			return Ok(av1);
		}

		av1.mono_chrome = &captures["mono"] == "1";
		av1.chroma_subsampling_x = &captures["chroma_subsampling_x"] == "1";
		av1.chroma_subsampling_y = &captures["chroma_subsampling_y"] == "1";
		av1.chroma_sample_position = u8::from_str(&captures["chroma_sample_position"])?;
		av1.color_primaries = u8::from_str(&captures["color"])?;
		av1.transfer_characteristics = u8::from_str(&captures["transfer"])?;
		av1.matrix_coefficients = u8::from_str(&captures["matrix"])?;
		av1.full_range = &captures["full"] == "1";

		Ok(av1)
	}
}

impl Default for AV1 {
	fn default() -> Self {
		// .0.110.01.01.01.0
		Self {
			profile: 0,
			level: 0,
			tier: 'M',
			bitdepth: 8,
			mono_chrome: false,
			chroma_subsampling_x: true,
			chroma_subsampling_y: true,
			chroma_sample_position: 0,
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
	fn test_av1() {
		let encoded = "av01.0.04M.10.0.112.09.16.09.0";
		let decoded = AV1 {
			profile: 0,
			level: 4,
			tier: 'M',
			bitdepth: 10,
			mono_chrome: false,
			chroma_subsampling_x: true,
			chroma_subsampling_y: true,
			chroma_sample_position: 2,
			color_primaries: 9,
			transfer_characteristics: 16,
			matrix_coefficients: 9,
			full_range: false,
		}
		.into();

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_av1_short() {
		let encoded = "av01.0.01M.08";
		let decoded = AV1 {
			profile: 0,
			level: 1,
			tier: 'M',
			bitdepth: 8,
			..Default::default()
		}
		.into();

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}
}

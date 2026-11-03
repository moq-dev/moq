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
	/// The codec string prefix that identifies AV1.
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
			u8::from(self.mono_chrome),
			u8::from(self.chroma_subsampling_x),
			u8::from(self.chroma_subsampling_y),
			{ self.chroma_sample_position },
			self.color_primaries,
			self.transfer_characteristics,
			self.matrix_coefficients,
			u8::from(self.full_range),
		)
	}
}

impl std::str::FromStr for AV1 {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		fn number(field: Option<&str>, width: usize, max: u8) -> Result<u8, Error> {
			let field = field.filter(|field| field.len() == width).ok_or(Error::InvalidCodec)?;
			let value = crate::catalog::codec_number(field, 10)?;
			if value > max {
				return Err(Error::InvalidCodec);
			}
			Ok(value)
		}

		let mut fields = s.strip_prefix("av01.").ok_or(Error::InvalidCodec)?.split('.');
		let profile = number(fields.next(), 1, 2)?;
		let [tens @ b'0'..=b'9', ones @ b'0'..=b'9', tier @ (b'M' | b'H')] =
			fields.next().ok_or(Error::InvalidCodec)?.as_bytes()
		else {
			return Err(Error::InvalidCodec);
		};
		let mut av1 = AV1 {
			profile,
			level: (tens - b'0') * 10 + (ones - b'0'),
			tier: char::from(*tier),
			bitdepth: number(fields.next(), 2, 99)?,
			..Default::default()
		};

		let Some(mono) = fields.next() else {
			return Ok(av1);
		};
		av1.mono_chrome = number(Some(mono), 1, 1)? == 1;
		let [x @ b'0'..=b'1', y @ b'0'..=b'1', position @ b'0'..=b'3'] =
			fields.next().ok_or(Error::InvalidCodec)?.as_bytes()
		else {
			return Err(Error::InvalidCodec);
		};
		av1.chroma_subsampling_x = *x == b'1';
		av1.chroma_subsampling_y = *y == b'1';
		av1.chroma_sample_position = position - b'0';
		av1.color_primaries = number(fields.next(), 2, 99)?;
		av1.transfer_characteristics = number(fields.next(), 2, 99)?;
		av1.matrix_coefficients = number(fields.next(), 2, 99)?;
		av1.full_range = number(fields.next(), 1, 1)? == 1;
		if fields.next().is_some() {
			return Err(Error::InvalidCodec);
		}

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

	use crate::catalog::{VideoCodec, VideoConfig};

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

	#[test]
	fn test_av1_profiles_roundtrip() {
		for (encoded, profile) in [
			("av01.0.00M.08", 0),
			("av01.1.00M.10.0.000.01.01.01.0", 1),
			("av01.2.00M.12", 2),
			("av01.2.00M.08.0.100.02.02.02.0", 2),
			("av01.2.00M.10.0.100.09.16.09.0", 2),
			("av01.2.00M.12.0.100.09.16.09.0", 2),
			("av01.2.00M.12.0.000.09.16.09.0", 2),
		] {
			let decoded = VideoCodec::from_str(encoded).expect("failed to parse");
			assert!(matches!(&decoded, VideoCodec::AV1(av1) if av1.profile == profile));
			assert_eq!(decoded.to_string(), encoded);

			let config = VideoConfig::new(decoded);
			let json = serde_json::to_string(&config).unwrap();
			let output: VideoConfig = serde_json::from_str(&json).unwrap();
			assert_eq!(output, config);
		}
	}

	#[test]
	fn field_boundaries() {
		for encoded in [
			"av01.0.04M.08.",
			"av01.0.04M.08.0.110",
			"av01.0.04M.08.0.110.01.01.01.0.extra",
			"av01.0.04M.08.2.110.01.01.01.0",
			"av01.0.04M.08.0.210.01.01.01.0",
			"av01.0.04M.08.0.114.01.01.01.0",
			"av01.0.04M.08.0.110.01.01.01.2",
			"av01.0.04M.+8",
			"av01.0.04M.٠٨",
			"av01.0.04M.08\n",
			"av01.0.04M.08.0.110.1.01.01.0",
		] {
			assert!(AV1::from_str(encoded).is_err(), "{encoded:?}");
		}
	}

	#[test]
	fn test_av1_invalid_profile() {
		for profile in 3..=9 {
			assert!(VideoCodec::from_str(&format!("av01.{profile}.00M.08")).is_err());
		}
	}
}

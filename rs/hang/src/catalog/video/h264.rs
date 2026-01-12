use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::Error;

/// H.264/AVC codec mimetype.
///
/// This struct contains the profile, constraints, and level information
/// needed to identify a specific H.264 variant. These parameters determine
/// the features and complexity allowed in the encoded stream.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct H264 {
	/// If true, SPS/PPS are inline in the bitstream (avc3).
	/// If false, they must be in the description (avc1).
	#[serde(default)]
	pub inline: bool,
	/// The H.264 profile (e.g., 0x42 for Baseline, 0x4D for Main, 0x64 for High)
	pub profile: u8,
	/// Profile compatibility flags and constraints
	pub constraints: u8,
	/// The H.264 level (e.g., 0x1F for Level 3.1, 0x28 for Level 4.0)
	pub level: u8,
}

impl fmt::Display for H264 {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let prefix = if self.inline { "avc3" } else { "avc1" };
		write!(
			f,
			"{}.{:02x}{:02x}{:02x}",
			prefix, self.profile, self.constraints, self.level
		)
	}
}

impl FromStr for H264 {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut parts = s.split('.');
		let inline = match parts.next() {
			Some("avc1") => false,
			Some("avc3") => true,
			_ => return Err(Error::InvalidCodec),
		};

		let part = parts.next().ok_or(Error::InvalidCodec)?;
		if part.len() != 6 {
			return Err(Error::InvalidCodec);
		}

		Ok(Self {
			profile: u8::from_str_radix(&part[0..2], 16)?,
			constraints: u8::from_str_radix(&part[2..4], 16)?,
			level: u8::from_str_radix(&part[4..6], 16)?,
			inline,
		})
	}
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use crate::VideoCodec;

	use super::*;

	#[test]
	fn test_h264_avc1() {
		let encoded = "avc1.42c01e";
		let decoded = H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1e,
			inline: false,
		};

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, VideoCodec::H264(decoded.clone()));

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}

	#[test]
	fn test_h264_avc3() {
		let encoded = "avc3.42c01e";
		let decoded = H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1e,
			inline: true,
		};

		let output = VideoCodec::from_str(encoded).expect("failed to parse");
		assert_eq!(output, VideoCodec::H264(decoded.clone()));

		let output = decoded.to_string();
		assert_eq!(output, encoded);
	}
}

use crate::Error;

use serde::{Deserialize, Serialize};

/// AAC codec mimetype.
///
/// This struct contains the profile information for AAC audio streams.
/// AAC supports multiple profiles with different complexity and quality levels.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AAC {
	/// AAC profile (e.g., 2 for LC, 5 for HE-AAC, 29 for HE-AACv2)
	pub profile: u8,
	// TODO:
	// freq_index
	// chan_conf
}

impl std::fmt::Display for AAC {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "mp4a.40.{}", self.profile)
	}
}

impl std::str::FromStr for AAC {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let remain = s.strip_prefix("mp4a.40.").ok_or(Error::InvalidCodec)?;
		Ok(Self {
			profile: crate::catalog::codec_number(remain, 10)?,
		})
	}
}

#[cfg(test)]
mod test {
	use std::str::FromStr;

	use super::*;

	#[test]
	fn test_aac() {
		let encoded = "mp4a.40.2";
		let decoded = AAC { profile: 2 };

		let output = AAC::from_str(encoded).expect("failed to parse AAC string");
		assert_eq!(output, decoded);

		let output = decoded.to_string();
		assert_eq!(output, encoded);

		// The WebCodecs AAC registration spells each object type with and without a leading zero.
		assert_eq!(AAC::from_str("mp4a.40.05").unwrap(), AAC { profile: 5 });
		assert_eq!(AAC::from_str("mp4a.40.02").unwrap(), AAC { profile: 2 });
	}
}

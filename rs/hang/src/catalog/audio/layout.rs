use std::str::FromStr;

use derive_more::Display;

/// What a rendition's decoded channels carry, beyond the codec's own channel order.
///
/// Absent means the codec's channel order. An unrecognized value decodes to
/// [`AudioLayout::Unknown`] instead of failing, and a consumer treats it as absent.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
#[non_exhaustive]
pub enum AudioLayout {
	/// Full-sphere ambisonics in AmbiX: ACN channel order with SN3D normalization.
	///
	/// The order follows from the channel count, `(order + 1)²`, so the count is a square of at least 4.
	#[display("ambisonic")]
	Ambisonic,

	/// A layout this build does not recognize, preserved verbatim.
	#[display("{_0}")]
	Unknown(String),
}

impl AudioLayout {
	/// Refuse a channel count this layout cannot carry.
	pub fn check(&self, channel_count: u32) -> crate::Result<()> {
		if *self == Self::Ambisonic {
			let order = channel_count.isqrt();
			if order < 2 || order * order != channel_count {
				return Err(crate::Error::InvalidAmbisonicChannels(channel_count));
			}
		}
		Ok(())
	}
}

impl FromStr for AudioLayout {
	type Err = crate::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"ambisonic" => Self::Ambisonic,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

#[cfg(test)]
mod test {
	use crate::catalog::{AudioCodec, AudioConfig};

	use super::*;

	#[test]
	fn round_trips() {
		for (layout, json) in [
			(AudioLayout::Ambisonic, "ambisonic"),
			(AudioLayout::Unknown("surround".to_string()), "surround"),
		] {
			let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 4);
			config.layout = Some(layout.clone());
			let encoded = serde_json::to_value(&config).unwrap();
			assert_eq!(encoded["layout"], json);
			assert_eq!(
				serde_json::from_value::<AudioConfig>(encoded).unwrap().layout,
				Some(layout)
			);
		}
	}

	#[test]
	fn ambisonic_needs_a_square_channel_count() {
		for channels in [4, 9, 16, 225] {
			AudioLayout::Ambisonic.check(channels).unwrap();
		}
		for channels in [0, 1, 2, 6, 8] {
			assert!(matches!(
				AudioLayout::Ambisonic.check(channels),
				Err(crate::Error::InvalidAmbisonicChannels(c)) if c == channels
			));
		}
		// An unknown layout puts no constraint on the channel count.
		AudioLayout::Unknown("surround".to_string()).check(6).unwrap();
	}
}

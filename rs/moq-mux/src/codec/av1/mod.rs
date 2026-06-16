//! AV1.
//!
//! Maps the AV1CodecConfigurationRecord (av1C) flag bits into the
//! catalog's AV1 codec struct, and provides an [`Import`] that publishes
//! raw AV1 bitstreams (OBU-framed) to a moq broadcast.

mod import;
mod split;

pub use import::*;
pub use split::*;

use hang::catalog::AV1;

/// AV1 parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("OBU is too short")]
	ObuTooShort,

	#[error("OBU size too large")]
	ObuSizeTooLarge,

	#[error("not initialized")]
	NotInitialized,

	#[error("expected sequence header before any frames")]
	MissingSequenceHeader,

	#[error("missing timestamp")]
	MissingTimestamp,

	#[error("OBU header parse: {0}")]
	ObuHeaderParse(std::sync::Arc<std::io::Error>),
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::ObuHeaderParse(std::sync::Arc::new(err))
	}
}

pub type Result<T> = std::result::Result<T, Error>;

/// Map a parsed `mp4_atom::Av1c` (AV1CodecConfigurationRecord) to the
/// hang catalog's AV1 codec struct.
///
/// Fills in profile, level, bit depth, and chroma sampling info. Color/HDR
/// fields default to unspecified.
pub(crate) fn av1_from_av1c(av1c: &mp4_atom::Av1c) -> AV1 {
	AV1 {
		profile: av1c.seq_profile,
		level: av1c.seq_level_idx_0,
		bitdepth: bitdepth(av1c.twelve_bit, av1c.high_bitdepth),
		mono_chrome: av1c.monochrome,
		chroma_subsampling_x: av1c.chroma_subsampling_x,
		chroma_subsampling_y: av1c.chroma_subsampling_y,
		chroma_sample_position: av1c.chroma_sample_position,
		..Default::default()
	}
}

/// Bit depth from the (twelve_bit, high_bitdepth) av1C flag pair
/// (ISO/IEC 14496-15 + av1-isobmff §2.3.3).
///
/// Computes `8 + 2*high_bitdepth + 2*twelve_bit`.
pub(crate) fn bitdepth(twelve_bit: bool, high_bitdepth: bool) -> u8 {
	8 + 2 * u8::from(high_bitdepth) + 2 * u8::from(twelve_bit)
}

#[cfg(test)]
mod tests {
	use super::bitdepth;

	#[test]
	fn maps_bitdepth_flags() {
		assert_eq!(bitdepth(false, false), 8);
		assert_eq!(bitdepth(false, true), 10);
		assert_eq!(bitdepth(true, true), 12);
		// twelve_bit=true with high_bitdepth=false is not a valid combination
		// per the spec, but the additive formula still gives a defined answer.
		assert_eq!(bitdepth(true, false), 10);
	}
}

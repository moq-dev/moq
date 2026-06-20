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

/// Build a catalog [`VideoConfig`](hang::catalog::VideoConfig) for the `av01`
/// shape from an AV1CodecConfigurationRecord (av1C).
///
/// Used by the enhanced-RTMP / FLV importer, where the av1C arrives out of band
/// in the sequence-header tag (leading `0x81` marker) and the coded samples are
/// raw OBU temporal units, so the record passes straight through as the catalog
/// `description`. Resolution and color live in the inline sequence header, not
/// the av1C, so `coded_width`/`coded_height` are left unset here.
pub(crate) fn config_from_av1c(av1c: &[u8]) -> Result<hang::catalog::VideoConfig> {
	// av1C: byte 0 = marker(1)|version(7) = 0x81, byte 1 = seq_profile(3)|seq_level_idx_0(5),
	// byte 2 = seq_tier_0|high_bitdepth|twelve_bit|monochrome|subsampling_x|subsampling_y|sample_position(2).
	if av1c.len() < 4 || av1c[0] != 0x81 {
		return Err(Error::ObuTooShort);
	}
	let high_bitdepth = ((av1c[2] >> 6) & 0x01) == 1;
	let twelve_bit = ((av1c[2] >> 5) & 0x01) == 1;

	let mut config = hang::catalog::VideoConfig::new(AV1 {
		profile: (av1c[1] >> 5) & 0x07,
		level: av1c[1] & 0x1f,
		tier: if ((av1c[2] >> 7) & 0x01) == 1 { 'H' } else { 'M' },
		bitdepth: bitdepth(twelve_bit, high_bitdepth),
		mono_chrome: ((av1c[2] >> 4) & 0x01) == 1,
		chroma_subsampling_x: ((av1c[2] >> 3) & 0x01) == 1,
		chroma_subsampling_y: ((av1c[2] >> 2) & 0x01) == 1,
		chroma_sample_position: av1c[2] & 0x03,
		..Default::default()
	});
	config.description = Some(bytes::Bytes::copy_from_slice(av1c));
	config.container = hang::catalog::Container::Legacy;
	Ok(config)
}

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

/// Build an `mp4_atom::Av1c` (AV1CodecConfigurationRecord) from the hang
/// catalog's AV1 codec struct, the inverse of [`av1_from_av1c`].
///
/// `config_obus` is left empty: moq-video publishes AV1 with the sequence
/// header inline in the bitstream (the `.av01` in-band case, analogous to
/// `hev1`/`avc3`), so the decoder reads it from the keyframe rather than the
/// out-of-band config record. The catalog's color fields (color primaries,
/// transfer characteristics, matrix coefficients, full range) have no slot in
/// av1C; they live in the sequence header OBU instead.
pub(crate) fn av1c_from_av1(av1: &AV1) -> mp4_atom::Av1c {
	let (twelve_bit, high_bitdepth) = bitdepth_flags(av1.bitdepth);
	mp4_atom::Av1c {
		seq_profile: av1.profile,
		seq_level_idx_0: av1.level,
		seq_tier_0: av1.tier == 'H',
		high_bitdepth,
		twelve_bit,
		monochrome: av1.mono_chrome,
		chroma_subsampling_x: av1.chroma_subsampling_x,
		chroma_subsampling_y: av1.chroma_subsampling_y,
		chroma_sample_position: av1.chroma_sample_position,
		initial_presentation_delay: None,
		config_obus: Vec::new(),
	}
}

/// Bit depth from the (twelve_bit, high_bitdepth) av1C flag pair
/// (ISO/IEC 14496-15 + av1-isobmff §2.3.3).
///
/// Computes `8 + 2*high_bitdepth + 2*twelve_bit`.
pub(crate) fn bitdepth(twelve_bit: bool, high_bitdepth: bool) -> u8 {
	8 + 2 * u8::from(high_bitdepth) + 2 * u8::from(twelve_bit)
}

/// The (twelve_bit, high_bitdepth) av1C flag pair for a given bit depth, the
/// inverse of [`bitdepth`]. 8-bit -> (false, false), 10-bit -> (false, true),
/// 12-bit -> (true, true).
pub(crate) fn bitdepth_flags(bitdepth: u8) -> (bool, bool) {
	(bitdepth >= 12, bitdepth >= 10)
}

#[cfg(test)]
mod tests {
	use super::{av1_from_av1c, av1c_from_av1, bitdepth, bitdepth_flags};
	use hang::catalog::AV1;

	#[test]
	fn maps_bitdepth_flags() {
		assert_eq!(bitdepth(false, false), 8);
		assert_eq!(bitdepth(false, true), 10);
		assert_eq!(bitdepth(true, true), 12);
		// twelve_bit=true with high_bitdepth=false is not a valid combination
		// per the spec, but the additive formula still gives a defined answer.
		assert_eq!(bitdepth(true, false), 10);
	}

	#[test]
	fn bitdepth_flags_round_trip() {
		for (depth, flags) in [(8, (false, false)), (10, (false, true)), (12, (true, true))] {
			assert_eq!(bitdepth_flags(depth), flags);
			let (twelve_bit, high_bitdepth) = flags;
			assert_eq!(bitdepth(twelve_bit, high_bitdepth), depth);
		}
	}

	#[test]
	fn av1c_round_trips_catalog_fields() {
		let av1 = AV1 {
			profile: 0,
			level: 8,
			tier: 'H',
			bitdepth: 10,
			mono_chrome: false,
			chroma_subsampling_x: true,
			chroma_subsampling_y: true,
			chroma_sample_position: 2,
			// Color fields have no av1C slot; they live in the sequence header.
			..Default::default()
		};

		let av1c = av1c_from_av1(&av1);
		assert_eq!(av1c.seq_profile, 0);
		assert_eq!(av1c.seq_level_idx_0, 8);
		assert!(av1c.seq_tier_0);
		assert!(av1c.high_bitdepth);
		assert!(!av1c.twelve_bit);
		assert!(av1c.chroma_subsampling_x);
		assert!(av1c.chroma_subsampling_y);
		assert_eq!(av1c.chroma_sample_position, 2);
		assert!(av1c.config_obus.is_empty());

		// The av1C-backed fields survive a round trip back to the catalog.
		let back = av1_from_av1c(&av1c);
		assert_eq!(back.profile, av1.profile);
		assert_eq!(back.level, av1.level);
		assert_eq!(back.bitdepth, av1.bitdepth);
		assert_eq!(back.mono_chrome, av1.mono_chrome);
		assert_eq!(back.chroma_subsampling_x, av1.chroma_subsampling_x);
		assert_eq!(back.chroma_subsampling_y, av1.chroma_subsampling_y);
		assert_eq!(back.chroma_sample_position, av1.chroma_sample_position);
	}
}

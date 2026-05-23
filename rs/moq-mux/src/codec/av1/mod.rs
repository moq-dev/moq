//! AV1 parsing and AV1CodecConfigurationRecord helpers.
//!
//! Centralizes the av1C → catalog [`hang::catalog::AV1`] field extraction
//! used by the fMP4 and MKV importers. [`import`] holds the per-codec
//! [`Import`](import::Import) that publishes raw AV1 bitstreams.

pub mod import;

use hang::catalog::AV1;

/// Map a parsed `mp4_atom::Av1c` (AV1CodecConfigurationRecord) to the
/// hang catalog's AV1 codec struct.
///
/// Fills in profile, level, bit depth, and chroma sampling info. Color/HDR
/// fields default to unspecified.
pub fn av1_from_av1c(av1c: &mp4_atom::Av1c) -> AV1 {
	AV1 {
		profile: av1c.seq_profile,
		level: av1c.seq_level_idx_0,
		bitdepth: bitdepth(av1c.seq_tier_0, av1c.high_bitdepth),
		mono_chrome: av1c.monochrome,
		chroma_subsampling_x: av1c.chroma_subsampling_x,
		chroma_subsampling_y: av1c.chroma_subsampling_y,
		chroma_sample_position: av1c.chroma_sample_position,
		..Default::default()
	}
}

/// Bit depth from the (seq_tier_0, high_bitdepth) av1C flag pair.
pub fn bitdepth(tier: bool, high_bitdepth: bool) -> u8 {
	match (tier, high_bitdepth) {
		(true, true) => 12,
		(true, false) | (false, true) => 10,
		(false, false) => 8,
	}
}

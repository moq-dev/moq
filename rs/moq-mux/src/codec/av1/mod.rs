//! AV1.
//!
//! Maps the AV1CodecConfigurationRecord (av1C) flag bits into the
//! catalog's AV1 codec struct, and provides an [`Import`] that publishes
//! raw AV1 bitstreams (OBU-framed) to a moq broadcast.

mod import;

pub use import::*;

use hang::catalog::AV1;

/// Build a catalog [`VideoConfig`](hang::catalog::VideoConfig) for the `av01`
/// shape from an AV1CodecConfigurationRecord (av1C).
///
/// Used by the enhanced-RTMP / FLV importer, where the av1C arrives out of band
/// in the sequence-header tag (leading `0x81` marker) and the coded samples are
/// raw OBU temporal units, so the record passes straight through as the catalog
/// `description`. Resolution and color live in the inline sequence header, not
/// the av1C, so `coded_width`/`coded_height` are left unset here.
pub(crate) fn config_from_av1c(av1c: &[u8]) -> anyhow::Result<hang::catalog::VideoConfig> {
	// av1C: byte 0 = marker(1)|version(7) = 0x81, byte 1 = seq_profile(3)|seq_level_idx_0(5),
	// byte 2 = seq_tier_0|high_bitdepth|twelve_bit|monochrome|subsampling_x|subsampling_y|sample_position(2).
	anyhow::ensure!(av1c.len() >= 4 && av1c[0] == 0x81, "invalid av1C record");
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

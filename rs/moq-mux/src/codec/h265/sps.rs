//! The SPS fields moq-mux reads, with a fallback for an SPS that scuffle_h265 refuses.
//!
//! scuffle_h265 turns semantic constraints deep in the SPS (VUI colour fields, HRD, extensions)
//! into parse errors, and real encoders break some of them: cameras ship `matrix_coeffs` 0 on
//! 4:2:0, which ITU-T H.265 E.3.1 forbids and libavcodec and GStreamer decode anyway. The codec
//! string, coded size and hvcC header only need the SPS head, which ends at
//! `bit_depth_chroma_minus8`. So when scuffle_h265 refuses a value (`InvalidData`), [`Sps::parse`]
//! reads the head itself. A truncated SPS, or one whose head is invalid, is still an error.

use h264_parser::{bitreader::BitReader, eg::read_ue};
use scuffle_h265::{NALUnitType, SpsNALUnit};

use super::{Error, Result};

/// The SPS fields up to `bit_depth_chroma_minus8` (ITU-T H.265 7.3.2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpsHead {
	pub profile_space: u8,
	pub tier_flag: bool,
	pub profile_idc: u8,
	pub profile_compatibility_flags: [u8; 4],
	/// As [`pack_constraint_flags`](super::pack_constraint_flags) packs them.
	pub constraint_flags: [u8; 6],
	pub level_idc: u8,
	pub max_sub_layers_minus1: u8,
	pub temporal_id_nesting_flag: bool,
	pub chroma_format_idc: u8,
	pub bit_depth_luma_minus8: u8,
	pub bit_depth_chroma_minus8: u8,
	/// The picture size inside the conformance window.
	pub width: u32,
	pub height: u32,
}

/// A parsed SPS: its head, plus scuffle_h265's full parse (with the VUI) when it accepted the SPS.
pub(crate) struct Sps {
	pub head: SpsHead,
	pub full: Option<scuffle_h265::SpsRbsp>,
}

impl Sps {
	/// Parse an SPS NAL unit, keeping only its head when scuffle_h265 refuses a later value.
	pub(crate) fn parse(nal: &[u8]) -> Result<Self> {
		match SpsNALUnit::parse(&mut &nal[..]) {
			Ok(sps) => Ok(Self {
				head: SpsHead::from_rbsp(&sps.rbsp)?,
				full: Some(sps.rbsp),
			}),
			// A value scuffle_h265 refused, not a bitstream that ran out.
			Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
				let head = SpsHead::parse(nal).ok_or(Error::SpsParse)?;
				tracing::warn!(%err, "H.265 SPS refused by scuffle_h265; using its head, without the VUI");
				Ok(Self { head, full: None })
			}
			Err(_) => Err(Error::SpsParse),
		}
	}
}

impl SpsHead {
	fn from_rbsp(rbsp: &scuffle_h265::SpsRbsp) -> Result<Self> {
		let profile = &rbsp.profile_tier_level.general_profile;
		Ok(Self {
			profile_space: profile.profile_space,
			tier_flag: profile.tier_flag,
			profile_idc: profile.profile_idc,
			profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
			constraint_flags: super::pack_constraint_flags(profile),
			level_idc: profile.level_idc.ok_or(Error::MissingLevelIdc)?,
			max_sub_layers_minus1: rbsp.sps_max_sub_layers_minus1,
			temporal_id_nesting_flag: rbsp.sps_temporal_id_nesting_flag,
			chroma_format_idc: rbsp.chroma_format_idc,
			bit_depth_luma_minus8: rbsp.bit_depth_luma_minus8,
			bit_depth_chroma_minus8: rbsp.bit_depth_chroma_minus8,
			width: rbsp.cropped_width() as u32,
			height: rbsp.cropped_height() as u32,
		})
	}

	/// Read the head of an SPS NAL unit (header included). `None` when the NAL is not an SPS, ends
	/// before `bit_depth_chroma_minus8`, or breaks a constraint on the head.
	fn parse(nal: &[u8]) -> Option<Self> {
		let [header, header2, payload @ ..] = nal else {
			return None;
		};
		// forbidden_zero_bit, nal_unit_type, and TemporalId 0 for an SPS (7.4.2.2).
		if header & 0x80 != 0 || super::split::nal_unit_type(*header) != NALUnitType::SpsNut || header2 & 0x07 != 1 {
			return None;
		}
		let rbsp = h264_parser::nal::ebsp_to_rbsp(payload);
		Self::read(&mut BitReader::new(&rbsp)).ok()?
	}

	/// Read the head from the SPS RBSP: `Ok(None)` for an out-of-range value.
	fn read(r: &mut BitReader<'_>) -> h264_parser::Result<Option<Self>> {
		r.skip_bits(4)?; // sps_video_parameter_set_id
		let max_sub_layers_minus1 = r.read_bits(3)? as u8;
		let temporal_id_nesting_flag = r.read_flag()?;
		// 7.4.3.2.1: at most 7 sub-layers, and a single sub-layer is nested.
		if max_sub_layers_minus1 > 6 || (max_sub_layers_minus1 == 0 && !temporal_id_nesting_flag) {
			return Ok(None);
		}

		// profile_tier_level(1, sps_max_sub_layers_minus1), 7.3.3
		let profile_space = r.read_bits(2)? as u8;
		let tier_flag = r.read_flag()?;
		let profile_idc = r.read_bits(5)? as u8;
		let profile_compatibility_flags = r.read_bits(32)?.to_be_bytes();
		// progressive_source, interlaced_source, non_packed_constraint and frame_only_constraint.
		let constraint_flags = [(r.read_bits(4)? as u8) << 4, 0, 0, 0, 0, 0];
		r.skip_bits(44)?; // the other 43 constraint and reserved bits, and general_inbld_flag
		let level_idc = r.read_u8()?;
		let sub_layers = usize::from(max_sub_layers_minus1);
		let mut present = [(false, false); 6];
		for flags in &mut present[..sub_layers] {
			*flags = (r.read_flag()?, r.read_flag()?);
		}
		if sub_layers > 0 {
			r.skip_bits(2 * (8 - u32::from(max_sub_layers_minus1)))?; // reserved_zero_2bits
		}
		for &(profile, level) in &present[..sub_layers] {
			// A sub-layer profile is the same 88 bits as the general one; sub_layer_level_idc is 8.
			r.skip_bits(88 * u32::from(profile) + 8 * u32::from(level))?;
		}

		let sps_seq_parameter_set_id = read_ue(r)?;
		let chroma_format_idc = read_ue(r)?;
		if sps_seq_parameter_set_id > 15 || chroma_format_idc > 3 {
			return Ok(None);
		}
		if chroma_format_idc == 3 {
			r.skip_bits(1)?; // separate_colour_plane_flag
		}
		let pic_width = read_ue(r)?;
		let pic_height = read_ue(r)?;
		// conformance_window_flag, then the left, right, top and bottom offsets.
		let [left, right, top, bottom] = if r.read_flag()? {
			[read_ue(r)?, read_ue(r)?, read_ue(r)?, read_ue(r)?]
		} else {
			[0; 4]
		};
		let bit_depth_luma_minus8 = read_ue(r)?;
		let bit_depth_chroma_minus8 = read_ue(r)?;
		if bit_depth_luma_minus8 > 8 || bit_depth_chroma_minus8 > 8 {
			return Ok(None);
		}

		// The window offsets count chroma samples: SubWidthC and SubHeightC from Table 6-1.
		let sub_width_c = if matches!(chroma_format_idc, 1 | 2) { 2 } else { 1 };
		let sub_height_c = if chroma_format_idc == 1 { 2 } else { 1 };
		let width = pic_width.checked_sub(left.saturating_add(right).saturating_mul(sub_width_c));
		let height = pic_height.checked_sub(top.saturating_add(bottom).saturating_mul(sub_height_c));
		let (Some(width @ 1..), Some(height @ 1..)) = (width, height) else {
			return Ok(None);
		};

		Ok(Some(Self {
			profile_space,
			tier_flag,
			profile_idc,
			profile_compatibility_flags,
			constraint_flags,
			level_idc,
			max_sub_layers_minus1,
			temporal_id_nesting_flag,
			chroma_format_idc: chroma_format_idc as u8,
			bit_depth_luma_minus8: bit_depth_luma_minus8 as u8,
			bit_depth_chroma_minus8: bit_depth_chroma_minus8 as u8,
			width,
			height,
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::super::fixtures;
	use super::*;

	/// The x265 fixture with `sps_max_sub_layers_minus1 = 2`: sub-layer 0 signals its profile,
	/// sub-layer 1 its level, so the head walks both sub-layer branches of 7.3.3.
	const SPS_SUB_LAYERS: &[u8] = &[
		0x42, 0x01, 0x05, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5d,
		0x90, 0x00, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5d, 0xa0,
		0x02, 0x80, 0x80, 0x2d, 0x16, 0x51, 0x59, 0xa4, 0x93, 0x2b, 0xc0, 0x5a, 0x02, 0x00, 0x00, 0x03, 0x00, 0x02,
		0x00, 0x00, 0x03, 0x00, 0x3c, 0x10,
	];

	/// As [`SPS_SUB_LAYERS`], but sub-layer 0 signals both its profile and its level.
	const SPS_SUB_LAYER_PROFILE_AND_LEVEL: &[u8] = &[
		0x42, 0x01, 0x05, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5d,
		0xd0, 0x00, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5a, 0x5d,
		0xa0, 0x02, 0x80, 0x80, 0x2d, 0x16, 0x51, 0x59, 0xa4, 0x93, 0x2b, 0xc0, 0x5a, 0x02, 0x00, 0x00, 0x03, 0x00,
		0x02, 0x00, 0x00, 0x03, 0x00, 0x3c, 0x10,
	];

	/// The head reader agrees with scuffle_h265 on every SPS scuffle_h265 accepts.
	#[test]
	fn head_matches_the_full_parse() {
		for nal in [fixtures::SPS, fixtures::SPS_PYRAMID, SPS_SUB_LAYERS] {
			let sps = Sps::parse(nal).unwrap();
			assert!(sps.full.is_some(), "scuffle_h265 accepts this SPS");
			assert_eq!(SpsHead::parse(nal), Some(sps.head));
		}
		assert_eq!(SpsHead::parse(SPS_SUB_LAYERS).unwrap().max_sub_layers_minus1, 2);
	}

	/// scuffle_h265 0.2.2 reads that `sub_layer_level_idc` twice and refuses the SPS; the head
	/// reads it as GStreamer's parser does.
	#[test]
	fn sub_layer_profile_and_level() {
		let expected = SpsHead {
			max_sub_layers_minus1: 2,
			..SpsHead::parse(fixtures::SPS).unwrap()
		};
		assert_eq!(Sps::parse(SPS_SUB_LAYER_PROFILE_AND_LEVEL).unwrap().head, expected);
	}

	/// Zeroed VUI colour fields on 4:2:0: scuffle_h265 refuses the SPS, the head carries on.
	#[test]
	fn zeroed_colour_description_falls_back_to_the_head() {
		let sps = Sps::parse(fixtures::SPS_ZERO_COLOUR).unwrap();
		assert!(sps.full.is_none(), "scuffle_h265 refuses this SPS");
		assert_eq!(
			sps.head,
			SpsHead {
				profile_space: 0,
				tier_flag: false,
				profile_idc: 1,
				profile_compatibility_flags: [0x60, 0, 0, 0],
				constraint_flags: [0; 6],
				level_idc: 150,
				max_sub_layers_minus1: 0,
				temporal_id_nesting_flag: true,
				chroma_format_idc: 1,
				bit_depth_luma_minus8: 0,
				bit_depth_chroma_minus8: 0,
				width: 1920,
				height: 1080,
			}
		);
	}

	/// The fallback covers a refusal after the head, not a truncated or invalid head.
	#[test]
	fn truncated_or_invalid_head_is_still_an_error() {
		assert!(Sps::parse(&fixtures::SPS[..8]).is_err());
		assert!(Sps::parse(&fixtures::SPS_ZERO_COLOUR[..20]).is_err());
		assert!(Sps::parse(&fixtures::SPS[..fixtures::SPS.len() - 6]).is_err());
		// TemporalId 1, then a single sub-layer without temporal nesting.
		for (index, byte) in [(1, 0x02), (2, 0x00)] {
			let mut nal = fixtures::SPS_ZERO_COLOUR.to_vec();
			nal[index] = byte;
			assert!(Sps::parse(&nal).is_err());
		}
		assert_eq!(SpsHead::parse(fixtures::PPS), None);
		assert_eq!(SpsHead::parse(fixtures::VPS), None);
	}
}

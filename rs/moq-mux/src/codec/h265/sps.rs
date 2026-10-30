//! The SPS fields moq-mux reads, with a fallback for an SPS that scuffle_h265 refuses.
//!
//! scuffle_h265 turns semantic constraints deep in the SPS (VUI colour fields, HRD, extensions)
//! into parse errors, and real encoders break some of them: cameras ship `matrix_coeffs` 0 on
//! 4:2:0, which ITU-T H.265 E.3.1 forbids and libavcodec and GStreamer decode anyway. The codec
//! string, coded size and hvcC header only need the SPS head, which ends at
//! `bit_depth_chroma_minus8`. So when scuffle_h265 refuses a value (`InvalidData`), [`Sps::parse`]
//! reads the head itself. A truncated SPS, or one whose head is invalid, is still an error.

use h264_reader::rbsp::{BitRead, BitReaderError};
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

	/// The colour the VUI's video signal type carries, when scuffle_h265 accepted the SPS.
	pub(crate) fn color(&self) -> Option<hang::catalog::Color> {
		let signal = &self.full.as_ref()?.vui_parameters.as_ref()?.video_signal_type;
		// scuffle_h265 fills an absent video signal type with its default, inferred limited range
		// included, so only a departure from it was signalled. A stream that signals limited range
		// and nothing else reads as unsignalled, which a renderer assumes anyway.
		if *signal == scuffle_h265::VideoSignalType::default() {
			return None;
		}
		crate::codec::color::cicp(
			signal.colour_primaries,
			signal.transfer_characteristics,
			signal.matrix_coeffs,
			Some(signal.video_full_range_flag),
		)
	}
}

impl SpsHead {
	fn from_rbsp(rbsp: &scuffle_h265::SpsRbsp) -> Result<Self> {
		let profile = &rbsp.profile_tier_level.general_profile;
		// scuffle_h265's `cropped_width` subtracts unchecked, so crop here and refuse a conformance
		// window that leaves no picture, as `read` does.
		let window = &rbsp.conformance_window;
		let crop = |size: u64, sub: u8, start: u64, end: u64| {
			let offset = start.checked_add(end)?.checked_mul(sub.into())?;
			u32::try_from(size.checked_sub(offset)?).ok().filter(|size| *size > 0)
		};
		let width = crop(
			rbsp.pic_width_in_luma_samples.get(),
			rbsp.sub_width_c(),
			window.conf_win_left_offset,
			window.conf_win_right_offset,
		)
		.ok_or(Error::SpsParse)?;
		let height = crop(
			rbsp.pic_height_in_luma_samples.get(),
			rbsp.sub_height_c(),
			window.conf_win_top_offset,
			window.conf_win_bottom_offset,
		)
		.ok_or(Error::SpsParse)?;
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
			width,
			height,
		})
	}

	/// Read the head of an SPS NAL unit (header included). `None` when the NAL is not an SPS, ends
	/// before `bit_depth_chroma_minus8`, or breaks a constraint on the head.
	fn parse(nal: &[u8]) -> Option<Self> {
		let [header, header2, ..] = nal else {
			return None;
		};
		// forbidden_zero_bit, nal_unit_type, and TemporalId 0 for an SPS (7.4.2.2).
		if header & 0x80 != 0 || super::split::nal_unit_type(*header) != NALUnitType::SpsNut || header2 & 0x07 != 1 {
			return None;
		}
		Self::read(&mut crate::codec::annexb::rbsp(nal, 2)).ok()?
	}

	/// Read the head from the SPS RBSP: `Ok(None)` for an out-of-range value.
	fn read(r: &mut impl BitRead) -> std::result::Result<Option<Self>, BitReaderError> {
		r.skip(4, "sps_video_parameter_set_id")?;
		let max_sub_layers_minus1 = r.read::<3, u8>("sps_max_sub_layers_minus1")?;
		let temporal_id_nesting_flag = r.read_bit("sps_temporal_id_nesting_flag")?;
		// 7.4.3.2.1: at most 7 sub-layers, and a single sub-layer is nested.
		if max_sub_layers_minus1 > 6 || (max_sub_layers_minus1 == 0 && !temporal_id_nesting_flag) {
			return Ok(None);
		}

		// profile_tier_level(1, sps_max_sub_layers_minus1), 7.3.3
		let profile_space = r.read::<2, u8>("general_profile_space")?;
		let tier_flag = r.read_bit("general_tier_flag")?;
		let profile_idc = r.read::<5, u8>("general_profile_idc")?;
		let profile_compatibility_flags = r.read::<32, u32>("general_profile_compatibility_flags")?.to_be_bytes();
		// progressive_source, interlaced_source, non_packed_constraint and frame_only_constraint.
		let constraint_flags = [r.read::<4, u8>("general_constraint_flags")? << 4, 0, 0, 0, 0, 0];
		r.skip(44, "general_reserved_zero_43bits")?; // and general_inbld_flag
		let level_idc = r.read::<8, u8>("general_level_idc")?;
		skip_sub_layers(r, max_sub_layers_minus1)?;

		let sps_seq_parameter_set_id = r.read_ue("sps_seq_parameter_set_id")?;
		let chroma_format_idc = r.read_ue("chroma_format_idc")?;
		if sps_seq_parameter_set_id > 15 || chroma_format_idc > 3 {
			return Ok(None);
		}
		if chroma_format_idc == 3 {
			r.skip(1, "separate_colour_plane_flag")?;
		}
		let pic_width = r.read_ue("pic_width_in_luma_samples")?;
		let pic_height = r.read_ue("pic_height_in_luma_samples")?;
		// conformance_window_flag, then the left, right, top and bottom offsets.
		let [left, right, top, bottom] = if r.read_bit("conformance_window_flag")? {
			[
				r.read_ue("conf_win_left_offset")?,
				r.read_ue("conf_win_right_offset")?,
				r.read_ue("conf_win_top_offset")?,
				r.read_ue("conf_win_bottom_offset")?,
			]
		} else {
			[0; 4]
		};
		let bit_depth_luma_minus8 = r.read_ue("bit_depth_luma_minus8")?;
		let bit_depth_chroma_minus8 = r.read_ue("bit_depth_chroma_minus8")?;
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

/// Read `sps_seq_parameter_set_id` from an SPS RBSP (the payload after the NAL header).
pub(crate) fn id(r: &mut impl BitRead) -> std::result::Result<u32, BitReaderError> {
	r.skip(4, "sps_video_parameter_set_id")?;
	let max_sub_layers_minus1 = r.read::<3, u8>("sps_max_sub_layers_minus1")?;
	r.skip(1, "sps_temporal_id_nesting_flag")?;
	r.skip(96, "general_profile_and_level")?; // the general profile's 88 bits and general_level_idc
	skip_sub_layers(r, max_sub_layers_minus1)?;
	r.read_ue("sps_seq_parameter_set_id")
}

/// Skip the sub-layer half of profile_tier_level (7.3.3), which follows `general_level_idc`.
fn skip_sub_layers(r: &mut impl BitRead, max_sub_layers_minus1: u8) -> std::result::Result<(), BitReaderError> {
	// 7.4.3.2.1: at most 7 sub-layers.
	if max_sub_layers_minus1 > 6 {
		return Err(BitReaderError::ReaderError(
			"sps_max_sub_layers_minus1",
			std::io::ErrorKind::InvalidData.into(),
		));
	}
	let sub_layers = usize::from(max_sub_layers_minus1);
	let mut present = [(false, false); 6];
	for flags in &mut present[..sub_layers] {
		*flags = (
			r.read_bit("sub_layer_profile_present_flag")?,
			r.read_bit("sub_layer_level_present_flag")?,
		);
	}
	if sub_layers > 0 {
		r.skip(2 * (8 - u32::from(max_sub_layers_minus1)), "reserved_zero_2bits")?;
	}
	for &(profile, level) in &present[..sub_layers] {
		// A sub-layer profile is the same 88 bits as the general one; sub_layer_level_idc is 8.
		r.skip(
			88 * u32::from(profile) + 8 * u32::from(level),
			"sub_layer_profile_and_level",
		)?;
	}
	Ok(())
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

	/// The id reader walks the same profile_tier_level, sub-layers included, to the same id.
	#[test]
	fn id_matches_the_full_parse() {
		for nal in [fixtures::SPS, fixtures::SPS_PYRAMID, SPS_SUB_LAYERS] {
			let full = SpsNALUnit::parse(&mut &nal[..]).unwrap().rbsp;
			assert_eq!(
				id(&mut crate::codec::annexb::rbsp(nal, 2)).unwrap(),
				full.sps_seq_parameter_set_id as u32
			);
		}
		assert_eq!(
			id(&mut crate::codec::annexb::rbsp(SPS_SUB_LAYER_PROFILE_AND_LEVEL, 2)).unwrap(),
			0
		);
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

	/// A conformance window that crops away the whole picture, or past it, is refused rather than
	/// handing a zero (or wrapped) size to the display aspect.
	#[test]
	fn empty_conformance_window_is_refused() {
		let rbsp = SpsNALUnit::parse(&mut &fixtures::SPS[..]).unwrap().rbsp;
		let width = rbsp.pic_width_in_luma_samples.get() / u64::from(rbsp.sub_width_c());
		for right in [width, width + 1] {
			let mut rbsp = rbsp.clone();
			rbsp.conformance_window.conf_win_left_offset = 0;
			rbsp.conformance_window.conf_win_right_offset = right;
			assert!(SpsHead::from_rbsp(&rbsp).is_err());
		}
		let height = rbsp.pic_height_in_luma_samples.get() / u64::from(rbsp.sub_height_c());
		let mut rbsp = rbsp.clone();
		rbsp.conformance_window.conf_win_top_offset = 0;
		rbsp.conformance_window.conf_win_bottom_offset = height;
		assert!(SpsHead::from_rbsp(&rbsp).is_err());
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

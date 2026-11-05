//! The SPS fields moq-mux reads, with a fallback for an SPS that scuffle_h265 refuses.
//!
//! scuffle_h265 turns semantic constraints deep in the SPS (VUI colour fields, HRD, extensions)
//! into parse errors, and real encoders break some of them: cameras ship `matrix_coeffs` 0 on
//! 4:2:0, which ITU-T H.265 E.3.1 forbids and libavcodec and GStreamer decode anyway. The codec
//! string, coded size and hvcC header only need the SPS head, which ends at
//! `bit_depth_chroma_minus8`. So when scuffle_h265 refuses a value (`InvalidData`), [`Sps::parse`]
//! reads the head itself. A truncated SPS, or one whose head is invalid, is still an error.
//!
//! scuffle_h265 also overflows on hostile values, so [`parse_full`] walks the SPS with moq-mux's
//! own reader before handing it over, and an SPS that walk refuses falls back to its head too.

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
		match parse_full(nal) {
			Ok(rbsp) => Ok(Self {
				head: SpsHead::from_rbsp(&rbsp)?,
				full: Some(rbsp),
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
		skip_sub_layers(r, max_sub_layers_minus1, false)?;

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
	skip_sub_layers(r, max_sub_layers_minus1, false)?;
	r.read_ue("sps_seq_parameter_set_id")
}

/// Skip the sub-layer half of profile_tier_level (7.3.3), which follows `general_level_idc`.
///
/// With `level_twice`, a sub-layer that signals both its profile and its level carries
/// `sub_layer_level_idc` twice, as scuffle_h265 0.2.2 reads it.
fn skip_sub_layers(
	r: &mut impl BitRead,
	max_sub_layers_minus1: u8,
	level_twice: bool,
) -> std::result::Result<(), BitReaderError> {
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
		let twice = level_twice && profile && level;
		r.skip(
			88 * u32::from(profile) + 8 * u32::from(level) + 8 * u32::from(twice),
			"sub_layer_profile_and_level",
		)?;
	}
	Ok(())
}

/// Parse an SPS NAL unit with scuffle_h265, once [`check`] has walked it.
///
/// Every scuffle_h265 SPS parse in moq-mux goes through here, so no hostile SPS reaches its overflows.
pub(crate) fn parse_full(nal: &[u8]) -> std::io::Result<scuffle_h265::SpsRbsp> {
	check(nal)?;
	Ok(SpsNALUnit::parse(&mut &nal[..])?.rbsp)
}

/// Walk an SPS NAL unit as scuffle_h265 0.2.2 reads it, refusing the values its parser would
/// overflow on.
///
/// scuffle_h265 subtracts and indexes with SPS fields unchecked, and scuffle-expgolomb 0.1.5 wraps
/// an Exp-Golomb code of 64 or more leading zeros
/// ([ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653),
/// [ScuffleCloud/scuffle#654](https://github.com/ScuffleCloud/scuffle/issues/654)). This reads
/// every field scuffle_h265 does, in its layout rather than the spec's where the two differ, with
/// h264_reader's reader, whose `ue(v)` stops at 2^32 - 2 as ITU-T H.265 9.2 bounds it. That keeps
/// scuffle_h265's sums inside a `u64`, and the checks below cover its subtractions, its scaling
/// list index, and its palette allocation.
///
/// An SPS that runs out of bits is `UnexpectedEof`, the kind scuffle_h265 reports for it. One with
/// a refused value, or with a byte sequence emulation prevention forbids, is `InvalidData`.
fn check(nal: &[u8]) -> std::io::Result<()> {
	use std::io::{Error, ErrorKind};

	let [header, _, ..] = *nal else {
		return Err(ErrorKind::UnexpectedEof.into());
	};
	if super::split::nal_unit_type(header) != NALUnitType::SpsNut {
		return Err(Error::new(ErrorKind::InvalidData, "not an SPS"));
	}
	match walk(&mut crate::codec::annexb::rbsp(nal, 2)) {
		Ok(()) => Ok(()),
		Err(BitReaderError::ReaderError(_, err)) if err.kind() == ErrorKind::UnexpectedEof => Err(err),
		Err(err) => Err(Error::new(ErrorKind::InvalidData, format!("{err:?}"))),
	}
}

/// Refuse `field` unless `valid`.
fn ensure(valid: bool, field: &'static str) -> std::result::Result<(), BitReaderError> {
	match valid {
		true => Ok(()),
		false => Err(BitReaderError::ReaderError(
			field,
			std::io::ErrorKind::InvalidData.into(),
		)),
	}
}

/// `seq_parameter_set_rbsp` (7.3.2.2.1), as [`check`] describes.
fn walk(r: &mut impl BitRead) -> std::result::Result<(), BitReaderError> {
	r.skip(4, "sps_video_parameter_set_id")?;
	let max_sub_layers_minus1 = r.read::<3, u8>("sps_max_sub_layers_minus1")?;
	r.skip(1, "sps_temporal_id_nesting_flag")?;
	r.skip(96, "general_profile_and_level")?;
	skip_sub_layers(r, max_sub_layers_minus1, true)?;

	r.read_ue("sps_seq_parameter_set_id")?;
	let chroma_format_idc = r.read_ue("chroma_format_idc")?;
	ensure(chroma_format_idc <= 3, "chroma_format_idc")?;
	if chroma_format_idc == 3 {
		r.skip(1, "separate_colour_plane_flag")?;
	}
	r.read_ue("pic_width_in_luma_samples")?;
	r.read_ue("pic_height_in_luma_samples")?;
	if r.read_bit("conformance_window_flag")? {
		for field in [
			"conf_win_left_offset",
			"conf_win_right_offset",
			"conf_win_top_offset",
			"conf_win_bottom_offset",
		] {
			r.read_ue(field)?;
		}
	}
	let bit_depth_luma = r.read_ue("bit_depth_luma_minus8")?;
	let bit_depth_chroma = r.read_ue("bit_depth_chroma_minus8")?;
	ensure(bit_depth_luma <= 8 && bit_depth_chroma <= 8, "bit_depth_minus8")?;
	let log2_max_pic_order_cnt_lsb_minus4 = r.read_ue("log2_max_pic_order_cnt_lsb_minus4")?;
	ensure(
		log2_max_pic_order_cnt_lsb_minus4 <= 12,
		"log2_max_pic_order_cnt_lsb_minus4",
	)?;

	let first = match r.read_bit("sps_sub_layer_ordering_info_present_flag")? {
		true => 0,
		false => max_sub_layers_minus1,
	};
	for _ in first..=max_sub_layers_minus1 {
		r.read_ue("sps_max_dec_pic_buffering_minus1")?;
		r.read_ue("sps_max_num_reorder_pics")?;
		r.read_ue("sps_max_latency_increase_plus1")?;
	}

	let min_cb_log2_size = u64::from(r.read_ue("log2_min_luma_coding_block_size_minus3")?) + 3;
	let ctb_log2_size = min_cb_log2_size + u64::from(r.read_ue("log2_diff_max_min_luma_coding_block_size")?);
	let min_tb_log2_size = u64::from(r.read_ue("log2_min_luma_transform_block_size_minus2")?) + 2;
	// scuffle_h265 bounds the transform hierarchy depths by CtbLog2SizeY - MinTbLog2SizeY. 7.4.3.2.1
	// keeps MinTbLog2SizeY below MinCbLog2SizeY, and so below CtbLog2SizeY.
	ensure(
		min_tb_log2_size <= ctb_log2_size,
		"log2_min_luma_transform_block_size_minus2",
	)?;
	r.read_ue("log2_diff_max_min_luma_transform_block_size")?;
	r.read_ue("max_transform_hierarchy_depth_inter")?;
	r.read_ue("max_transform_hierarchy_depth_intra")?;

	if r.read_bit("scaling_list_enabled_flag")? && r.read_bit("sps_scaling_list_data_present_flag")? {
		scaling_list_data(r)?;
	}
	r.skip(2, "amp_enabled_flag")?; // and sample_adaptive_offset_enabled_flag
	if r.read_bit("pcm_enabled_flag")? {
		r.skip(8, "pcm_sample_bit_depth_minus1")?; // luma, then chroma
		r.read_ue("log2_min_pcm_luma_coding_block_size_minus3")?;
		r.read_ue("log2_diff_max_min_pcm_luma_coding_block_size")?;
		r.skip(1, "pcm_loop_filter_disabled_flag")?;
	}
	short_term_ref_pic_sets(r)?;
	if r.read_bit("long_term_ref_pics_present_flag")? {
		let count = r.read_ue("num_long_term_ref_pics_sps")?;
		ensure(count <= 32, "num_long_term_ref_pics_sps")?;
		for _ in 0..count {
			// lt_ref_pic_poc_lsb_sps, then used_by_curr_pic_lt_sps_flag.
			r.skip(log2_max_pic_order_cnt_lsb_minus4 + 5, "lt_ref_pic_poc_lsb_sps")?;
		}
	}
	r.skip(2, "sps_temporal_mvp_enabled_flag")?; // and strong_intra_smoothing_enabled_flag
	if r.read_bit("vui_parameters_present_flag")? {
		vui_parameters(r, max_sub_layers_minus1)?;
	}

	if r.read_bit("sps_extension_present_flag")? {
		let range = r.read_bit("sps_range_extension_flag")?;
		let multilayer = r.read_bit("sps_multilayer_extension_flag")?;
		let three_d = r.read_bit("sps_3d_extension_flag")?;
		let scc = r.read_bit("sps_scc_extension_flag")?;
		ensure(r.read::<4, u8>("sps_extension_4bits")? == 0, "sps_extension_4bits")?;
		if range {
			r.skip(9, "sps_range_extension")?;
		}
		if multilayer {
			r.skip(1, "inter_view_mv_vert_constraint_flag")?;
		}
		if three_d {
			// scuffle_h265 reads iv_di_mc_enabled_flag and iv_mv_scal_enabled_flag once, not per d.
			r.skip(2, "iv_di_mc_enabled_flag")?;
			r.read_ue("log2_ivmc_sub_pb_size_minus3")?;
			r.skip(5, "iv_res_pred_enabled_flag")?; // through tex_mc_enabled_flag
			r.read_ue("log2_texmc_sub_pb_size_minus3")?;
			r.skip(5, "intra_contour_enabled_flag")?; // through skip_intra_enabled_flag
		}
		if scc {
			let bit_depths = [bit_depth_luma + 8, bit_depth_chroma + 8, bit_depth_chroma + 8];
			let components = if chroma_format_idc == 0 { 1 } else { 3 };
			sps_scc_extension(r, &bit_depths[..components])?;
		}
	}
	ensure(r.read_bit("rbsp_stop_one_bit")?, "rbsp_stop_one_bit")
}

/// `scaling_list_data` (7.3.4).
fn scaling_list_data(r: &mut impl BitRead) -> std::result::Result<(), BitReaderError> {
	for size_id in 0..4 {
		let step = if size_id == 3 { 3 } else { 1 };
		for matrix_id in (0..6).step_by(step) {
			if !r.read_bit("scaling_list_pred_mode_flag")? {
				let delta = r.read_ue("scaling_list_pred_matrix_id_delta")?;
				// 7.4.5: the reference is this matrix or an earlier one; scuffle_h265 subtracts unchecked.
				ensure(
					u64::from(delta) * step as u64 <= matrix_id as u64,
					"scaling_list_pred_matrix_id_delta",
				)?;
				continue;
			}
			if size_id > 1 {
				r.read_se("scaling_list_dc_coef_minus8")?;
			}
			for _ in 0..(16 << (2 * size_id)).min(64) {
				r.read_se("scaling_list_delta_coef")?;
			}
		}
	}
	Ok(())
}

/// The `st_ref_pic_set` (7.3.7) list. Inter prediction sizes a set by the one before it, so each
/// set's delta POCs are derived (7-61, 7-62) to know how many flags the next one reads.
fn short_term_ref_pic_sets(r: &mut impl BitRead) -> std::result::Result<(), BitReaderError> {
	let count = r.read_ue("num_short_term_ref_pic_sets")?;
	ensure(count <= 64, "num_short_term_ref_pic_sets")?;
	// The delta POCs of the set before: S0 (negative) then S1 (positive), in the order
	// inter_ref_pic_set_prediction_flag's use_delta_flag indexes them.
	let (mut s0, mut s1) = (Vec::<i32>::new(), Vec::<i32>::new());
	for index in 0..count {
		if index != 0 && r.read_bit("inter_ref_pic_set_prediction_flag")? {
			// delta_idx_minus1 is only present in a slice header, so the reference is the set before.
			let negative = r.read_bit("delta_rps_sign")?;
			let abs_delta_rps_minus1 = r.read_ue("abs_delta_rps_minus1")?;
			ensure(abs_delta_rps_minus1 < 1 << 15, "abs_delta_rps_minus1")?;
			let delta = (abs_delta_rps_minus1 as i32 + 1) * if negative { -1 } else { 1 };
			let mut use_delta = Vec::with_capacity(s0.len() + s1.len() + 1);
			for _ in 0..=s0.len() + s1.len() {
				// use_delta_flag follows only a used_by_curr_pic_flag of 0, and is inferred 1.
				use_delta.push(r.read_bit("used_by_curr_pic_flag")? || r.read_bit("use_delta_flag")?);
			}
			let (use0, rest) = use_delta.split_at(s0.len());
			let (use1, own) = rest.split_at(s1.len());
			let shifted = |pocs: &[i32], used: &[bool]| -> Vec<i32> {
				pocs.iter()
					.zip(used)
					.filter(|(_, used)| **used)
					.map(|(poc, _)| poc + delta)
					.collect()
			};
			let (from0, from1) = (shifted(&s0, use0), shifted(&s1, use1));
			let own = own[0].then_some(delta);
			// (7-61): S1 reversed, deltaRps, then S0; (7-62): S0 reversed, deltaRps, then S1.
			let next0 = (from1.iter().rev().chain(&own).chain(&from0).copied()).filter(|poc| *poc < 0);
			let next0: Vec<_> = next0.collect();
			let next1 = (from0.iter().rev().chain(&own).chain(&from1).copied()).filter(|poc| *poc > 0);
			let next1: Vec<_> = next1.collect();
			(s0, s1) = (next0, next1);
		} else {
			let negative = r.read_ue("num_negative_pics")?;
			let positive = r.read_ue("num_positive_pics")?;
			ensure(negative <= 16 && positive <= 16, "num_delta_pics")?;
			let mut read = |count: u32, sign: i32| -> std::result::Result<Vec<i32>, BitReaderError> {
				let mut poc = 0;
				(0..count)
					.map(|_| {
						let delta = r.read_ue("delta_poc_minus1")?;
						ensure(delta < 1 << 15, "delta_poc_minus1")?;
						r.skip(1, "used_by_curr_pic_flag")?;
						poc += sign * (delta as i32 + 1);
						Ok(poc)
					})
					.collect()
			};
			s0 = read(negative, -1)?;
			s1 = read(positive, 1)?;
		}
		// MaxDpbSize is at most 16 (A.4.2), which bounds both lists in any layer.
		ensure(s0.len() <= 16 && s1.len() <= 16, "num_delta_pocs")?;
	}
	Ok(())
}

/// `vui_parameters` (E.2.1).
fn vui_parameters(r: &mut impl BitRead, max_sub_layers_minus1: u8) -> std::result::Result<(), BitReaderError> {
	if r.read_bit("aspect_ratio_info_present_flag")? && r.read::<8, u8>("aspect_ratio_idc")? == 255 {
		r.skip(32, "sar_width")?; // and sar_height
	}
	if r.read_bit("overscan_info_present_flag")? {
		r.skip(1, "overscan_appropriate_flag")?;
	}
	if r.read_bit("video_signal_type_present_flag")? {
		r.skip(4, "video_format")?; // and video_full_range_flag
		if r.read_bit("colour_description_present_flag")? {
			r.skip(24, "colour_description")?;
		}
	}
	if r.read_bit("chroma_loc_info_present_flag")? {
		r.read_ue("chroma_sample_loc_type_top_field")?;
		r.read_ue("chroma_sample_loc_type_bottom_field")?;
	}
	r.skip(3, "neutral_chroma_indication_flag")?; // field_seq_flag, frame_field_info_present_flag
	if r.read_bit("default_display_window_flag")? {
		for field in [
			"def_disp_win_left_offset",
			"def_disp_win_right_offset",
			"def_disp_win_top_offset",
			"def_disp_win_bottom_offset",
		] {
			r.read_ue(field)?;
		}
	}
	if r.read_bit("vui_timing_info_present_flag")? {
		r.skip(64, "vui_num_units_in_tick")?; // and vui_time_scale
		if r.read_bit("vui_poc_proportional_to_timing_flag")? {
			r.read_ue("vui_num_ticks_poc_diff_one_minus1")?;
		}
		if r.read_bit("vui_hrd_parameters_present_flag")? {
			hrd_parameters(r, max_sub_layers_minus1)?;
		}
	}
	if r.read_bit("bitstream_restriction_flag")? {
		r.skip(3, "tiles_fixed_structure_flag")?; // through restricted_ref_pic_lists_flag
		for field in [
			"min_spatial_segmentation_idc",
			"max_bytes_per_pic_denom",
			"max_bits_per_min_cu_denom",
			"log2_max_mv_length_horizontal",
			"log2_max_mv_length_vertical",
		] {
			r.read_ue(field)?;
		}
	}
	Ok(())
}

/// `hrd_parameters(1, sps_max_sub_layers_minus1)` (E.2.2), with its `sub_layer_hrd_parameters`
/// (E.2.3).
fn hrd_parameters(r: &mut impl BitRead, max_sub_layers_minus1: u8) -> std::result::Result<(), BitReaderError> {
	let nal = r.read_bit("nal_hrd_parameters_present_flag")?;
	let vcl = r.read_bit("vcl_hrd_parameters_present_flag")?;
	let mut sub_pic = false;
	if nal || vcl {
		sub_pic = r.read_bit("sub_pic_hrd_params_present_flag")?;
		if sub_pic {
			r.skip(19, "tick_divisor_minus2")?; // through dpb_output_delay_du_length_minus1
		}
		r.skip(8, "bit_rate_scale")?; // and cpb_size_scale
		if sub_pic {
			r.skip(4, "cpb_size_du_scale")?;
		}
		r.skip(15, "initial_cpb_removal_delay_length_minus1")?; // through dpb_output_delay_length_minus1
	}
	for _ in 0..=max_sub_layers_minus1 {
		let fixed = r.read_bit("fixed_pic_rate_general_flag")? || r.read_bit("fixed_pic_rate_within_cvs_flag")?;
		let low_delay = match fixed {
			true => r.read_ue("elemental_duration_in_tc_minus1").map(|_| false)?,
			false => r.read_bit("low_delay_hrd_flag")?,
		};
		let cpb_cnt_minus1 = match low_delay {
			true => 0,
			false => r.read_ue("cpb_cnt_minus1")?,
		};
		ensure(cpb_cnt_minus1 <= 31, "cpb_cnt_minus1")?;
		for _ in 0..u32::from(nal) + u32::from(vcl) {
			for _ in 0..=cpb_cnt_minus1 {
				r.read_ue("bit_rate_value_minus1")?;
				r.read_ue("cpb_size_value_minus1")?;
				if sub_pic {
					r.read_ue("cpb_size_du_value_minus1")?;
					r.read_ue("bit_rate_du_value_minus1")?;
				}
				r.skip(1, "cbr_flag")?;
			}
		}
	}
	Ok(())
}

/// `sps_scc_extension` (7.3.2.2.3), with each component's bit depth.
fn sps_scc_extension(r: &mut impl BitRead, bit_depths: &[u32]) -> std::result::Result<(), BitReaderError> {
	r.skip(1, "sps_curr_pic_ref_enabled_flag")?;
	if r.read_bit("palette_mode_enabled_flag")? {
		let palette_max_size = r.read_ue("palette_max_size")?;
		// 7.4.3.3.8 bounds it at 64; scuffle_h265 allocates the predictor initializers it sizes.
		ensure(palette_max_size <= 64, "palette_max_size")?;
		r.read_ue("delta_palette_max_predictor_size")?;
		if r.read_bit("sps_palette_predictor_initializers_present_flag")? {
			let initializers = r.read_ue("sps_num_palette_predictor_initializers_minus1")?;
			ensure(
				initializers < palette_max_size,
				"sps_num_palette_predictor_initializers_minus1",
			)?;
			let bits: u32 = bit_depths.iter().sum();
			r.skip((initializers + 1) * bits, "sps_palette_predictor_initializer")?;
		}
	}
	r.skip(3, "motion_vector_resolution_control_idc")?; // and intra_boundary_filtering_disabled_flag
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

	/// scuffle_h265's `sps_parse5` test SPS, which carries `scaling_list_data`.
	const SPS_SCALING_LIST: &[u8] = &[
		0x42, 0x01, 0x01, 0x24, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9d, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x99, 0xb0,
		0x01, 0xe0, 0x20, 0x02, 0x1c, 0x4d, 0x94, 0xd6, 0xed, 0xbe, 0x41, 0x12, 0x64, 0xeb, 0x25, 0x11, 0x44, 0x1a,
		0x6c, 0x9d, 0x64, 0xa2, 0x29, 0x09, 0x26, 0xba, 0xf5, 0xff, 0xeb, 0xfa, 0xfd, 0x7f, 0xeb, 0xf5, 0x44, 0x51,
		0x04, 0x93, 0x5d, 0x7a, 0xff, 0xf5, 0xfd, 0x7e, 0xbf, 0xf5, 0xfa, 0xc8, 0xa4, 0x92, 0x4d, 0x75, 0xeb, 0xff,
		0xd7, 0xf5, 0xfa, 0xff, 0xd7, 0xea, 0x88, 0xa2, 0x24, 0x93, 0x5d, 0x7a, 0xff, 0xf5, 0xfd, 0x7e, 0xbf, 0xf5,
		0xfa, 0xc8, 0x94, 0x08, 0x53, 0x49, 0x29, 0x24, 0x89, 0x55, 0x12, 0xa5, 0x2a, 0x94, 0xc1, 0x35, 0x01, 0x01,
		0x01, 0x03, 0xb8, 0x40, 0x20, 0x80,
	];

	/// scuffle_h265's `sps_parse_inter_ref_prediction` test SPS: 26 short-term RPSs, most of them
	/// predicted from the one before.
	const SPS_INTER_RPS: &[u8] = &[
		0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
		0x00, 0xa0, 0x0b, 0x08, 0x04, 0x85, 0x96, 0x5e, 0x49, 0x1b, 0x60, 0xd9, 0x78, 0x88, 0x88, 0x8f, 0xe7, 0x9f,
		0xcf, 0xe7, 0xf3, 0xf9, 0xfc, 0xf2, 0xff, 0xff, 0xff, 0xcf, 0xe7, 0xf3, 0xf9, 0xfc, 0xfe, 0x7f, 0x3f, 0x3f,
		0x9f, 0xcf, 0xe7, 0xf3, 0xf9, 0xdb, 0x20,
	];

	/// A syntax element of a synthetic SPS RBSP.
	#[derive(Clone, Copy)]
	enum Field {
		/// `u(n)`: the low `n` bits of the value.
		U(u32, u64),
		Ue(u32),
		/// An Exp-Golomb code with 64 leading zeros, which only a wrapping reader decodes.
		Ue64,
	}
	use Field::*;

	/// A Main 4:2:0 64x64 SPS through `bit_depth_chroma_minus8`.
	const HEAD: &[Field] = &[
		U(4, 0),                 // sps_video_parameter_set_id
		U(3, 0),                 // sps_max_sub_layers_minus1
		U(1, 1),                 // sps_temporal_id_nesting_flag
		U(8, 1),                 // general_profile_space, tier and profile_idc: Main
		U(32, 0x6000_0000),      // general_profile_compatibility_flags
		U(48, 0x9000_0000_0000), // progressive and frame-only, then the reserved bits
		U(8, 93),                // general_level_idc
		Ue(0),                   // sps_seq_parameter_set_id
		Ue(1),                   // chroma_format_idc
		Ue(64),                  // pic_width_in_luma_samples
		Ue(64),                  // pic_height_in_luma_samples
		U(1, 0),                 // conformance_window_flag
		Ue(0),                   // bit_depth_luma_minus8
		Ue(0),                   // bit_depth_chroma_minus8
		Ue(0),                   // log2_max_pic_order_cnt_lsb_minus4
		U(1, 1),                 // sps_sub_layer_ordering_info_present_flag
		Ue(4),                   // sps_max_dec_pic_buffering_minus1
		Ue(0),                   // sps_max_num_reorder_pics
		Ue(0),                   // sps_max_latency_increase_plus1
	];

	/// 8x8 coding blocks in 16x16 CTBs, 4x4 to 16x16 transform blocks.
	const BLOCKS: &[Field] = &[Ue(0), Ue(1), Ue(0), Ue(2), Ue(0), Ue(0)];

	/// The RBSP ends: no PCM, RPS, long-term pictures, VUI or extension.
	const END: &[Field] = &[U(2, 0), U(1, 0), Ue(0), U(1, 0), U(2, 0), U(1, 0), U(1, 0)];

	/// An SPS NAL unit holding `fields`, with its trailing bits and emulation prevention.
	fn synthetic(fields: &[&[Field]]) -> Vec<u8> {
		use h264_reader::rbsp::{BitWrite, BitWriter, ByteWriter};
		use std::io::Write;

		let mut w = BitWriter::new(Vec::new());
		for field in fields.iter().copied().flatten() {
			match *field {
				U(bits, value) => w.write_var(bits, value).unwrap(),
				Ue(value) => w.write_ue(value).unwrap(),
				Ue64 => {
					w.write_var(64, 0u64).unwrap();
					w.write_bit(true).unwrap();
					w.write_var(64, 0u64).unwrap();
				}
			}
		}
		w.write_rbsp_trailing_bits().unwrap();
		let mut nal = ByteWriter::new(vec![0x42, 0x01]);
		nal.write_all(&w.into_writer()).unwrap();
		nal.into_writer()
	}

	/// `check` reads an SPS scuffle_h265 accepts to its stop bit, with only zeros after it in
	/// that byte, so it read the fields scuffle_h265 did.
	fn assert_walks_to_the_end(nal: &[u8]) {
		SpsNALUnit::parse(&mut &nal[..]).expect("scuffle_h265 accepts this SPS");
		let mut r = crate::codec::annexb::rbsp(nal, 2);
		walk(&mut r).unwrap();
		while !r.byte_aligned() {
			assert!(!r.read_bit("rbsp_alignment_zero_bit").unwrap());
		}
		parse_full(nal).unwrap();
	}

	/// The walk keeps pace with scuffle_h265 through every branch it takes.
	#[test]
	fn check_reads_what_scuffle_reads() {
		for nal in [
			fixtures::SPS,
			fixtures::SPS_HDR10,
			fixtures::SPS_PYRAMID,
			fixtures::SPS_NVENC,
			fixtures::SPS_ANAMORPHIC,
			SPS_SUB_LAYERS,
			SPS_SCALING_LIST,
			SPS_INTER_RPS,
		] {
			assert_walks_to_the_end(nal);
		}
		assert_walks_to_the_end(&synthetic(&[HEAD, BLOCKS, &[U(1, 0)], END]));

		// PCM, long-term pictures, a VUI with sub-picture HRD parameters, and every extension.
		let nal = synthetic(&[
			HEAD,
			BLOCKS,
			&[U(1, 0), U(2, 0)], // scaling_list_enabled_flag, amp and sao
			// pcm_enabled_flag, its sample bit depths, 8x8 to 16x16 blocks, loop filter flag.
			&[U(1, 1), U(4, 7), U(4, 7), Ue(0), Ue(1), U(1, 0)],
			// Two short-term RPSs: {-1, +1}, then the second predicted from it with deltaRps -1.
			&[Ue(2), Ue(1), Ue(1), Ue(0), U(1, 1), Ue(0), U(1, 1)],
			&[U(1, 1), U(1, 1), Ue(0), U(1, 1), U(1, 1), U(1, 1)],
			// Two long-term pictures, then sps_temporal_mvp and strong_intra_smoothing.
			&[U(1, 1), Ue(2), U(4, 3), U(1, 1), U(4, 5), U(1, 0), U(2, 0)],
			// vui_parameters_present_flag, an extended SAR, overscan, video signal type.
			&[U(1, 1), U(1, 1), U(8, 255), U(16, 4), U(16, 3), U(1, 1), U(1, 0)],
			&[U(1, 1), U(3, 5), U(1, 0), U(1, 1), U(24, 0x01_01_01)],
			// Chroma location, three flags, a default display window.
			&[U(1, 1), Ue(0), Ue(0), U(3, 0), U(1, 1), Ue(1), Ue(1), Ue(1), Ue(1)],
			// Timing with POC proportional to it, then NAL and VCL HRD parameters.
			&[
				U(1, 1),
				U(32, 1001),
				U(32, 60000),
				U(1, 1),
				Ue(1),
				U(1, 1),
				U(1, 1),
				U(1, 1),
			],
			&[U(1, 1), U(19, 0), U(8, 0), U(4, 0), U(15, 0x5ef7)],
			// One sub-layer without a fixed picture rate, two CPBs.
			&[U(1, 0), U(1, 0), U(1, 0), Ue(1)],
			&[
				Ue(10),
				Ue(20),
				Ue(1),
				Ue(1),
				U(1, 0),
				Ue(11),
				Ue(19),
				Ue(1),
				Ue(1),
				U(1, 0),
			],
			&[
				Ue(10),
				Ue(20),
				Ue(1),
				Ue(1),
				U(1, 0),
				Ue(11),
				Ue(19),
				Ue(1),
				Ue(1),
				U(1, 0),
			],
			// Bitstream restriction.
			&[U(1, 1), U(3, 0), Ue(0), Ue(0), Ue(0), Ue(0), Ue(0)],
			// Every extension: range, multilayer, 3D, then SCC with two palette initializers.
			&[U(1, 1), U(4, 0xf), U(4, 0), U(9, 0), U(1, 0)],
			&[U(2, 0), Ue(0), U(5, 0), Ue(1), U(5, 0)],
			&[U(1, 0), U(1, 1), Ue(4), Ue(2), U(1, 1), Ue(1), U(48, 0x0102_0304_0506)],
			&[U(2, 0), U(1, 0)],
		]);
		let full = SpsNALUnit::parse(&mut &nal[..]).unwrap().rbsp;
		assert!(full.pcm.is_some() && full.long_term_ref_pics.is_some());
		assert_eq!(full.short_term_ref_pic_sets.num_negative_pics, [1, 2]);
		let timing = full.vui_parameters.as_ref().unwrap().vui_timing_info.as_ref().unwrap();
		assert!(
			timing
				.hrd_parameters
				.as_ref()
				.unwrap()
				.common_inf
				.sub_pic_hrd_params
				.is_some()
		);
		assert!(full.range_extension.is_some() && full.multilayer_extension.is_some());
		assert!(full.sps_3d_extension.is_some() && full.scc_extension.is_some());
		assert_walks_to_the_end(&nal);
	}

	/// An SPS `check` refuses: [`parse_full`] reports `InvalidData` without calling scuffle_h265,
	/// and [`Sps::parse`] keeps the head.
	fn assert_refused(nal: &[u8]) {
		let err = parse_full(nal).unwrap_err();
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
		assert!(Sps::parse(nal).unwrap().full.is_none());
	}

	/// Each SPS reaches an overflow in scuffle_h265 0.2.2 or scuffle-expgolomb 0.1.5.
	#[test]
	fn overflowing_fields_are_refused() {
		// A 32x32 transform block in an 8x8 CTB: CtbLog2SizeY - MinTbLog2SizeY underflows.
		assert_refused(&synthetic(&[
			HEAD,
			&[Ue(0), Ue(0), Ue(3), Ue(0), Ue(0), Ue(0), U(1, 0)],
			END,
		]));

		// A 8x8 scaling list predicted from the matrix before the first: matrixId - delta wraps.
		let size_0 = [U(1, 0), Ue(0)].repeat(6);
		assert_refused(&synthetic(&[
			HEAD,
			BLOCKS,
			&[U(1, 1), U(1, 1)],
			&size_0,
			&[U(1, 0), Ue(1)],
			END,
		]));

		// 64 leading zeros: scuffle-expgolomb's `result - 1` wraps.
		let mut head = HEAD.to_vec();
		head[14] = Ue64; // log2_max_pic_order_cnt_lsb_minus4
		assert_refused(&synthetic(&[&head, BLOCKS, &[U(1, 0)], END]));

		// A palette of 2^32 - 2 entries, whose predictor initializers scuffle_h265 allocates.
		let end = &END[..END.len() - 1];
		let scc = [
			U(1, 1),
			U(4, 1),
			U(4, 0),
			U(1, 0),
			U(1, 1),
			Ue(u32::MAX - 1),
			Ue(0),
			U(1, 1),
		];
		assert_refused(&synthetic(&[HEAD, BLOCKS, &[U(1, 0)], end, &scc, &[Ue(u32::MAX - 2)]]));
	}
}

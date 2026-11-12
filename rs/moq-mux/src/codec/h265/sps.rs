//! H.265 sequence parameter sets (ITU-T H.265 7.3.2.2.1), read with a bounded reader.
//!
//! Real encoders break semantic constraints deep in the SPS: cameras ship `matrix_coeffs` 0 on
//! 4:2:0, which E.3.1 forbids and libavcodec and GStreamer decode anyway. The codec string, coded
//! size and hvcC header only need the head, which ends at `bit_depth_chroma_minus8`, so
//! [`Sps::parse_or_head`] keeps the head when a later value is refused. A truncated SPS, or one
//! whose head is invalid, is still an error.
//!
//! Every `ue(v)` stops at 2^32 - 2, as 9.2 bounds it, and every count that sizes a loop is bounded
//! before the loop runs, so reading a hostile SPS costs no more than its length.

use h264_reader::rbsp::{BitRead, BitReaderError};

use super::{Error, NalType, Result};

type ReadResult<T> = std::result::Result<T, BitReaderError>;

/// The SPS fields up to `bit_depth_chroma_minus8` (ITU-T H.265 7.3.2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpsHead {
	pub profile_space: u8,
	pub tier_flag: bool,
	pub profile_idc: u8,
	pub profile_compatibility_flags: [u8; 4],
	/// The progressive, interlaced, non-packed and frame-only flags in the first byte's high bits,
	/// as the hvcC and codec string carry them.
	pub constraint_flags: [u8; 6],
	pub level_idc: u8,
	pub max_sub_layers_minus1: u8,
	pub temporal_id_nesting_flag: bool,
	pub id: u8,
	pub chroma_format_idc: u8,
	pub separate_colour_plane_flag: bool,
	pub bit_depth_luma_minus8: u8,
	pub bit_depth_chroma_minus8: u8,
	/// The picture size inside the conformance window.
	pub width: u32,
	pub height: u32,
}

/// The SPS fields moq-mux reads after the head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpsTail {
	/// `log2_max_pic_order_cnt_lsb_minus4 + 4`: the width of a slice's POC LSB.
	pub log2_max_pic_order_cnt_lsb: u8,
	/// `sps_max_num_reorder_pics` at the highest sub-layer.
	pub max_num_reorder_pics: u32,
	/// The VUI, or its inferred defaults when the SPS carries none.
	pub vui: Vui,
}

/// The VUI fields moq-mux reads (E.2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Vui {
	/// The sample aspect ratio, when signalled with a registered `aspect_ratio_idc` or as a SAR.
	pub sample_aspect: Option<(u32, u32)>,
	/// The video signal type, when present.
	pub signal: Option<VideoSignal>,
	pub timing: Option<Timing>,
}

/// The VUI timing info (E.2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Timing {
	/// `vui_num_units_in_tick`, never 0.
	pub num_units_in_tick: u32,
	/// `vui_time_scale`, never 0.
	pub time_scale: u32,
	/// `vui_num_ticks_poc_diff_one_minus1 + 1`, when the POC is proportional to the timing.
	pub ticks_poc_diff_one: Option<u64>,
	pub hrd: Option<Hrd>,
}

/// The HRD parameters (E.2.2) at the highest sub-layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hrd {
	/// `elemental_duration_in_tc_minus1 + 1`, when the picture rate is fixed within the CVS.
	pub elemental_duration: Option<u64>,
	/// The NAL HRD's last schedule, or the VCL HRD's when that is all there is.
	pub schedule: Option<crate::codec::video::Hrd>,
}

/// An SPS with its cropped picture size and any parsed VUI signal.
#[derive(Debug, Clone)]
pub struct Sps {
	pub(crate) head: SpsHead,
	/// `None` when a value after the head was refused.
	pub(crate) tail: Option<SpsTail>,
}

/// The colour code points and range signalled by an H.265 VUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSignal {
	/// The colour primaries code point.
	pub primaries: u8,
	/// The transfer characteristics code point.
	pub transfer: u8,
	/// The matrix coefficients code point.
	pub matrix: u8,
	/// Whether the samples use the full numeric range.
	pub full_range: bool,
}

impl Sps {
	/// Parse a complete SPS NAL unit, including its header, refusing any invalid or truncated field.
	pub fn parse(nal: &[u8]) -> Result<Self> {
		let (head, tail) = read(nal)?;
		let tail = tail.map_err(|_| Error::SpsParse)?;
		Ok(Self { head, tail: Some(tail) })
	}

	/// Return the picture size inside its conformance window.
	pub fn size(&self) -> (u32, u32) {
		(self.head.width, self.head.height)
	}

	/// Return the VUI's video signal when present.
	pub fn video_signal(&self) -> Option<VideoSignal> {
		self.tail.as_ref()?.vui.signal
	}

	/// Parse an SPS NAL unit, keeping only its head when a later value is refused.
	pub(crate) fn parse_or_head(nal: &[u8]) -> Result<Self> {
		let (head, tail) = read(nal)?;
		let tail = tail
			.inspect_err(|err| {
				tracing::warn!(
					?err,
					"H.265 SPS refused after its head; using the head, without the VUI"
				)
			})
			.ok();
		Ok(Self { head, tail })
	}

	/// The colour the VUI's video signal type carries.
	pub(crate) fn color(&self) -> Option<hang::catalog::Color> {
		let signal = self.video_signal()?;
		// Unspecified code points in limited range are what an absent signal infers (E.3.1), so
		// only a departure from them was signalled. A stream that signals limited range and
		// nothing else reads as unsignalled, which a renderer assumes anyway.
		if (signal.primaries, signal.transfer, signal.matrix, signal.full_range) == (2, 2, 2, false) {
			return None;
		}
		crate::codec::color::cicp(
			signal.primaries,
			signal.transfer,
			signal.matrix,
			Some(signal.full_range),
		)
	}
}

impl Vui {
	/// One picture's duration at the highest sub-layer as `(units, scale)`, when the HRD
	/// parameters fix the picture rate within the CVS (E.3.2). Without that the VUI tick only
	/// bounds the rate from above.
	pub(crate) fn period(&self) -> Option<(u64, u64)> {
		let timing = self.timing.as_ref()?;
		let elemental = timing.hrd.as_ref()?.elemental_duration?;
		let units = elemental.checked_mul(u64::from(timing.num_units_in_tick))?;
		Some((units, u64::from(timing.time_scale)))
	}
}

/// Read an SPS NAL unit (header included): its head, and the rest or the error that refused it.
///
/// A truncated SPS, or one whose header or head is invalid, is an error.
fn read(nal: &[u8]) -> Result<(SpsHead, ReadResult<SpsTail>)> {
	let [header, header2, ..] = *nal else {
		return Err(Error::SpsParse);
	};
	// forbidden_zero_bit, nal_unit_type, and TemporalId 0 for an SPS (7.4.2.2).
	if header & 0x80 != 0 || NalType::of(header) != NalType::SPS || header2 & 0x07 != 1 {
		return Err(Error::SpsParse);
	}
	let mut r = crate::codec::annexb::rbsp(nal, 2);
	let head = read_head(&mut r).map_err(|_| Error::SpsParse)?;
	match read_tail(&mut r, &head) {
		Err(BitReaderError::ReaderError(_, err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
			Err(Error::SpsParse)
		}
		tail => Ok((head, tail)),
	}
}

/// Refuse `field` unless `valid`.
fn ensure(valid: bool, field: &'static str) -> ReadResult<()> {
	match valid {
		true => Ok(()),
		false => Err(BitReaderError::ReaderError(
			field,
			std::io::ErrorKind::InvalidData.into(),
		)),
	}
}

/// Read the SPS RBSP through `bit_depth_chroma_minus8`.
fn read_head(r: &mut impl BitRead) -> ReadResult<SpsHead> {
	r.skip(4, "sps_video_parameter_set_id")?;
	let max_sub_layers_minus1 = r.read::<3, u8>("sps_max_sub_layers_minus1")?;
	let temporal_id_nesting_flag = r.read_bit("sps_temporal_id_nesting_flag")?;
	// 7.4.3.2.1: a single sub-layer is nested.
	ensure(
		max_sub_layers_minus1 > 0 || temporal_id_nesting_flag,
		"sps_temporal_id_nesting_flag",
	)?;

	// profile_tier_level(1, sps_max_sub_layers_minus1), 7.3.3
	let profile_space = r.read::<2, u8>("general_profile_space")?;
	let tier_flag = r.read_bit("general_tier_flag")?;
	let profile_idc = r.read::<5, u8>("general_profile_idc")?;
	let profile_compatibility_flags = r.read::<32, u32>("general_profile_compatibility_flags")?.to_be_bytes();
	let constraint_flags = [r.read::<4, u8>("general_constraint_flags")? << 4, 0, 0, 0, 0, 0];
	r.skip(44, "general_reserved_zero_43bits")?; // and general_inbld_flag
	let level_idc = r.read::<8, u8>("general_level_idc")?;
	skip_sub_layers(r, max_sub_layers_minus1)?;

	let id = r.read_ue("sps_seq_parameter_set_id")?;
	ensure(id <= 15, "sps_seq_parameter_set_id")?;
	let chroma_format_idc = r.read_ue("chroma_format_idc")?;
	ensure(chroma_format_idc <= 3, "chroma_format_idc")?;
	let separate_colour_plane_flag = chroma_format_idc == 3 && r.read_bit("separate_colour_plane_flag")?;
	let pic_width = r.read_ue("pic_width_in_luma_samples")?;
	let pic_height = r.read_ue("pic_height_in_luma_samples")?;
	let [left, right, top, bottom] = match r.read_bit("conformance_window_flag")? {
		true => [
			r.read_ue("conf_win_left_offset")?,
			r.read_ue("conf_win_right_offset")?,
			r.read_ue("conf_win_top_offset")?,
			r.read_ue("conf_win_bottom_offset")?,
		],
		false => [0; 4],
	};
	let bit_depth_luma_minus8 = r.read_ue("bit_depth_luma_minus8")?;
	let bit_depth_chroma_minus8 = r.read_ue("bit_depth_chroma_minus8")?;
	ensure(
		bit_depth_luma_minus8 <= 8 && bit_depth_chroma_minus8 <= 8,
		"bit_depth_minus8",
	)?;

	// The window offsets count chroma samples: SubWidthC and SubHeightC from Table 6-1.
	let sub_width_c = if matches!(chroma_format_idc, 1 | 2) { 2 } else { 1 };
	let sub_height_c = if chroma_format_idc == 1 { 2 } else { 1 };
	let width = pic_width.checked_sub(left.saturating_add(right).saturating_mul(sub_width_c));
	let height = pic_height.checked_sub(top.saturating_add(bottom).saturating_mul(sub_height_c));
	let (Some(width @ 1..), Some(height @ 1..)) = (width, height) else {
		return Err(BitReaderError::ReaderError(
			"conformance_window",
			std::io::ErrorKind::InvalidData.into(),
		));
	};

	Ok(SpsHead {
		profile_space,
		tier_flag,
		profile_idc,
		profile_compatibility_flags,
		constraint_flags,
		level_idc,
		max_sub_layers_minus1,
		temporal_id_nesting_flag,
		id: id as u8,
		chroma_format_idc: chroma_format_idc as u8,
		separate_colour_plane_flag,
		bit_depth_luma_minus8: bit_depth_luma_minus8 as u8,
		bit_depth_chroma_minus8: bit_depth_chroma_minus8 as u8,
		width,
		height,
	})
}

/// Read `sps_seq_parameter_set_id` from an SPS RBSP (the payload after the NAL header) in layer
/// `layer`, whose `nuh_layer_id` picks the layout (F.7.3.2.2.1).
pub(crate) fn id(r: &mut impl BitRead, layer: u8) -> ReadResult<u32> {
	r.skip(4, "sps_video_parameter_set_id")?;
	let max_sub_layers_minus1 = r.read::<3, u8>("sps_ext_or_max_sub_layers_minus1")?;
	// MultiLayerExtSpsFlag: an enhancement layer's SPS that inherits its profile_tier_level.
	if layer > 0 && max_sub_layers_minus1 == 7 {
		return r.read_ue("sps_seq_parameter_set_id");
	}
	r.skip(1, "sps_temporal_id_nesting_flag")?;
	r.skip(96, "general_profile_and_level")?; // the general profile's 88 bits and general_level_idc
	skip_sub_layers(r, max_sub_layers_minus1)?;
	r.read_ue("sps_seq_parameter_set_id")
}

/// Skip the sub-layer half of profile_tier_level (7.3.3), which follows `general_level_idc`.
fn skip_sub_layers(r: &mut impl BitRead, max_sub_layers_minus1: u8) -> ReadResult<()> {
	// 7.4.3.2.1: at most 7 sub-layers.
	ensure(max_sub_layers_minus1 <= 6, "sps_max_sub_layers_minus1")?;
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

/// Read the SPS RBSP after the head, through `rbsp_stop_one_bit`.
fn read_tail(r: &mut impl BitRead, head: &SpsHead) -> ReadResult<SpsTail> {
	let log2_max_pic_order_cnt_lsb_minus4 = r.read_ue("log2_max_pic_order_cnt_lsb_minus4")?;
	ensure(
		log2_max_pic_order_cnt_lsb_minus4 <= 12,
		"log2_max_pic_order_cnt_lsb_minus4",
	)?;
	let log2_max_pic_order_cnt_lsb = log2_max_pic_order_cnt_lsb_minus4 as u8 + 4;

	// Without the ordering info for every sub-layer, only the highest is signalled.
	let first = match r.read_bit("sps_sub_layer_ordering_info_present_flag")? {
		true => 0,
		false => head.max_sub_layers_minus1,
	};
	let mut max_num_reorder_pics = 0;
	for _ in first..=head.max_sub_layers_minus1 {
		let max_dec_pic_buffering_minus1 = r.read_ue("sps_max_dec_pic_buffering_minus1")?;
		// No profile's MaxDpbSize exceeds 16 (A.4.2), which bounds the reorder depth below it.
		ensure(max_dec_pic_buffering_minus1 < 16, "sps_max_dec_pic_buffering_minus1")?;
		max_num_reorder_pics = r.read_ue("sps_max_num_reorder_pics")?;
		ensure(
			max_num_reorder_pics <= max_dec_pic_buffering_minus1,
			"sps_max_num_reorder_pics",
		)?;
		r.read_ue("sps_max_latency_increase_plus1")?;
	}

	let min_cb_log2_size = u64::from(r.read_ue("log2_min_luma_coding_block_size_minus3")?) + 3;
	let ctb_log2_size = min_cb_log2_size + u64::from(r.read_ue("log2_diff_max_min_luma_coding_block_size")?);
	let min_tb_log2_size = u64::from(r.read_ue("log2_min_luma_transform_block_size_minus2")?) + 2;
	// 7.4.3.2.1 keeps MinTbLog2SizeY below MinCbLog2SizeY, and so below CtbLog2SizeY.
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
			r.skip(u32::from(log2_max_pic_order_cnt_lsb) + 1, "lt_ref_pic_poc_lsb_sps")?;
		}
	}
	r.skip(2, "sps_temporal_mvp_enabled_flag")?; // and strong_intra_smoothing_enabled_flag
	let vui = match r.read_bit("vui_parameters_present_flag")? {
		true => vui_parameters(r, head)?,
		false => Vui::default(),
	};
	let tail = SpsTail {
		log2_max_pic_order_cnt_lsb,
		max_num_reorder_pics,
		vui,
	};

	if r.read_bit("sps_extension_present_flag")? {
		let range = r.read_bit("sps_range_extension_flag")?;
		let multilayer = r.read_bit("sps_multilayer_extension_flag")?;
		let three_d = r.read_bit("sps_3d_extension_flag")?;
		let scc = r.read_bit("sps_scc_extension_flag")?;
		let extension_4bits = r.read::<4, u8>("sps_extension_4bits")?;
		if range {
			r.skip(9, "sps_range_extension")?;
		}
		if multilayer {
			r.skip(1, "inter_view_mv_vert_constraint_flag")?;
		}
		if three_d {
			// sps_3d_extension (I.7.3.2.2.5), for d = 0 then d = 1.
			r.skip(2, "iv_di_mc_enabled_flag")?; // and iv_mv_scal_enabled_flag
			r.read_ue("log2_ivmc_sub_pb_size_minus3")?;
			r.skip(4, "iv_res_pred_enabled_flag")?; // through dbbp_enabled_flag
			r.skip(3, "iv_di_mc_enabled_flag")?; // iv_mv_scal_enabled_flag, then tex_mc_enabled_flag
			r.read_ue("log2_texmc_sub_pb_size_minus3")?;
			r.skip(5, "intra_contour_enabled_flag")?; // through skip_intra_enabled_flag
		}
		if scc {
			let luma = u32::from(head.bit_depth_luma_minus8) + 8;
			let chroma = u32::from(head.bit_depth_chroma_minus8) + 8;
			let bits = if head.chroma_format_idc == 0 {
				luma
			} else {
				luma + 2 * chroma
			};
			sps_scc_extension(r, bits)?;
		}
		// sps_extension_data_flag bits follow, which decoders ignore (7.4.3.2.1). They run to the
		// last set bit, the stop bit, so a set bit must remain.
		if extension_4bits != 0 {
			while !r.read_bit("sps_extension_data_flag")? {}
			read_rest(r)?;
			return Ok(tail);
		}
	}
	ensure(r.read_bit("rbsp_stop_one_bit")?, "rbsp_stop_one_bit")?;
	ensure(!read_rest(r)?, "rbsp_alignment_zero_bit")?;
	Ok(tail)
}

/// Read the RBSP to its end, which checks its emulation prevention, returning whether a bit was set.
/// Trailing zero bytes are already gone, so nothing set after the stop bit means the RBSP ends there.
fn read_rest(r: &mut impl BitRead) -> ReadResult<bool> {
	let mut set = false;
	loop {
		match r.read_bit("rbsp_trailing_bits") {
			Ok(bit) => set |= bit,
			Err(BitReaderError::ReaderError(_, err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
				return Ok(set);
			}
			Err(err) => return Err(err),
		}
	}
}

/// `scaling_list_data` (7.3.4).
fn scaling_list_data(r: &mut impl BitRead) -> ReadResult<()> {
	for size_id in 0..4 {
		let step = if size_id == 3 { 3 } else { 1 };
		for matrix_id in (0..6).step_by(step) {
			if !r.read_bit("scaling_list_pred_mode_flag")? {
				let delta = r.read_ue("scaling_list_pred_matrix_id_delta")?;
				// 7.4.5: the reference is this matrix or an earlier one.
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
fn short_term_ref_pic_sets(r: &mut impl BitRead) -> ReadResult<()> {
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
			let mut read = |count: u32, sign: i32| -> ReadResult<Vec<i32>> {
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
fn vui_parameters(r: &mut impl BitRead, head: &SpsHead) -> ReadResult<Vui> {
	let mut vui = Vui::default();
	if r.read_bit("aspect_ratio_info_present_flag")? {
		vui.sample_aspect = match r.read::<8, u8>("aspect_ratio_idc")? {
			// EXTENDED_SAR
			255 => Some((r.read::<16, u32>("sar_width")?, r.read::<16, u32>("sar_height")?)),
			idc => crate::codec::video::sample_aspect(idc),
		};
	}
	if r.read_bit("overscan_info_present_flag")? {
		r.skip(1, "overscan_appropriate_flag")?;
	}
	if r.read_bit("video_signal_type_present_flag")? {
		r.skip(3, "video_format")?;
		let full_range = r.read_bit("video_full_range_flag")?;
		// E.3.1 infers 2, unspecified, for an absent colour description.
		let (primaries, transfer, matrix) = match r.read_bit("colour_description_present_flag")? {
			true => (
				r.read::<8, u8>("colour_primaries")?,
				r.read::<8, u8>("transfer_characteristics")?,
				r.read::<8, u8>("matrix_coeffs")?,
			),
			false => (2, 2, 2),
		};
		// E.3.1: identity (0) needs 4:4:4 at one bit depth, and YCgCo (8) one bit depth, or 4:4:4
		// with chroma a bit deeper.
		let (luma, chroma) = (head.bit_depth_luma_minus8, head.bit_depth_chroma_minus8);
		let yuv444 = head.chroma_format_idc == 3;
		ensure(matrix != 0 || (luma == chroma && yuv444), "matrix_coeffs")?;
		ensure(
			matrix != 8 || luma == chroma || (chroma == luma + 1 && yuv444),
			"matrix_coeffs",
		)?;
		vui.signal = Some(VideoSignal {
			primaries,
			transfer,
			matrix,
			full_range,
		});
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
		let num_units_in_tick = r.read::<32, u32>("vui_num_units_in_tick")?;
		let time_scale = r.read::<32, u32>("vui_time_scale")?;
		// E.3.1: both are greater than 0.
		ensure(num_units_in_tick > 0 && time_scale > 0, "vui_time_scale")?;
		let ticks_poc_diff_one = match r.read_bit("vui_poc_proportional_to_timing_flag")? {
			true => Some(u64::from(r.read_ue("vui_num_ticks_poc_diff_one_minus1")?) + 1),
			false => None,
		};
		let hrd = match r.read_bit("vui_hrd_parameters_present_flag")? {
			true => Some(hrd_parameters(r, head.max_sub_layers_minus1)?),
			false => None,
		};
		vui.timing = Some(Timing {
			num_units_in_tick,
			time_scale,
			ticks_poc_diff_one,
			hrd,
		});
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
	Ok(vui)
}

/// `hrd_parameters(1, sps_max_sub_layers_minus1)` (E.2.2), with its `sub_layer_hrd_parameters`
/// (E.2.3), keeping the highest sub-layer's.
fn hrd_parameters(r: &mut impl BitRead, max_sub_layers_minus1: u8) -> ReadResult<Hrd> {
	let nal = r.read_bit("nal_hrd_parameters_present_flag")?;
	let vcl = r.read_bit("vcl_hrd_parameters_present_flag")?;
	let (mut sub_pic, mut bit_rate_scale, mut cpb_size_scale) = (false, 0, 0);
	if nal || vcl {
		sub_pic = r.read_bit("sub_pic_hrd_params_present_flag")?;
		if sub_pic {
			r.skip(19, "tick_divisor_minus2")?; // through dpb_output_delay_du_length_minus1
		}
		bit_rate_scale = r.read::<4, u32>("bit_rate_scale")?;
		cpb_size_scale = r.read::<4, u32>("cpb_size_scale")?;
		if sub_pic {
			r.skip(4, "cpb_size_du_scale")?;
		}
		r.skip(15, "initial_cpb_removal_delay_length_minus1")?; // through dpb_output_delay_length_minus1
	}
	let mut highest = None;
	for _ in 0..=max_sub_layers_minus1 {
		// fixed_pic_rate_within_cvs_flag is inferred 1 when fixed_pic_rate_general_flag is.
		let fixed = r.read_bit("fixed_pic_rate_general_flag")? || r.read_bit("fixed_pic_rate_within_cvs_flag")?;
		let elemental_duration = match fixed {
			true => Some(u64::from(r.read_ue("elemental_duration_in_tc_minus1")?) + 1),
			false => None,
		};
		let low_delay = !fixed && r.read_bit("low_delay_hrd_flag")?;
		let cpb_cnt_minus1 = match low_delay {
			true => 0,
			false => r.read_ue("cpb_cnt_minus1")?,
		};
		ensure(cpb_cnt_minus1 <= 31, "cpb_cnt_minus1")?;
		let mut schedule = None;
		// The NAL HRD's schedules come first when both are present.
		for hrd in 0..u32::from(nal) + u32::from(vcl) {
			for _ in 0..=cpb_cnt_minus1 {
				let bit_rate_value_minus1 = r.read_ue("bit_rate_value_minus1")?;
				let cpb_size_value_minus1 = r.read_ue("cpb_size_value_minus1")?;
				if sub_pic {
					r.read_ue("cpb_size_du_value_minus1")?;
					r.read_ue("bit_rate_du_value_minus1")?;
				}
				r.skip(1, "cbr_flag")?;
				if hrd == 0 {
					// (E-75) and (E-76).
					schedule = Some(crate::codec::video::Hrd {
						bit_rate: (u64::from(bit_rate_value_minus1) + 1) << (6 + bit_rate_scale),
						cpb_size: (u64::from(cpb_size_value_minus1) + 1) << (4 + cpb_size_scale),
					});
				}
			}
		}
		highest = Some(Hrd {
			elemental_duration,
			schedule,
		});
	}
	Ok(highest.expect("the loop reads at least one sub-layer"))
}

/// `sps_scc_extension` (7.3.2.2.3), with the bits of one predictor initializer across components.
fn sps_scc_extension(r: &mut impl BitRead, bits: u32) -> ReadResult<()> {
	r.skip(1, "sps_curr_pic_ref_enabled_flag")?;
	if r.read_bit("palette_mode_enabled_flag")? {
		let palette_max_size = r.read_ue("palette_max_size")?;
		ensure(palette_max_size <= 64, "palette_max_size")?;
		let delta = r.read_ue("delta_palette_max_predictor_size")?;
		// 7.4.3.3.8: PaletteMaxPredictorSize is at most 128.
		let max_predictor_size = u64::from(palette_max_size) + u64::from(delta);
		ensure(max_predictor_size <= 128, "delta_palette_max_predictor_size")?;
		if r.read_bit("sps_palette_predictor_initializers_present_flag")? {
			let initializers = r.read_ue("sps_num_palette_predictor_initializers_minus1")?;
			ensure(
				u64::from(initializers) < max_predictor_size,
				"sps_num_palette_predictor_initializers_minus1",
			)?;
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

	/// scuffle-h265's `sps_parse5` test SPS, which carries `scaling_list_data`.
	const SPS_SCALING_LIST: &[u8] = &[
		0x42, 0x01, 0x01, 0x24, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9d, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x99, 0xb0,
		0x01, 0xe0, 0x20, 0x02, 0x1c, 0x4d, 0x94, 0xd6, 0xed, 0xbe, 0x41, 0x12, 0x64, 0xeb, 0x25, 0x11, 0x44, 0x1a,
		0x6c, 0x9d, 0x64, 0xa2, 0x29, 0x09, 0x26, 0xba, 0xf5, 0xff, 0xeb, 0xfa, 0xfd, 0x7f, 0xeb, 0xf5, 0x44, 0x51,
		0x04, 0x93, 0x5d, 0x7a, 0xff, 0xf5, 0xfd, 0x7e, 0xbf, 0xf5, 0xfa, 0xc8, 0xa4, 0x92, 0x4d, 0x75, 0xeb, 0xff,
		0xd7, 0xf5, 0xfa, 0xff, 0xd7, 0xea, 0x88, 0xa2, 0x24, 0x93, 0x5d, 0x7a, 0xff, 0xf5, 0xfd, 0x7e, 0xbf, 0xf5,
		0xfa, 0xc8, 0x94, 0x08, 0x53, 0x49, 0x29, 0x24, 0x89, 0x55, 0x12, 0xa5, 0x2a, 0x94, 0xc1, 0x35, 0x01, 0x01,
		0x01, 0x03, 0xb8, 0x40, 0x20, 0x80,
	];

	/// scuffle-h265's `sps_parse_inter_ref_prediction` test SPS: 26 short-term RPSs, most of them
	/// predicted from the one before.
	const SPS_INTER_RPS: &[u8] = &[
		0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
		0x00, 0xa0, 0x0b, 0x08, 0x04, 0x85, 0x96, 0x5e, 0x49, 0x1b, 0x60, 0xd9, 0x78, 0x88, 0x88, 0x8f, 0xe7, 0x9f,
		0xcf, 0xe7, 0xf3, 0xf9, 0xfc, 0xf2, 0xff, 0xff, 0xff, 0xcf, 0xe7, 0xf3, 0xf9, 0xfc, 0xfe, 0x7f, 0x3f, 0x3f,
		0x9f, 0xcf, 0xe7, 0xf3, 0xf9, 0xdb, 0x20,
	];

	/// The parser reads `nal` to its stop bit, with only zeros after it in that byte, so it read
	/// every field the SPS holds.
	fn assert_reads_to_the_end(nal: &[u8]) -> Sps {
		let mut r = crate::codec::annexb::rbsp(nal, 2);
		let head = read_head(&mut r).unwrap();
		read_tail(&mut r, &head).unwrap();
		while !r.byte_aligned() {
			assert!(!r.read_bit("rbsp_alignment_zero_bit").unwrap());
		}
		Sps::parse(nal).unwrap()
	}

	/// Every fixture parses whole, through every branch of profile_tier_level, the scaling
	/// lists, and predicted RPSs.
	#[test]
	fn fixtures_read_to_the_end() {
		for nal in [
			fixtures::SPS,
			fixtures::SPS_HDR10,
			fixtures::SPS_PYRAMID,
			fixtures::SPS_NVENC,
			fixtures::SPS_ANAMORPHIC,
			SPS_SUB_LAYERS,
			SPS_SUB_LAYER_PROFILE_AND_LEVEL,
			SPS_SCALING_LIST,
			SPS_INTER_RPS,
			fixtures::MV_SPS,
		] {
			let sps = assert_reads_to_the_end(nal);
			assert_eq!(Sps::parse_or_head(nal).unwrap().tail, sps.tail);
			assert_eq!(
				id(&mut crate::codec::annexb::rbsp(nal, 2), 0).unwrap(),
				u32::from(sps.head.id)
			);
		}
	}

	/// An enhancement layer's SPS that inherits its profile_tier_level carries its id right after
	/// `sps_ext_or_max_sub_layers_minus1`, where the base layout reads a refused sub-layer count.
	#[test]
	fn multi_layer_ext_sps_id() {
		let nal = fixtures::MV_SPS_LAYER1;
		assert_eq!(id(&mut crate::codec::annexb::rbsp(nal, 2), 1).unwrap(), 1);
		assert!(id(&mut crate::codec::annexb::rbsp(nal, 2), 0).is_err());
	}

	/// The fields callers read, from x265's HDR10 and B-pyramid encodes and nvenc's anamorphic one.
	#[test]
	fn fixtures_read_their_fields() {
		let hdr10 = Sps::parse(fixtures::SPS_HDR10).unwrap();
		assert_eq!(hdr10.size(), (64, 64));
		assert_eq!(hdr10.head.bit_depth_luma_minus8, 2);
		assert_eq!(
			hdr10.video_signal(),
			Some(VideoSignal {
				primaries: 9,
				transfer: 16,
				matrix: 9,
				full_range: false,
			})
		);

		let pyramid = Sps::parse(fixtures::SPS_PYRAMID).unwrap().tail.unwrap();
		assert_eq!(pyramid.max_num_reorder_pics, 2);
		assert_eq!(pyramid.vui.period(), Some((1, 25)));

		let anamorphic = Sps::parse(fixtures::SPS_ANAMORPHIC).unwrap();
		assert_eq!(anamorphic.size(), (1440, 1080));
		assert_eq!(anamorphic.tail.unwrap().vui.sample_aspect, Some((4, 3)));
	}

	/// A sub-layer that signals both its profile and its level carries `sub_layer_level_idc` once
	/// (7.3.3), as GStreamer's parser reads it.
	#[test]
	fn sub_layer_profile_and_level() {
		let expected = SpsHead {
			max_sub_layers_minus1: 2,
			..Sps::parse(fixtures::SPS).unwrap().head
		};
		assert_eq!(Sps::parse(SPS_SUB_LAYER_PROFILE_AND_LEVEL).unwrap().head, expected);
		assert_eq!(Sps::parse(SPS_SUB_LAYERS).unwrap().head, expected);
	}

	/// Zeroed VUI colour fields on 4:2:0 break E.3.1: the head carries on, and `parse` refuses it.
	#[test]
	fn zeroed_colour_description_falls_back_to_the_head() {
		let sps = Sps::parse_or_head(fixtures::SPS_ZERO_COLOUR).unwrap();
		assert!(sps.tail.is_none());
		assert!(Sps::parse(fixtures::SPS_ZERO_COLOUR).is_err());
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
				id: 0,
				chroma_format_idc: 1,
				separate_colour_plane_flag: false,
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
		// The 64x64 4:2:0 picture's offsets count pairs of luma samples.
		let window = |offsets: [u32; 4]| {
			let mut head = HEAD.to_vec();
			head.splice(11..12, [U(1, 1)].into_iter().chain(offsets.map(Ue)));
			synthetic(&[&head, BLOCKS, &[U(1, 0)], END])
		};
		assert_eq!(Sps::parse(&window([1, 2, 3, 4])).unwrap().size(), (58, 50));
		for offsets in [[0, 32, 0, 0], [16, 17, 0, 0], [0, 0, 32, 0], [0, 0, 0, u32::MAX - 1]] {
			assert!(Sps::parse_or_head(&window(offsets)).is_err(), "{offsets:?}");
		}
	}

	/// The fallback covers a refusal after the head, not a truncated or invalid head.
	#[test]
	fn truncated_or_invalid_head_is_still_an_error() {
		assert!(Sps::parse_or_head(&fixtures::SPS[..8]).is_err());
		assert!(Sps::parse_or_head(&fixtures::SPS_ZERO_COLOUR[..20]).is_err());
		assert!(Sps::parse_or_head(&fixtures::SPS[..fixtures::SPS.len() - 6]).is_err());
		// TemporalId 1, then a single sub-layer without temporal nesting.
		for (index, byte) in [(1, 0x02), (2, 0x00)] {
			let mut nal = fixtures::SPS_ZERO_COLOUR.to_vec();
			nal[index] = byte;
			assert!(Sps::parse_or_head(&nal).is_err());
		}
		assert!(Sps::parse_or_head(fixtures::PPS).is_err());
		assert!(Sps::parse_or_head(fixtures::VPS).is_err());
	}

	/// A syntax element of a synthetic SPS RBSP.
	#[derive(Clone, Copy)]
	enum Field {
		/// `u(n)`: the low `n` bits of the value.
		U(u32, u64),
		Ue(u32),
		/// An Exp-Golomb code with 64 leading zeros, past the 32 that 9.2 allows.
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

	/// Every branch the fixtures leave out: PCM, long-term pictures, a VUI with sub-picture HRD
	/// parameters, and every extension.
	#[test]
	fn synthetic_reads_every_branch() {
		assert_reads_to_the_end(&synthetic(&[HEAD, BLOCKS, &[U(1, 0)], END]));

		let vui_and_extensions: [&[Field]; 10] = [
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
			&[U(1, 1), U(19, 0), U(4, 1), U(4, 2), U(4, 0), U(15, 0x5ef7)],
			// One sub-layer without a fixed picture rate, two CPBs, then the NAL and VCL schedules.
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
				Ue(12),
				Ue(18),
				Ue(1),
				Ue(1),
				U(1, 0),
				Ue(13),
				Ue(17),
				Ue(1),
				Ue(1),
				U(1, 0),
			],
			// Bitstream restriction.
			&[U(1, 1), U(3, 0), Ue(0), Ue(0), Ue(0), Ue(0), Ue(0)],
			// Every extension: range, multilayer, 3D (d = 0, then d = 1), then SCC with two palette
			// initializers.
			&[
				U(1, 1),
				U(4, 0xf),
				U(4, 0),
				U(9, 0),
				U(1, 0),
				U(2, 0),
				Ue(0),
				U(4, 0),
				U(3, 0),
				Ue(1),
				U(5, 0),
				U(1, 0),
				U(1, 1),
				Ue(4),
				Ue(2),
				U(1, 1),
				Ue(1),
				U(48, 0x0102_0304_0506),
				U(3, 0),
			],
		];
		let mut fields = vec![
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
		];
		fields.extend(vui_and_extensions);
		let tail = assert_reads_to_the_end(&synthetic(&fields)).tail.unwrap();
		assert_eq!(
			tail.vui,
			Vui {
				sample_aspect: Some((4, 3)),
				signal: Some(VideoSignal {
					primaries: 1,
					transfer: 1,
					matrix: 1,
					full_range: false,
				}),
				timing: Some(Timing {
					num_units_in_tick: 1001,
					time_scale: 60000,
					ticks_poc_diff_one: Some(2),
					hrd: Some(Hrd {
						elemental_duration: None,
						// The NAL HRD's last schedule, scaled by 2^(6 + 1) and 2^(4 + 2).
						schedule: Some(crate::codec::video::Hrd {
							bit_rate: 12 << 7,
							cpb_size: 20 << 6,
						}),
					}),
				}),
			}
		);

		// Nonzero sps_extension_4bits: the sps_extension_data_flag bits after the extensions are
		// ignored, stop bit included.
		let mut fields = vec![HEAD, BLOCKS, &[U(1, 0)], &END[..END.len() - 1]];
		fields.push(&[U(1, 1), U(4, 0), U(4, 1), U(7, 0x55)]);
		Sps::parse(&synthetic(&fields)).unwrap();
	}

	/// With nonzero sps_extension_4bits, an SPS whose remaining bits are all zeros has no stop bit.
	#[test]
	fn extension_data_without_stop_bit_is_truncated() {
		// Zero sps_extension_data_flag bits pad the stop bit to the start of the last byte.
		let nal = (0..8)
			.map(|padding| {
				synthetic(&[
					HEAD,
					BLOCKS,
					&[U(1, 0)],
					&END[..END.len() - 1],
					&[U(1, 1), U(4, 0), U(4, 1), U(padding, 0)],
				])
			})
			.find(|nal| nal.last() == Some(&0x80))
			.unwrap();
		Sps::parse(&nal).unwrap();

		// Zero the stop bit, or drop its byte.
		let mut zeroed = nal.clone();
		*zeroed.last_mut().unwrap() = 0;
		for nal in [&zeroed[..], &nal[..nal.len() - 1]] {
			assert!(matches!(Sps::parse(nal), Err(Error::SpsParse)));
			assert!(matches!(Sps::parse_or_head(nal), Err(Error::SpsParse)));
		}
	}

	/// After the stop bit, only zeros may follow; after extension data, only valid RBSP bytes.
	#[test]
	fn data_after_the_stop_bit_is_refused() {
		let mut nal = synthetic(&[HEAD, BLOCKS, &[U(1, 0)], END]);
		nal.push(0x80);
		assert_refused(&nal);

		// Extension data past h264-reader's 128-byte read-ahead, so only reading to the end checks
		// its emulation prevention.
		let data = [U(32, 0x5555_5555)].repeat(40);
		let mut nal = synthetic(&[
			HEAD,
			BLOCKS,
			&[U(1, 0)],
			&END[..END.len() - 1],
			&[U(1, 1), U(4, 0), U(4, 1)],
			&data,
		]);
		Sps::parse(&nal).unwrap();
		// A start code inside the extension data, which emulation prevention forbids.
		let stop = nal.pop().unwrap();
		nal.extend([0x00, 0x00, 0x01, stop]);
		assert_refused(&nal);
	}

	/// An SPS refused after its head: [`Sps::parse`] errors, and [`Sps::parse_or_head`] keeps the
	/// head.
	fn assert_refused(nal: &[u8]) {
		assert!(Sps::parse(nal).is_err());
		assert!(Sps::parse_or_head(nal).unwrap().tail.is_none());
	}

	/// Each value is out of the spec's range, and most would size a loop or a shift.
	#[test]
	fn out_of_range_fields_are_refused() {
		// A 32x32 transform block in an 8x8 CTB.
		assert_refused(&synthetic(&[
			HEAD,
			&[Ue(0), Ue(0), Ue(3), Ue(0), Ue(0), Ue(0), U(1, 0)],
			END,
		]));

		// A 8x8 scaling list predicted from the matrix before the first.
		let size_0 = [U(1, 0), Ue(0)].repeat(6);
		assert_refused(&synthetic(&[
			HEAD,
			BLOCKS,
			&[U(1, 1), U(1, 1)],
			&size_0,
			&[U(1, 0), Ue(1)],
			END,
		]));

		// 64 leading zeros, past the 32 that 9.2 allows.
		let mut head = HEAD.to_vec();
		head[14] = Ue64; // log2_max_pic_order_cnt_lsb_minus4
		assert_refused(&synthetic(&[&head, BLOCKS, &[U(1, 0)], END]));

		// A decoded picture buffer past MaxDpbSize, with as deep a reorder.
		let mut head = HEAD.to_vec();
		head[16] = Ue(16); // sps_max_dec_pic_buffering_minus1
		head[17] = Ue(16); // sps_max_num_reorder_pics
		assert_refused(&synthetic(&[&head, BLOCKS, &[U(1, 0)], END]));

		// More pictures reordered than buffered.
		let mut head = HEAD.to_vec();
		head[17] = Ue(5); // sps_max_num_reorder_pics
		assert_refused(&synthetic(&[&head, BLOCKS, &[U(1, 0)], END]));

		// A palette of 2^32 - 2 entries.
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

		// A VUI tick of zero, which a frame rate would divide by.
		let end = &END[..END.len() - 2];
		let vui = [
			U(1, 1),
			U(1, 0),
			U(1, 0),
			U(1, 0),
			U(1, 0),
			U(3, 0),
			U(1, 0),
			U(1, 1),
			U(32, 0),
			U(32, 25),
		];
		assert_refused(&synthetic(&[
			HEAD,
			BLOCKS,
			&[U(1, 0)],
			end,
			&vui,
			&[U(2, 0), U(1, 0), U(1, 0)],
		]));
	}
}

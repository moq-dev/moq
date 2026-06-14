// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::fmt;
use std::io::Write;

use crate::codec::h264::nalu_writer::NaluWriter;
use crate::codec::h264::nalu_writer::NaluWriterError;
use crate::codec::h264::parser::HrdParams;
use crate::codec::h264::parser::NaluType;
use crate::codec::h264::parser::Pps;
use crate::codec::h264::parser::SliceHeader;
#[cfg(feature = "vaapi")]
use crate::codec::h264::parser::SliceType;
use crate::codec::h264::parser::Sps;
use crate::codec::h264::parser::DEFAULT_4X4_INTER;
use crate::codec::h264::parser::DEFAULT_4X4_INTRA;
use crate::codec::h264::parser::DEFAULT_8X8_INTER;
use crate::codec::h264::parser::DEFAULT_8X8_INTRA;
#[cfg(feature = "vaapi")]
use crate::encode::IsReference;

mod private {
	pub trait NaluStruct {}
}

impl private::NaluStruct for Sps {}

impl private::NaluStruct for Pps {}

impl private::NaluStruct for SliceHeader {}

#[derive(Debug)]
pub enum SynthesizerError {
	Unsupported,
	NaluWriter(NaluWriterError),
}

impl fmt::Display for SynthesizerError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			SynthesizerError::Unsupported => write!(f, "tried to synthesize unsupported settings"),
			SynthesizerError::NaluWriter(x) => write!(f, "{}", x.to_string()),
		}
	}
}

impl From<NaluWriterError> for SynthesizerError {
	fn from(err: NaluWriterError) -> Self {
		SynthesizerError::NaluWriter(err)
	}
}

pub type SynthesizerResult<T> = Result<T, SynthesizerError>;

/// A helper to output typed NALUs to [`std::io::Write`] using [`NaluWriter`].
pub struct Synthesizer<'n, N: private::NaluStruct, W: Write> {
	writer: NaluWriter<W>,
	nalu: &'n N,
}

/// Extended Sample Aspect Ratio - H.264 Table E-1
const EXTENDED_SAR: u8 = 255;

impl<N: private::NaluStruct, W: Write> Synthesizer<'_, N, W> {
	fn u<T: Into<u32>>(&mut self, bits: usize, value: T) -> SynthesizerResult<()> {
		self.writer.write_u(bits, value)?;
		Ok(())
	}

	fn f<T: Into<u32>>(&mut self, bits: usize, value: T) -> SynthesizerResult<()> {
		self.writer.write_f(bits, value)?;
		Ok(())
	}

	fn ue<T: Into<u32>>(&mut self, value: T) -> SynthesizerResult<()> {
		self.writer.write_ue(value)?;
		Ok(())
	}

	fn se<T: Into<i32>>(&mut self, value: T) -> SynthesizerResult<()> {
		self.writer.write_se(value)?;
		Ok(())
	}

	fn scaling_list(&mut self, list: &[u8], default: &[u8]) -> SynthesizerResult<()> {
		// H.264 7.3.2.1.1.1
		if list == default {
			self.se(-8)?;
			return Ok(());
		}

		// The number of list values we want to encode.
		let mut run = list.len();

		// Check how many values at the end of the matrix are the same,
		// so we can save on encoding those.
		for j in (1..list.len()).rev() {
			if list[j - 1] != list[j] {
				break;
			}
			run -= 1;
		}

		// Encode deltas.
		let mut last_scale = 8;
		for scale in &list[0..run] {
			let delta_scale = *scale as i32 - last_scale;
			self.se(delta_scale)?;
			last_scale = *scale as i32;
		}

		// Didn't encode all values, encode -|last_scale| to set decoder's
		// |next_scale| (H.264 7.3.2.1.1.1) to zero, i.e. decoder should repeat
		// last values in matrix.
		if run < list.len() {
			self.se(-last_scale)?;
		}

		Ok(())
	}

	fn default_scaling_list(i: usize) -> &'static [u8] {
		// H.264 Table 7-2
		match i {
			0 => &DEFAULT_4X4_INTRA[..],
			1 => &DEFAULT_4X4_INTRA[..],
			2 => &DEFAULT_4X4_INTRA[..],
			3 => &DEFAULT_4X4_INTER[..],
			4 => &DEFAULT_4X4_INTER[..],
			5 => &DEFAULT_4X4_INTER[..],
			6 => &DEFAULT_8X8_INTRA[..],
			7 => &DEFAULT_8X8_INTER[..],
			8 => &DEFAULT_8X8_INTRA[..],
			9 => &DEFAULT_8X8_INTER[..],
			10 => &DEFAULT_8X8_INTRA[..],
			11 => &DEFAULT_8X8_INTER[..],
			_ => unreachable!(),
		}
	}

	fn rbsp_trailing_bits(&mut self) -> SynthesizerResult<()> {
		self.f(1, 1u32)?;

		while !self.writer.aligned() {
			self.f(1, 0u32)?;
		}

		Ok(())
	}
}

impl<'n, W: Write> Synthesizer<'n, Sps, W> {
	pub fn synthesize(ref_idc: u8, sps: &'n Sps, writer: W, ep_enabled: bool) -> SynthesizerResult<()> {
		let mut s = Self {
			writer: NaluWriter::<W>::new(writer, ep_enabled),
			nalu: sps,
		};

		s.writer.write_header(ref_idc, NaluType::Sps as u8)?;
		s.seq_parameter_set_data()?;
		s.rbsp_trailing_bits()
	}

	fn hrd_parameters(&mut self, hrd_params: &HrdParams) -> SynthesizerResult<()> {
		self.ue(hrd_params.cpb_cnt_minus1)?;
		self.u(4, hrd_params.bit_rate_scale)?;
		self.u(4, hrd_params.cpb_size_scale)?;

		for i in 0..=(hrd_params.cpb_cnt_minus1 as usize) {
			self.ue(hrd_params.bit_rate_value_minus1[i])?;
			self.ue(hrd_params.cpb_size_value_minus1[i])?;
			self.u(1, hrd_params.cbr_flag[i])?;
		}

		self.u(5, hrd_params.initial_cpb_removal_delay_length_minus1)?;
		self.u(5, hrd_params.cpb_removal_delay_length_minus1)?;
		self.u(5, hrd_params.dpb_output_delay_length_minus1)?;
		self.u(5, hrd_params.time_offset_length)?;

		Ok(())
	}

	fn vui_parameters(&mut self) -> SynthesizerResult<()> {
		// H.264 E.1.1
		let vui_params = &self.nalu.vui_parameters;

		self.u(1, vui_params.aspect_ratio_info_present_flag)?;
		if vui_params.aspect_ratio_info_present_flag {
			self.u(8, vui_params.aspect_ratio_idc)?;
			if vui_params.aspect_ratio_idc == EXTENDED_SAR {
				self.u(16, vui_params.sar_width)?;
				self.u(16, vui_params.sar_height)?;
			}
		}

		self.u(1, vui_params.overscan_info_present_flag)?;
		if vui_params.overscan_info_present_flag {
			self.u(1, vui_params.overscan_appropriate_flag)?;
		}

		self.u(1, vui_params.video_signal_type_present_flag)?;
		if vui_params.video_signal_type_present_flag {
			self.u(3, vui_params.video_format)?;
			self.u(1, vui_params.video_full_range_flag)?;

			self.u(1, vui_params.colour_description_present_flag)?;
			if vui_params.colour_description_present_flag {
				self.u(8, vui_params.colour_primaries)?;
				self.u(8, vui_params.transfer_characteristics)?;
				self.u(8, vui_params.matrix_coefficients)?;
			}
		}

		self.u(1, vui_params.chroma_loc_info_present_flag)?;
		if vui_params.chroma_loc_info_present_flag {
			self.ue(vui_params.chroma_sample_loc_type_top_field)?;
			self.ue(self.nalu.vui_parameters.chroma_sample_loc_type_bottom_field)?;
		}

		self.u(1, vui_params.timing_info_present_flag)?;
		if vui_params.timing_info_present_flag {
			self.u(32, vui_params.num_units_in_tick)?;
			self.u(32, vui_params.time_scale)?;
			self.u(1, vui_params.fixed_frame_rate_flag)?;
		}

		self.u(1, vui_params.nal_hrd_parameters_present_flag)?;
		if vui_params.nal_hrd_parameters_present_flag {
			self.hrd_parameters(&vui_params.nal_hrd_parameters)?;
		}
		self.u(1, vui_params.vcl_hrd_parameters_present_flag)?;
		if vui_params.vcl_hrd_parameters_present_flag {
			self.hrd_parameters(&vui_params.vcl_hrd_parameters)?;
		}

		if vui_params.nal_hrd_parameters_present_flag || vui_params.vcl_hrd_parameters_present_flag {
			self.u(1, vui_params.low_delay_hrd_flag)?;
		}

		self.u(1, vui_params.pic_struct_present_flag)?;

		self.u(1, vui_params.bitstream_restriction_flag)?;
		if vui_params.bitstream_restriction_flag {
			self.u(1, vui_params.motion_vectors_over_pic_boundaries_flag)?;
			self.ue(vui_params.max_bytes_per_pic_denom)?;
			self.ue(vui_params.max_bits_per_mb_denom)?;
			self.ue(vui_params.log2_max_mv_length_horizontal)?;
			self.ue(vui_params.log2_max_mv_length_vertical)?;
			self.ue(vui_params.max_num_reorder_frames)?;
			self.ue(vui_params.max_dec_frame_buffering)?;
		}

		Ok(())
	}

	fn seq_parameter_set_data(&mut self) -> SynthesizerResult<()> {
		// H.264 7.3.2.1.1
		self.u(8, self.nalu.profile_idc)?;
		self.u(1, self.nalu.constraint_set0_flag)?;
		self.u(1, self.nalu.constraint_set1_flag)?;
		self.u(1, self.nalu.constraint_set2_flag)?;
		self.u(1, self.nalu.constraint_set3_flag)?;
		self.u(1, self.nalu.constraint_set4_flag)?;
		self.u(1, self.nalu.constraint_set5_flag)?;
		self.u(2, /* reserved_zero_2bits */ 0u32)?;
		self.u(8, self.nalu.level_idc as u32)?;
		self.ue(self.nalu.seq_parameter_set_id)?;

		if self.nalu.profile_idc == 100
			|| self.nalu.profile_idc == 110
			|| self.nalu.profile_idc == 122
			|| self.nalu.profile_idc == 244
			|| self.nalu.profile_idc == 44
			|| self.nalu.profile_idc == 83
			|| self.nalu.profile_idc == 86
			|| self.nalu.profile_idc == 118
			|| self.nalu.profile_idc == 128
			|| self.nalu.profile_idc == 138
			|| self.nalu.profile_idc == 139
			|| self.nalu.profile_idc == 134
			|| self.nalu.profile_idc == 135
		{
			self.ue(self.nalu.chroma_format_idc)?;

			if self.nalu.chroma_format_idc == 3 {
				self.u(1, self.nalu.separate_colour_plane_flag)?;
			}

			self.ue(self.nalu.bit_depth_luma_minus8)?;
			self.ue(self.nalu.bit_depth_chroma_minus8)?;
			self.u(1, self.nalu.qpprime_y_zero_transform_bypass_flag)?;
			self.u(1, self.nalu.seq_scaling_matrix_present_flag)?;

			if self.nalu.seq_scaling_matrix_present_flag {
				let scaling_list_count = if self.nalu.chroma_format_idc != 3 { 8 } else { 12 };

				for i in 0..scaling_list_count {
					// Assume if scaling lists are zeroed that they are not present.
					if i < 6 {
						if self.nalu.scaling_lists_4x4[i] == [0; 16] {
							self.u(1, /* seq_scaling_list_present_flag */ false)?;
						} else {
							self.u(1, /* seq_scaling_list_present_flag */ true)?;
							self.scaling_list(&self.nalu.scaling_lists_4x4[i], Self::default_scaling_list(i))?;
						}
					} else if self.nalu.scaling_lists_8x8[i - 6] == [0; 64] {
						self.u(1, /* seq_scaling_list_present_flag */ false)?;
					} else {
						self.u(1, /* seq_scaling_list_present_flag */ true)?;
						self.scaling_list(&self.nalu.scaling_lists_8x8[i - 6], Self::default_scaling_list(i))?;
					}
				}
			}
		}

		self.ue(self.nalu.log2_max_frame_num_minus4)?;
		self.ue(self.nalu.pic_order_cnt_type)?;

		if self.nalu.pic_order_cnt_type == 0 {
			self.ue(self.nalu.log2_max_pic_order_cnt_lsb_minus4)?;
		} else if self.nalu.pic_order_cnt_type == 1 {
			self.u(1, self.nalu.delta_pic_order_always_zero_flag)?;
			self.se(self.nalu.offset_for_non_ref_pic)?;
			self.se(self.nalu.offset_for_top_to_bottom_field)?;
			self.ue(self.nalu.num_ref_frames_in_pic_order_cnt_cycle)?;

			for offset_for_ref_frame in &self.nalu.offset_for_ref_frame {
				self.se(*offset_for_ref_frame)?;
			}
		}

		self.ue(self.nalu.max_num_ref_frames)?;
		self.u(1, self.nalu.gaps_in_frame_num_value_allowed_flag)?;
		self.ue(self.nalu.pic_width_in_mbs_minus1)?;
		self.ue(self.nalu.pic_height_in_map_units_minus1)?;
		self.u(1, self.nalu.frame_mbs_only_flag)?;
		if !self.nalu.frame_mbs_only_flag {
			self.u(1, self.nalu.mb_adaptive_frame_field_flag)?;
		}
		self.u(1, self.nalu.direct_8x8_inference_flag)?;

		self.u(1, self.nalu.frame_cropping_flag)?;
		if self.nalu.frame_cropping_flag {
			self.ue(self.nalu.frame_crop_left_offset)?;
			self.ue(self.nalu.frame_crop_right_offset)?;
			self.ue(self.nalu.frame_crop_top_offset)?;
			self.ue(self.nalu.frame_crop_bottom_offset)?;
		}

		self.u(1, self.nalu.vui_parameters_present_flag)?;
		if self.nalu.vui_parameters_present_flag {
			self.vui_parameters()?;
		}

		Ok(())
	}
}

impl<'n, W: Write> Synthesizer<'n, Pps, W> {
	pub fn synthesize(ref_idc: u8, pps: &'n Pps, writer: W, ep_enabled: bool) -> SynthesizerResult<()> {
		let mut s = Self {
			writer: NaluWriter::<W>::new(writer, ep_enabled),
			nalu: pps,
		};

		s.writer.write_header(ref_idc, NaluType::Pps as u8)?;
		s.pic_parameter_set_rbsp()?;
		s.rbsp_trailing_bits()
	}

	fn pic_parameter_set_rbsp(&mut self) -> SynthesizerResult<()> {
		self.ue(self.nalu.pic_parameter_set_id)?;
		self.ue(self.nalu.seq_parameter_set_id)?;
		self.u(1, self.nalu.entropy_coding_mode_flag)?;
		self.u(1, self.nalu.bottom_field_pic_order_in_frame_present_flag)?;

		self.ue(self.nalu.num_slice_groups_minus1)?;
		if self.nalu.num_slice_groups_minus1 > 0 {
			return Err(SynthesizerError::Unsupported);
		}

		self.ue(self.nalu.num_ref_idx_l0_default_active_minus1)?;
		self.ue(self.nalu.num_ref_idx_l1_default_active_minus1)?;
		self.u(1, self.nalu.weighted_pred_flag)?;
		self.u(2, self.nalu.weighted_bipred_idc)?;
		self.se(self.nalu.pic_init_qp_minus26)?;
		self.se(self.nalu.pic_init_qs_minus26)?;
		self.se(self.nalu.chroma_qp_index_offset)?;
		self.u(1, self.nalu.deblocking_filter_control_present_flag)?;
		self.u(1, self.nalu.constrained_intra_pred_flag)?;
		self.u(1, self.nalu.redundant_pic_cnt_present_flag)?;

		if !(self.nalu.transform_8x8_mode_flag
			|| self.nalu.pic_scaling_matrix_present_flag
			|| self.nalu.second_chroma_qp_index_offset != 0)
		{
			return Ok(());
		}

		self.u(1, self.nalu.transform_8x8_mode_flag)?;
		self.u(1, self.nalu.pic_scaling_matrix_present_flag)?;

		if self.nalu.pic_scaling_matrix_present_flag {
			let mut scaling_list_count = 6;
			if self.nalu.transform_8x8_mode_flag {
				if self.nalu.sps.chroma_format_idc != 3 {
					scaling_list_count += 2;
				} else {
					scaling_list_count += 6;
				}
			}

			for i in 0..scaling_list_count {
				// Assume if scaling lists are zeroed that they are not present.
				if i < 6 {
					if self.nalu.scaling_lists_4x4[i] == [0; 16] {
						self.u(1, /* seq_scaling_list_present_flag */ false)?;
					} else {
						self.u(1, /* seq_scaling_list_present_flag */ true)?;
						self.scaling_list(&self.nalu.scaling_lists_4x4[i], Self::default_scaling_list(i))?;
					}
				} else if self.nalu.scaling_lists_8x8[i - 6] == [0; 64] {
					self.u(1, /* seq_scaling_list_present_flag */ false)?;
				} else {
					self.u(1, /* seq_scaling_list_present_flag */ true)?;
					self.scaling_list(&self.nalu.scaling_lists_8x8[i - 6], Self::default_scaling_list(i))?;
				}
			}
		}

		self.se(self.nalu.second_chroma_qp_index_offset)?;

		Ok(())
	}
}

#[cfg(feature = "vaapi")]
pub type NumTrailingBits = u8;

#[cfg(feature = "vaapi")]
impl<'n, W: Write> Synthesizer<'n, SliceHeader, W> {
	pub fn synthesize(
		header: &'n SliceHeader,
		sps: &'n Sps,
		pps: &'n Pps,
		is_idr: bool,
		is_ref: IsReference,
		writer: W,
		ep_enabled: bool,
	) -> SynthesizerResult<NumTrailingBits> {
		let mut s = Self {
			writer: NaluWriter::<W>::new(writer, ep_enabled),
			nalu: header,
		};

		let ref_idc = if is_idr {
			3
		} else if is_ref == IsReference::LongTerm {
			2
		} else if is_ref == IsReference::ShortTerm {
			1
		} else {
			0
		};
		let nalu_type = if is_idr { NaluType::SliceIdr } else { NaluType::Slice };
		s.writer.write_header(ref_idc, nalu_type as u8)?;
		s.slice_header_data(sps, pps, is_idr, is_ref)?;
		let num_trailing_bits = s.writer.flush()?;
		Ok(num_trailing_bits)
	}

	fn slice_header_data(&mut self, sps: &Sps, pps: &Pps, is_idr: bool, is_ref: IsReference) -> SynthesizerResult<()> {
		let hdr = self.nalu;

		self.ue(hdr.first_mb_in_slice)?;
		self.ue(hdr.slice_type as u32)?;
		self.ue(hdr.pic_parameter_set_id)?;

		if sps.separate_colour_plane_flag {
			self.u(2, hdr.colour_plane_id)?;
		}

		let frame_num_bits = sps.log2_max_frame_num_minus4 as usize + 4;
		self.u(frame_num_bits, hdr.frame_num)?;

		if !sps.frame_mbs_only_flag {
			self.u(1, hdr.field_pic_flag as u32)?;
			if hdr.field_pic_flag {
				self.u(1, hdr.bottom_field_flag as u32)?;
			}
		}

		if is_idr {
			self.ue(hdr.idr_pic_id)?;
		}

		if sps.pic_order_cnt_type == 0 {
			let pic_order_cnt_lsb_bits = sps.log2_max_pic_order_cnt_lsb_minus4 as usize + 4;
			self.u(pic_order_cnt_lsb_bits, hdr.pic_order_cnt_lsb)?;
			if pps.bottom_field_pic_order_in_frame_present_flag && !hdr.field_pic_flag {
				self.se(hdr.delta_pic_order_cnt_bottom)?;
			}
		}

		if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero_flag {
			self.se(hdr.delta_pic_order_cnt[0])?;
			if pps.bottom_field_pic_order_in_frame_present_flag && !hdr.field_pic_flag {
				self.se(hdr.delta_pic_order_cnt[1])?;
			}
		}

		if pps.redundant_pic_cnt_present_flag {
			self.ue(hdr.redundant_pic_cnt)?;
		}

		if hdr.slice_type == SliceType::B {
			self.u(1, hdr.direct_spatial_mv_pred_flag as u32)?;
		}

		if hdr.slice_type == SliceType::P || hdr.slice_type == SliceType::Sp || hdr.slice_type == SliceType::B {
			self.u(1, hdr.num_ref_idx_active_override_flag as u32)?;
			if hdr.num_ref_idx_active_override_flag {
				self.ue(hdr.num_ref_idx_l0_active_minus1)?;
				if hdr.slice_type == SliceType::B {
					self.ue(hdr.num_ref_idx_l1_active_minus1)?;
				}
			}
		}

		self.ref_pic_list_modification(hdr)?;

		if (pps.weighted_pred_flag && (hdr.slice_type == SliceType::P || hdr.slice_type == SliceType::Sp))
			|| (pps.weighted_bipred_idc == 1 && hdr.slice_type == SliceType::B)
		{
			self.pred_weight_table(hdr, sps)?;
		}

		if is_ref != IsReference::No {
			self.dec_ref_pic_marking(hdr, is_idr)?;
		}

		if pps.entropy_coding_mode_flag && hdr.slice_type != SliceType::I && hdr.slice_type != SliceType::Si {
			self.ue(hdr.cabac_init_idc)?;
		}

		self.se(hdr.slice_qp_delta)?;

		if hdr.slice_type == SliceType::Sp || hdr.slice_type == SliceType::Si {
			if hdr.slice_type == SliceType::Sp {
				self.u(1, hdr.sp_for_switch_flag as u32)?;
			}
			self.se(hdr.slice_qs_delta)?;
		}

		if pps.deblocking_filter_control_present_flag {
			self.ue(hdr.disable_deblocking_filter_idc)?;
			if hdr.disable_deblocking_filter_idc != 1 {
				self.se(hdr.slice_alpha_c0_offset_div2)?;
				self.se(hdr.slice_beta_offset_div2)?;
			}
		}

		if pps.num_slice_groups_minus1 > 0 {
			// Slice groups are not supported, this should have been caught earlier
			return Err(SynthesizerError::Unsupported);
		}

		Ok(())
	}

	fn ref_pic_list_modification(&mut self, hdr: &SliceHeader) -> SynthesizerResult<()> {
		let slice_type_mod5 = hdr.slice_type as u8 % 5;

		if slice_type_mod5 != 2 && slice_type_mod5 != 4 {
			self.u(1, hdr.ref_pic_list_modification_flag_l0 as u32)?;
			if hdr.ref_pic_list_modification_flag_l0 {
				for modification in &hdr.ref_pic_list_modification_l0 {
					self.ue(modification.modification_of_pic_nums_idc)?;
					if modification.modification_of_pic_nums_idc == 0 || modification.modification_of_pic_nums_idc == 1
					{
						self.ue(modification.abs_diff_pic_num_minus1)?;
					} else if modification.modification_of_pic_nums_idc == 2 {
						self.ue(modification.long_term_pic_num)?;
					}
				}
				self.ue(3u32)?;
			}
		}

		if slice_type_mod5 == 1 {
			self.u(1, hdr.ref_pic_list_modification_flag_l1 as u32)?;
			if hdr.ref_pic_list_modification_flag_l1 {
				for modification in &hdr.ref_pic_list_modification_l1 {
					self.ue(modification.modification_of_pic_nums_idc)?;
					if modification.modification_of_pic_nums_idc == 0 || modification.modification_of_pic_nums_idc == 1
					{
						self.ue(modification.abs_diff_pic_num_minus1)?;
					} else if modification.modification_of_pic_nums_idc == 2 {
						self.ue(modification.long_term_pic_num)?;
					}
				}
				self.ue(3u32)?;
			}
		}

		Ok(())
	}

	fn pred_weight_table(&mut self, hdr: &SliceHeader, sps: &Sps) -> SynthesizerResult<()> {
		let pwt = &hdr.pred_weight_table;
		let chroma_array_type = sps.chroma_array_type();

		self.ue(pwt.luma_log2_weight_denom)?;
		if chroma_array_type != 0 {
			self.ue(pwt.chroma_log2_weight_denom)?;
		}

		let num_ref_idx_l0 = (hdr.num_ref_idx_l0_active_minus1 + 1) as usize;
		for i in 0..num_ref_idx_l0 {
			let luma_weight_l0_flag = pwt.luma_weight_l0[i] != (1 << pwt.luma_log2_weight_denom);
			self.u(1, luma_weight_l0_flag as u32)?;
			if luma_weight_l0_flag {
				self.se(pwt.luma_weight_l0[i])?;
				self.se(pwt.luma_offset_l0[i])?;
			}

			if chroma_array_type != 0 {
				let default_weight = 1 << pwt.chroma_log2_weight_denom;
				let chroma_weight_l0_flag = pwt.chroma_weight_l0[i][0] != default_weight
					|| pwt.chroma_weight_l0[i][1] != default_weight
					|| pwt.chroma_offset_l0[i][0] != 0
					|| pwt.chroma_offset_l0[i][1] != 0;
				self.u(1, chroma_weight_l0_flag as u32)?;
				if chroma_weight_l0_flag {
					for j in 0..2 {
						self.se(pwt.chroma_weight_l0[i][j])?;
						self.se(pwt.chroma_offset_l0[i][j])?;
					}
				}
			}
		}

		if hdr.slice_type as u8 % 5 == 1 {
			let num_ref_idx_l1 = (hdr.num_ref_idx_l1_active_minus1 + 1) as usize;
			for i in 0..num_ref_idx_l1 {
				let luma_weight_l1_flag = pwt.luma_weight_l1[i] != (1 << pwt.luma_log2_weight_denom) as i16;
				self.u(1, luma_weight_l1_flag as u32)?;
				if luma_weight_l1_flag {
					self.se(pwt.luma_weight_l1[i])?;
					self.se(pwt.luma_offset_l1[i])?;
				}

				if chroma_array_type != 0 {
					let default_weight = (1 << pwt.chroma_log2_weight_denom) as i16;
					let chroma_weight_l1_flag = pwt.chroma_weight_l1[i][0] != default_weight
						|| pwt.chroma_weight_l1[i][1] != default_weight
						|| pwt.chroma_offset_l1[i][0] != 0
						|| pwt.chroma_offset_l1[i][1] != 0;
					self.u(1, chroma_weight_l1_flag as u32)?;
					if chroma_weight_l1_flag {
						for j in 0..2 {
							self.se(pwt.chroma_weight_l1[i][j])?;
							self.se(pwt.chroma_offset_l1[i][j])?;
						}
					}
				}
			}
		}

		Ok(())
	}

	fn dec_ref_pic_marking(&mut self, hdr: &SliceHeader, is_idr: bool) -> SynthesizerResult<()> {
		let rpm = &hdr.dec_ref_pic_marking;

		if is_idr {
			self.u(1, rpm.no_output_of_prior_pics_flag as u32)?;
			self.u(1, rpm.long_term_reference_flag as u32)?;
		} else {
			self.u(1, rpm.adaptive_ref_pic_marking_mode_flag as u32)?;
			if rpm.adaptive_ref_pic_marking_mode_flag {
				for marking in &rpm.inner {
					self.ue(marking.memory_management_control_operation)?;
					if marking.memory_management_control_operation == 1
						|| marking.memory_management_control_operation == 3
					{
						self.ue(marking.difference_of_pic_nums_minus1)?;
					}
					if marking.memory_management_control_operation == 2 {
						self.ue(marking.long_term_pic_num)?;
					}
					if marking.memory_management_control_operation == 3
						|| marking.memory_management_control_operation == 6
					{
						self.ue(marking.long_term_frame_idx)?;
					}
					if marking.memory_management_control_operation == 4 {
						self.ue(marking.max_long_term_frame_idx.to_value_plus1())?;
					}
				}
				self.ue(0u32)?;
			}
		}

		Ok(())
	}
}

// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around VP9 `VABuffer` types.

use crate::bindings;

/// Wrapper over the `pic_fields` bindgen field in `VAPictureParameterBufferVP9`.
pub struct VP9PicFields(bindings::_VADecPictureParameterBufferVP9__bindgen_ty_1);

impl VP9PicFields {
	/// Creates the bindgen field
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		subsampling_x: u32,
		subsampling_y: u32,
		frame_type: u32,
		show_frame: u32,
		error_resilient_mode: u32,
		intra_only: u32,
		allow_high_precision_mv: u32,
		mcomp_filter_type: u32,
		frame_parallel_decoding_mode: u32,
		reset_frame_context: u32,
		refresh_frame_context: u32,
		frame_context_idx: u32,
		segmentation_enabled: u32,
		segmentation_temporal_update: u32,
		segmentation_update_map: u32,
		last_ref_frame: u32,
		last_ref_frame_sign_bias: u32,
		golden_ref_frame: u32,
		golden_ref_frame_sign_bias: u32,
		alt_ref_frame: u32,
		alt_ref_frame_sign_bias: u32,
		lossless_flag: u32,
	) -> Self {
		let _bitfield_1 = bindings::_VADecPictureParameterBufferVP9__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
			subsampling_x,
			subsampling_y,
			frame_type,
			show_frame,
			error_resilient_mode,
			intra_only,
			allow_high_precision_mv,
			mcomp_filter_type,
			frame_parallel_decoding_mode,
			reset_frame_context,
			refresh_frame_context,
			frame_context_idx,
			segmentation_enabled,
			segmentation_temporal_update,
			segmentation_update_map,
			last_ref_frame,
			last_ref_frame_sign_bias,
			golden_ref_frame,
			golden_ref_frame_sign_bias,
			alt_ref_frame,
			alt_ref_frame_sign_bias,
			lossless_flag,
		);

		Self(bindings::_VADecPictureParameterBufferVP9__bindgen_ty_1 {
			bits: bindings::_VADecPictureParameterBufferVP9__bindgen_ty_1__bindgen_ty_1 {
				_bitfield_align_1: Default::default(),
				_bitfield_1,
			},
		})
	}

	/// Returns the inner FFI type. Useful for testing purposes.
	pub fn inner(&mut self) -> &bindings::_VADecPictureParameterBufferVP9__bindgen_ty_1 {
		&self.0
	}
}

/// Wrapper over the `PictureParameterBufferVP9` FFI type.
pub struct PictureParameterBufferVP9(Box<bindings::VADecPictureParameterBufferVP9>);

impl PictureParameterBufferVP9 {
	/// Creates the wrapper
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		frame_width: u16,
		frame_height: u16,
		reference_frames: [bindings::VASurfaceID; 8],
		pic_fields: &VP9PicFields,
		filter_level: u8,
		sharpness_level: u8,
		log2_tile_rows: u8,
		log2_tile_columns: u8,
		frame_header_length_in_bytes: u8,
		first_partition_size: u16,
		mb_segment_tree_probs: [u8; 7usize],
		segment_pred_probs: [u8; 3usize],
		profile: u8,
		bit_depth: u8,
	) -> Self {
		let pic_fields = pic_fields.0;

		Self(Box::new(bindings::VADecPictureParameterBufferVP9 {
			frame_width,
			frame_height,
			reference_frames,
			pic_fields,
			filter_level,
			sharpness_level,
			log2_tile_rows,
			log2_tile_columns,
			frame_header_length_in_bytes,
			first_partition_size,
			mb_segment_tree_probs,
			segment_pred_probs,
			profile,
			bit_depth,
			va_reserved: Default::default(),
		}))
	}

	pub(crate) fn inner_mut(&mut self) -> &mut bindings::VADecPictureParameterBufferVP9 {
		self.0.as_mut()
	}

	/// Returns the inner FFI type. Useful for testing purposes.
	pub fn inner(&self) -> &bindings::VADecPictureParameterBufferVP9 {
		self.0.as_ref()
	}
}

/// Wrapper over the `segment_flags` bindgen field in `VASegmentParameterVP9`.
pub struct VP9SegmentFlags(bindings::_VASegmentParameterVP9__bindgen_ty_1);

impl VP9SegmentFlags {
	/// Creates the wrapper.
	pub fn new(segment_reference_enabled: u16, segment_reference: u16, segment_reference_skipped: u16) -> Self {
		let _bitfield_1 = bindings::_VASegmentParameterVP9__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
			segment_reference_enabled,
			segment_reference,
			segment_reference_skipped,
		);

		Self(bindings::_VASegmentParameterVP9__bindgen_ty_1 {
			fields: bindings::_VASegmentParameterVP9__bindgen_ty_1__bindgen_ty_1 {
				_bitfield_align_1: Default::default(),
				_bitfield_1,
				__bindgen_padding_0: Default::default(),
			},
		})
	}

	/// Returns the inner FFI type. Useful for testing purposes.
	pub fn inner(&mut self) -> &bindings::_VASegmentParameterVP9__bindgen_ty_1 {
		&self.0
	}
}

/// Wrapper over the `VASegmentParameterVP9` FFI type.
pub struct SegmentParameterVP9(bindings::VASegmentParameterVP9);

impl SegmentParameterVP9 {
	/// Creates the wrapper.
	pub fn new(
		segment_flags: &VP9SegmentFlags,
		filter_level: [[u8; 2usize]; 4usize],
		luma_ac_quant_scale: i16,
		luma_dc_quant_scale: i16,
		chroma_ac_quant_scale: i16,
		chroma_dc_quant_scale: i16,
	) -> Self {
		let segment_flags = segment_flags.0;

		Self(bindings::VASegmentParameterVP9 {
			segment_flags,
			filter_level,
			luma_ac_quant_scale,
			luma_dc_quant_scale,
			chroma_ac_quant_scale,
			chroma_dc_quant_scale,
			va_reserved: Default::default(),
		})
	}
}

/// Wrapper over the `VASliceParameterBufferVP9` FFI type.
pub struct SliceParameterBufferVP9(Box<bindings::VASliceParameterBufferVP9>);

impl SliceParameterBufferVP9 {
	/// Creates the wrapper.
	pub fn new(
		slice_data_size: u32,
		slice_data_offset: u32,
		slice_data_flag: u32,
		seg_param: [SegmentParameterVP9; 8usize],
	) -> Self {
		let seg_param = seg_param.map(|param| param.0);

		Self(Box::new(bindings::VASliceParameterBufferVP9 {
			slice_data_size,
			slice_data_offset,
			slice_data_flag,
			seg_param,
			va_reserved: Default::default(),
		}))
	}

	pub(crate) fn inner_mut(&mut self) -> &mut bindings::VASliceParameterBufferVP9 {
		self.0.as_mut()
	}

	/// Returns the inner FFI type. Useful for testing purposes.
	pub fn inner(&self) -> &bindings::VASliceParameterBufferVP9 {
		self.0.as_ref()
	}
}

pub struct CodedBufferVP9Status(Box<bindings::VACodedBufferVP9Status>);

impl CodedBufferVP9Status {
	pub fn new(
		base_qp_index: u16,
		loop_filter_level: u8,
		long_term_indication: u8,
		next_frame_width: u16,
		next_frame_height: u16,
	) -> Self {
		Self(Box::new(bindings::VACodedBufferVP9Status {
			base_qp_index,
			loop_filter_level,
			long_term_indication,
			next_frame_width,
			next_frame_height,
			va_reserved: Default::default(),
		}))
	}
}

pub struct EncSequenceParameterBufferVP9(Box<bindings::VAEncSequenceParameterBufferVP9>);

impl EncSequenceParameterBufferVP9 {
	pub fn new(
		max_frame_width: u32,
		max_frame_height: u32,
		kf_auto: u32,
		kf_min_dist: u32,
		kf_max_dist: u32,
		bits_per_second: u32,
		intra_period: u32,
	) -> Self {
		Self(Box::new(bindings::VAEncSequenceParameterBufferVP9 {
			max_frame_width,
			max_frame_height,
			kf_auto,
			kf_min_dist,
			kf_max_dist,
			bits_per_second,
			intra_period,
			va_reserved: Default::default(),
		}))
	}

	pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSequenceParameterBufferVP9 {
		&mut self.0
	}
}

pub struct VP9EncRefFlags(bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_1);

impl VP9EncRefFlags {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		force_kf: u32,
		ref_frame_ctrl_l0: u32,
		ref_frame_ctrl_l1: u32,
		ref_last_idx: u32,
		ref_last_sign_bias: u32,
		ref_gf_idx: u32,
		ref_gf_sign_bias: u32,
		ref_arf_idx: u32,
		ref_arf_sign_bias: u32,
		temporal_id: u32,
	) -> Self {
		let _bitfield_1 = bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
			force_kf,
			ref_frame_ctrl_l0,
			ref_frame_ctrl_l1,
			ref_last_idx,
			ref_last_sign_bias,
			ref_gf_idx,
			ref_gf_sign_bias,
			ref_arf_idx,
			ref_arf_sign_bias,
			temporal_id,
			Default::default(),
		);

		Self(bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_1 {
			bits: bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_1__bindgen_ty_1 {
				_bitfield_align_1: Default::default(),
				_bitfield_1,
			},
		})
	}
}

pub struct VP9EncPicFlags(bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_2);

impl VP9EncPicFlags {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		frame_type: u32,
		show_frame: u32,
		error_resilient_mode: u32,
		intra_only: u32,
		allow_high_precision_mv: u32,
		mcomp_filter_type: u32,
		frame_parallel_decoding_mode: u32,
		reset_frame_context: u32,
		refresh_frame_context: u32,
		frame_context_idx: u32,
		segmentation_enabled: u32,
		segmentation_temporal_update: u32,
		segmentation_update_map: u32,
		lossless_mode: u32,
		comp_prediction_mode: u32,
		auto_segmentation: u32,
		super_frame_flag: u32,
	) -> Self {
		let _bitfield_1 = bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
			frame_type,
			show_frame,
			error_resilient_mode,
			intra_only,
			allow_high_precision_mv,
			mcomp_filter_type,
			frame_parallel_decoding_mode,
			reset_frame_context,
			refresh_frame_context,
			frame_context_idx,
			segmentation_enabled,
			segmentation_temporal_update,
			segmentation_update_map,
			lossless_mode,
			comp_prediction_mode,
			auto_segmentation,
			super_frame_flag,
			Default::default(),
		);

		Self(bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_2 {
			bits: bindings::_VAEncPictureParameterBufferVP9__bindgen_ty_2__bindgen_ty_1 {
				_bitfield_align_1: Default::default(),
				_bitfield_1,
			},
		})
	}
}

pub struct EncPictureParameterBufferVP9(Box<bindings::VAEncPictureParameterBufferVP9>);

impl EncPictureParameterBufferVP9 {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		frame_width_src: u32,
		frame_height_src: u32,
		frame_width_dst: u32,
		frame_height_dst: u32,
		reconstructed_frame: bindings::VASurfaceID,
		reference_frames: [bindings::VASurfaceID; 8usize],
		coded_buf: bindings::VABufferID,
		ref_flags: &VP9EncRefFlags,
		pic_flags: &VP9EncPicFlags,
		refresh_frame_flags: u8,
		luma_ac_qindex: u8,
		luma_dc_qindex_delta: i8,
		chroma_ac_qindex_delta: i8,
		chroma_dc_qindex_delta: i8,
		filter_level: u8,
		sharpness_level: u8,
		ref_lf_delta: [i8; 4usize],
		mode_lf_delta: [i8; 2usize],
		bit_offset_ref_lf_delta: u16,
		bit_offset_mode_lf_delta: u16,
		bit_offset_lf_level: u16,
		bit_offset_qindex: u16,
		bit_offset_first_partition_size: u16,
		bit_offset_segmentation: u16,
		bit_size_segmentation: u16,
		log2_tile_rows: u8,
		log2_tile_columns: u8,
		skip_frame_flag: u8,
		number_skip_frames: u8,
		skip_frames_size: u32,
	) -> Self {
		let ref_flags = ref_flags.0;
		let pic_flags = pic_flags.0;

		Self(Box::new(bindings::VAEncPictureParameterBufferVP9 {
			frame_width_src,
			frame_height_src,
			frame_width_dst,
			frame_height_dst,
			reconstructed_frame,
			reference_frames,
			coded_buf,
			ref_flags,
			pic_flags,
			refresh_frame_flags,
			luma_ac_qindex,
			luma_dc_qindex_delta,
			chroma_ac_qindex_delta,
			chroma_dc_qindex_delta,
			filter_level,
			sharpness_level,
			ref_lf_delta,
			mode_lf_delta,
			bit_offset_ref_lf_delta,
			bit_offset_mode_lf_delta,
			bit_offset_lf_level,
			bit_offset_qindex,
			bit_offset_first_partition_size,
			bit_offset_segmentation,
			bit_size_segmentation,
			log2_tile_rows,
			log2_tile_columns,
			skip_frame_flag,
			number_skip_frames,
			skip_frames_size,
			// Zero-fill `va_reserved` plus any fields newer libva headers add
			// (e.g. `seg_id_block_size` / `va_reserved8` in libva 1.23), so this
			// stays buildable across libva versions.
			..Default::default()
		}))
	}

	pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncPictureParameterBufferVP9 {
		&mut self.0
	}
}

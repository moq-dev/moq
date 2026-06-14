// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around H264 `VABuffer` types.

use crate::bindings;

/// Wrapper over the `VAPictureH264` FFI type.
pub struct PictureH264(bindings::VAPictureH264);

impl PictureH264 {
    /// Creates the wrapper
    pub fn new(
        picture_id: bindings::VASurfaceID,
        frame_idx: u32,
        flags: u32,
        top_field_order_cnt: i32,
        bottom_field_order_cnt: i32,
    ) -> Self {
        Self(bindings::VAPictureH264 {
            picture_id,
            frame_idx,
            flags,
            TopFieldOrderCnt: top_field_order_cnt,
            BottomFieldOrderCnt: bottom_field_order_cnt,
            va_reserved: Default::default(),
        })
    }
}

/// Wrapper over the `seq_fields` bindgen field in `VAPictureParameterBufferH264`.
pub struct H264SeqFields(bindings::_VAPictureParameterBufferH264__bindgen_ty_1);

impl H264SeqFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chroma_format_idc: u32,
        residual_colour_transform_flag: u32,
        gaps_in_frame_num_value_allowed_flag: u32,
        frame_mbs_only_flag: u32,
        mb_adaptive_frame_field_flag: u32,
        direct_8x8_inference_flag: u32,
        min_luma_bi_pred_size8x8: u32,
        log2_max_frame_num_minus4: u32,
        pic_order_cnt_type: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
        delta_pic_order_always_zero_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                chroma_format_idc,
                residual_colour_transform_flag,
                gaps_in_frame_num_value_allowed_flag,
                frame_mbs_only_flag,
                mb_adaptive_frame_field_flag,
                direct_8x8_inference_flag,
                min_luma_bi_pred_size8x8,
                log2_max_frame_num_minus4,
                pic_order_cnt_type,
                log2_max_pic_order_cnt_lsb_minus4,
                delta_pic_order_always_zero_flag,
            );

        Self(bindings::_VAPictureParameterBufferH264__bindgen_ty_1 {
            bits: bindings::_VAPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferH264__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `pic_fields` bindgen field in `VAPictureParameterBufferH264`.
pub struct H264PicFields(bindings::_VAPictureParameterBufferH264__bindgen_ty_2);

impl H264PicFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        entropy_coding_mode_flag: u32,
        weighted_pred_flag: u32,
        weighted_bipred_idc: u32,
        transform_8x8_mode_flag: u32,
        field_pic_flag: u32,
        constrained_intra_pred_flag: u32,
        pic_order_present_flag: u32,
        deblocking_filter_control_present_flag: u32,
        redundant_pic_cnt_present_flag: u32,
        reference_pic_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferH264__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                entropy_coding_mode_flag,
                weighted_pred_flag,
                weighted_bipred_idc,
                transform_8x8_mode_flag,
                field_pic_flag,
                constrained_intra_pred_flag,
                pic_order_present_flag,
                deblocking_filter_control_present_flag,
                redundant_pic_cnt_present_flag,
                reference_pic_flag,
            );

        Self(bindings::_VAPictureParameterBufferH264__bindgen_ty_2 {
            bits: bindings::_VAPictureParameterBufferH264__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferH264__bindgen_ty_2 {
        &self.0
    }
}

/// A wrapper over `VAPictureParameterBufferH264` FFI type
pub struct PictureParameterBufferH264(Box<bindings::VAPictureParameterBufferH264>);

impl PictureParameterBufferH264 {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        curr_pic: PictureH264,
        reference_frames: [PictureH264; 16],
        picture_width_in_mbs_minus1: u16,
        picture_height_in_mbs_minus1: u16,
        bit_depth_luma_minus8: u8,
        bit_depth_chroma_minus8: u8,
        num_ref_frames: u8,
        seq_fields: &H264SeqFields,
        num_slice_groups_minus1: u8,
        slice_group_map_type: u8,
        slice_group_change_rate_minus1: u16,
        pic_init_qp_minus26: i8,
        pic_init_qs_minus26: i8,
        chroma_qp_index_offset: i8,
        second_chroma_qp_index_offset: i8,
        pic_fields: &H264PicFields,
        frame_num: u16,
    ) -> Self {
        let reference_frames = (0..16usize)
            .map(|i| reference_frames[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let seq_fields = seq_fields.0;
        let pic_fields = pic_fields.0;

        Self(Box::new(bindings::VAPictureParameterBufferH264 {
            CurrPic: curr_pic.0,
            ReferenceFrames: reference_frames,
            picture_width_in_mbs_minus1,
            picture_height_in_mbs_minus1,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            num_ref_frames,
            seq_fields,
            num_slice_groups_minus1,
            slice_group_map_type,
            slice_group_change_rate_minus1,
            pic_init_qp_minus26,
            pic_init_qs_minus26,
            chroma_qp_index_offset,
            second_chroma_qp_index_offset,
            pic_fields,
            frame_num,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferH264 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAPictureParameterBufferH264 {
        self.0.as_ref()
    }
}

/// Wrapper over the `VASliceParameterBufferH264` FFI type.
pub struct SliceParameterBufferH264(Vec<bindings::VASliceParameterBufferH264>);

impl SliceParameterBufferH264 {
    pub fn new_array() -> Self {
        Self(Vec::new())
    }

    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        slice_data_bit_offset: u16,
        first_mb_in_slice: u16,
        slice_type: u8,
        direct_spatial_mv_pred_flag: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        cabac_init_idc: u8,
        slice_qp_delta: i8,
        disable_deblocking_filter_idc: u8,
        slice_alpha_c0_offset_div2: i8,
        slice_beta_offset_div2: i8,
        ref_pic_list_0: [PictureH264; 32usize],
        ref_pic_list_1: [PictureH264; 32usize],
        luma_log2_weight_denom: u8,
        chroma_log2_weight_denom: u8,
        luma_weight_l0_flag: u8,
        luma_weight_l0: [i16; 32usize],
        luma_offset_l0: [i16; 32usize],
        chroma_weight_l0_flag: u8,
        chroma_weight_l0: [[i16; 2usize]; 32usize],
        chroma_offset_l0: [[i16; 2usize]; 32usize],
        luma_weight_l1_flag: u8,
        luma_weight_l1: [i16; 32usize],
        luma_offset_l1: [i16; 32usize],
        chroma_weight_l1_flag: u8,
        chroma_weight_l1: [[i16; 2usize]; 32usize],
        chroma_offset_l1: [[i16; 2usize]; 32usize],
    ) -> Self {
        let mut array = Self::new_array();

        array.add_slice_parameter(
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            slice_data_bit_offset,
            first_mb_in_slice,
            slice_type,
            direct_spatial_mv_pred_flag,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            cabac_init_idc,
            slice_qp_delta,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            ref_pic_list_0,
            ref_pic_list_1,
            luma_log2_weight_denom,
            chroma_log2_weight_denom,
            luma_weight_l0_flag,
            luma_weight_l0,
            luma_offset_l0,
            chroma_weight_l0_flag,
            chroma_weight_l0,
            chroma_offset_l0,
            luma_weight_l1_flag,
            luma_weight_l1,
            luma_offset_l1,
            chroma_weight_l1_flag,
            chroma_weight_l1,
            chroma_offset_l1,
        );

        array
    }

    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn add_slice_parameter(
        &mut self,
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        slice_data_bit_offset: u16,
        first_mb_in_slice: u16,
        slice_type: u8,
        direct_spatial_mv_pred_flag: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        cabac_init_idc: u8,
        slice_qp_delta: i8,
        disable_deblocking_filter_idc: u8,
        slice_alpha_c0_offset_div2: i8,
        slice_beta_offset_div2: i8,
        ref_pic_list_0: [PictureH264; 32usize],
        ref_pic_list_1: [PictureH264; 32usize],
        luma_log2_weight_denom: u8,
        chroma_log2_weight_denom: u8,
        luma_weight_l0_flag: u8,
        luma_weight_l0: [i16; 32usize],
        luma_offset_l0: [i16; 32usize],
        chroma_weight_l0_flag: u8,
        chroma_weight_l0: [[i16; 2usize]; 32usize],
        chroma_offset_l0: [[i16; 2usize]; 32usize],
        luma_weight_l1_flag: u8,
        luma_weight_l1: [i16; 32usize],
        luma_offset_l1: [i16; 32usize],
        chroma_weight_l1_flag: u8,
        chroma_weight_l1: [[i16; 2usize]; 32usize],
        chroma_offset_l1: [[i16; 2usize]; 32usize],
    ) {
        let ref_pic_list_0 = ref_pic_list_0.map(|pic| pic.0);
        let ref_pic_list_1 = ref_pic_list_1.map(|pic| pic.0);

        let buf = bindings::VASliceParameterBufferH264 {
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            slice_data_bit_offset,
            first_mb_in_slice,
            slice_type,
            direct_spatial_mv_pred_flag,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            cabac_init_idc,
            slice_qp_delta,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            RefPicList0: ref_pic_list_0,
            RefPicList1: ref_pic_list_1,
            luma_log2_weight_denom,
            chroma_log2_weight_denom,
            luma_weight_l0_flag,
            luma_weight_l0,
            luma_offset_l0,
            chroma_weight_l0_flag,
            chroma_weight_l0,
            chroma_offset_l0,
            luma_weight_l1_flag,
            luma_weight_l1,
            luma_offset_l1,
            chroma_weight_l1_flag,
            chroma_weight_l1,
            chroma_offset_l1,
            va_reserved: Default::default(),
        };

        self.0.push(buf);
    }

    pub(crate) fn inner_mut(&mut self) -> &mut Vec<bindings::VASliceParameterBufferH264> {
        &mut self.0
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &Vec<bindings::VASliceParameterBufferH264> {
        &self.0
    }
}

/// Wrapper over the `VAIQMatrixBufferH264` FFI type
pub struct IQMatrixBufferH264(Box<bindings::VAIQMatrixBufferH264>);

impl IQMatrixBufferH264 {
    /// Creates the wrapper.
    pub fn new(
        scaling_list4x4: [[u8; 16usize]; 6usize],
        scaling_list8x8: [[u8; 64usize]; 2usize],
    ) -> Self {
        Self(Box::new(bindings::VAIQMatrixBufferH264 {
            ScalingList4x4: scaling_list4x4,
            ScalingList8x8: scaling_list8x8,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAIQMatrixBufferH264 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAIQMatrixBufferH264 {
        self.0.as_ref()
    }
}

/// Wrapper over the `seq_fields` bindgen field in `VAEncSequenceParameterBufferH264`
pub struct H264EncSeqFields(bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_1);

impl H264EncSeqFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chroma_format_idc: u32,
        frame_mbs_only_flag: u32,
        mb_adaptive_frame_field_flag: u32,
        seq_scaling_matrix_present_flag: u32,
        direct_8x8_inference_flag: u32,
        log2_max_frame_num_minus4: u32,
        pic_order_cnt_type: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
        delta_pic_order_always_zero_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                chroma_format_idc,
                frame_mbs_only_flag,
                mb_adaptive_frame_field_flag,
                seq_scaling_matrix_present_flag,
                direct_8x8_inference_flag,
                log2_max_frame_num_minus4,
                pic_order_cnt_type,
                log2_max_pic_order_cnt_lsb_minus4,
                delta_pic_order_always_zero_flag,
            );

        Self(bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_1 {
            bits: bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }

    pub fn inner(&self) -> &bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_1 {
        &self.0
    }
}

#[derive(Default)]
pub struct H264VuiFields(bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_2);

impl H264VuiFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        aspect_ratio_info_present_flag: u32,
        timing_info_present_flag: u32,
        bitstream_restriction_flag: u32,
        log2_max_mv_length_horizontal: u32,
        log2_max_mv_length_vertical: u32,
        fixed_frame_rate_flag: u32,
        low_delay_hrd_flag: u32,
        motion_vectors_over_pic_boundaries_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                aspect_ratio_info_present_flag,
                timing_info_present_flag,
                bitstream_restriction_flag,
                log2_max_mv_length_horizontal,
                log2_max_mv_length_vertical,
                fixed_frame_rate_flag,
                low_delay_hrd_flag,
                motion_vectors_over_pic_boundaries_flag,
                Default::default(),
            );

        Self(bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_2 {
            bits: bindings::_VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct H264EncFrameCropOffsets {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl H264EncFrameCropOffsets {
    pub fn new(
        frame_crop_left_offset: u32,
        frame_crop_right_offset: u32,
        frame_crop_top_offset: u32,
        frame_crop_bottom_offset: u32,
    ) -> Self {
        Self {
            left: frame_crop_left_offset,
            right: frame_crop_right_offset,
            top: frame_crop_top_offset,
            bottom: frame_crop_bottom_offset,
        }
    }
}

pub struct EncSequenceParameterBufferH264(Box<bindings::VAEncSequenceParameterBufferH264>);

impl EncSequenceParameterBufferH264 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        seq_parameter_set_id: u8,
        level_idc: u8,
        intra_period: u32,
        intra_idr_period: u32,
        ip_period: u32,
        bits_per_second: u32,
        max_num_ref_frames: u32,
        picture_width_in_mbs: u16,
        picture_height_in_mbs: u16,
        seq_fields: &H264EncSeqFields,
        bit_depth_luma_minus8: u8,
        bit_depth_chroma_minus8: u8,
        num_ref_frames_in_pic_order_cnt_cycle: u8,
        offset_for_non_ref_pic: i32,
        offset_for_top_to_bottom_field: i32,
        offset_for_ref_frame: [i32; 256usize],
        frame_crop: Option<H264EncFrameCropOffsets>,
        vui_fields: Option<H264VuiFields>,
        aspect_ratio_idc: u8,
        sar_width: u32,
        sar_height: u32,
        num_units_in_tick: u32,
        time_scale: u32,
    ) -> Self {
        let seq_fields = seq_fields.0;

        let frame_cropping_flag = if frame_crop.is_some() { 1 } else { 0 };
        let frame_crop = frame_crop.unwrap_or_default();

        let frame_crop_left_offset = frame_crop.left;
        let frame_crop_right_offset = frame_crop.right;
        let frame_crop_top_offset = frame_crop.top;
        let frame_crop_bottom_offset = frame_crop.bottom;

        let vui_parameters_present_flag = if vui_fields.is_some() { 1 } else { 0 };
        let vui_fields = vui_fields.unwrap_or_default().0;

        Self(Box::new(bindings::VAEncSequenceParameterBufferH264 {
            seq_parameter_set_id,
            level_idc,
            intra_period,
            intra_idr_period,
            ip_period,
            bits_per_second,
            max_num_ref_frames,
            picture_width_in_mbs,
            picture_height_in_mbs,
            seq_fields,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            num_ref_frames_in_pic_order_cnt_cycle,
            offset_for_non_ref_pic,
            offset_for_top_to_bottom_field,
            offset_for_ref_frame,
            frame_cropping_flag,
            frame_crop_left_offset,
            frame_crop_right_offset,
            frame_crop_top_offset,
            frame_crop_bottom_offset,
            vui_parameters_present_flag,
            vui_fields,
            aspect_ratio_idc,
            sar_width,
            sar_height,
            num_units_in_tick,
            time_scale,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSequenceParameterBufferH264 {
        self.0.as_mut()
    }
}

pub struct H264EncPicFields(bindings::_VAEncPictureParameterBufferH264__bindgen_ty_1);

impl H264EncPicFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        idr_pic_flag: u32,
        reference_pic_flag: u32,
        entropy_coding_mode_flag: u32,
        weighted_pred_flag: u32,
        weighted_bipred_idc: u32,
        constrained_intra_pred_flag: u32,
        transform_8x8_mode_flag: u32,
        deblocking_filter_control_present_flag: u32,
        redundant_pic_cnt_present_flag: u32,
        pic_order_present_flag: u32,
        pic_scaling_matrix_present_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                idr_pic_flag,
                reference_pic_flag,
                entropy_coding_mode_flag,
                weighted_pred_flag,
                weighted_bipred_idc,
                constrained_intra_pred_flag,
                transform_8x8_mode_flag,
                deblocking_filter_control_present_flag,
                redundant_pic_cnt_present_flag,
                pic_order_present_flag,
                pic_scaling_matrix_present_flag,
            );

        Self(bindings::_VAEncPictureParameterBufferH264__bindgen_ty_1 {
            bits: bindings::_VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }
}

pub struct EncPictureParameterBufferH264(Box<bindings::VAEncPictureParameterBufferH264>);

impl EncPictureParameterBufferH264 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        curr_pic: PictureH264,
        reference_frames: [PictureH264; 16usize],
        coded_buf: bindings::VABufferID,
        pic_parameter_set_id: u8,
        seq_parameter_set_id: u8,
        last_picture: u8,
        frame_num: u16,
        pic_init_qp: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        chroma_qp_index_offset: i8,
        second_chroma_qp_index_offset: i8,
        pic_fields: &H264EncPicFields,
    ) -> Self {
        let reference_frames = (0..16usize)
            .map(|i| reference_frames[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let pic_fields = pic_fields.0;

        Self(Box::new(bindings::_VAEncPictureParameterBufferH264 {
            CurrPic: curr_pic.0,
            ReferenceFrames: reference_frames,
            coded_buf,
            pic_parameter_set_id,
            seq_parameter_set_id,
            last_picture,
            frame_num,
            pic_init_qp,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            chroma_qp_index_offset,
            second_chroma_qp_index_offset,
            pic_fields,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncPictureParameterBufferH264 {
        self.0.as_mut()
    }
}

pub struct EncSliceParameterBufferH264(Box<bindings::VAEncSliceParameterBufferH264>);

impl EncSliceParameterBufferH264 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        macroblock_address: u32,
        num_macroblocks: u32,
        macroblock_info: bindings::VABufferID,
        slice_type: u8,
        pic_parameter_set_id: u8,
        idr_pic_id: u16,
        pic_order_cnt_lsb: u16,
        delta_pic_order_cnt_bottom: i32,
        delta_pic_order_cnt: [i32; 2usize],
        direct_spatial_mv_pred_flag: u8,
        num_ref_idx_active_override_flag: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        ref_pic_list_0: [PictureH264; 32usize],
        ref_pic_list_1: [PictureH264; 32usize],
        luma_log2_weight_denom: u8,
        chroma_log2_weight_denom: u8,
        luma_weight_l0_flag: u8,
        luma_weight_l0: [i16; 32usize],
        luma_offset_l0: [i16; 32usize],
        chroma_weight_l0_flag: u8,
        chroma_weight_l0: [[i16; 2usize]; 32usize],
        chroma_offset_l0: [[i16; 2usize]; 32usize],
        luma_weight_l1_flag: u8,
        luma_weight_l1: [i16; 32usize],
        luma_offset_l1: [i16; 32usize],
        chroma_weight_l1_flag: u8,
        chroma_weight_l1: [[i16; 2usize]; 32usize],
        chroma_offset_l1: [[i16; 2usize]; 32usize],
        cabac_init_idc: u8,
        slice_qp_delta: i8,
        disable_deblocking_filter_idc: u8,
        slice_alpha_c0_offset_div2: i8,
        slice_beta_offset_div2: i8,
    ) -> Self {
        let ref_pic_list_0 = ref_pic_list_0.map(|pic| pic.0);
        let ref_pic_list_1 = ref_pic_list_1.map(|pic| pic.0);

        Self(Box::new(bindings::VAEncSliceParameterBufferH264 {
            macroblock_address,
            num_macroblocks,
            macroblock_info,
            slice_type,
            pic_parameter_set_id,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            direct_spatial_mv_pred_flag,
            num_ref_idx_active_override_flag,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            RefPicList0: ref_pic_list_0,
            RefPicList1: ref_pic_list_1,
            luma_log2_weight_denom,
            chroma_log2_weight_denom,
            luma_weight_l0_flag,
            luma_weight_l0,
            luma_offset_l0,
            chroma_weight_l0_flag,
            chroma_weight_l0,
            chroma_offset_l0,
            luma_weight_l1_flag,
            luma_weight_l1,
            luma_offset_l1,
            chroma_weight_l1_flag,
            chroma_weight_l1,
            chroma_offset_l1,
            cabac_init_idc,
            slice_qp_delta,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSliceParameterBufferH264 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAEncSliceParameterBufferH264 {
        self.0.as_ref()
    }
}

pub struct H264EncMacroblockInfo(bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1);

impl H264EncMacroblockInfo {
    pub fn new_intra(pred_avail_override_flag: u32, pred_avail_flags: u32) -> Self {
        let _bitfield_1 =
            bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(pred_avail_override_flag, pred_avail_flags);

        Self(bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1 {
            intra_fields: bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1__bindgen_ty_1 {
                bits:  bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1__bindgen_ty_1__bindgen_ty_1  {
                    _bitfield_align_1: Default::default(),
                    _bitfield_1,
                    __bindgen_padding_0: Default::default(),
                }
            }
        })
    }

    pub fn new_inter() -> Self {
        Self(bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1 {
            inter_fields: bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1__bindgen_ty_2 {
                bits:  bindings::_VAEncMacroblockParameterBufferH264__bindgen_ty_1__bindgen_ty_2__bindgen_ty_1  {
                    reserved: Default::default()
                }
            }
        })
    }
}

pub struct EncMacroblockParameterBufferH264(Box<bindings::VAEncMacroblockParameterBufferH264>);

impl EncMacroblockParameterBufferH264 {
    pub fn new(qp: u8, info: &H264EncMacroblockInfo) -> Self {
        let info = info.0;

        Self(Box::new(bindings::VAEncMacroblockParameterBufferH264 {
            qp,
            info,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncMacroblockParameterBufferH264 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAEncMacroblockParameterBufferH264 {
        self.0.as_ref()
    }
}

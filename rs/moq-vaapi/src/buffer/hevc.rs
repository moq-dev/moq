// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around HEVC `VABuffer` types.

use crate::bindings;

/// Wrapper over the `VAPictureH264` FFI type.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct PictureHEVC(bindings::VAPictureHEVC);

impl PictureHEVC {
    /// Creates the wrapper
    pub fn new(picture_id: bindings::VASurfaceID, pic_order_cnt: i32, flags: u32) -> Self {
        Self(bindings::VAPictureHEVC {
            picture_id,
            pic_order_cnt,
            flags,
            va_reserved: Default::default(),
        })
    }

    /// Returns the `picture_id` field.
    pub fn picture_id(&self) -> u32 {
        self.0.picture_id
    }

    /// Returns the `pic_order_cnt` field.
    pub fn pic_order_cnt(&self) -> i32 {
        self.0.pic_order_cnt
    }

    /// Returns the `flags` field.
    pub fn flags(&self) -> u32 {
        self.0.flags
    }
}

/// Wrapper over the `pic_fields` bindgen field in `VAPictureParameterBufferHEVC`.
pub struct HevcPicFields(bindings::_VAPictureParameterBufferHEVC__bindgen_ty_1);

impl HevcPicFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chroma_format_idc: u32,
        separate_colour_plane_flag: u32,
        pcm_enabled_flag: u32,
        scaling_list_enabled_flag: u32,
        transform_skip_enabled_flag: u32,
        amp_enabled_flag: u32,
        strong_intra_smoothing_enabled_flag: u32,
        sign_data_hiding_enabled_flag: u32,
        constrained_intra_pred_flag: u32,
        cu_qp_delta_enabled_flag: u32,
        weighted_pred_flag: u32,
        weighted_bipred_flag: u32,
        transquant_bypass_enabled_flag: u32,
        tiles_enabled_flag: u32,
        entropy_coding_sync_enabled_flag: u32,
        pps_loop_filter_across_slices_enabled_flag: u32,
        loop_filter_across_tiles_enabled_flag: u32,
        pcm_loop_filter_disabled_flag: u32,
        no_pic_reordering_flag: u32,
        no_bi_pred_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                chroma_format_idc,
                separate_colour_plane_flag,
                pcm_enabled_flag,
                scaling_list_enabled_flag,
                transform_skip_enabled_flag,
                amp_enabled_flag,
                strong_intra_smoothing_enabled_flag,
                sign_data_hiding_enabled_flag,
                constrained_intra_pred_flag,
                cu_qp_delta_enabled_flag,
                weighted_pred_flag,
                weighted_bipred_flag,
                transquant_bypass_enabled_flag,
                tiles_enabled_flag,
                entropy_coding_sync_enabled_flag,
                pps_loop_filter_across_slices_enabled_flag,
                loop_filter_across_tiles_enabled_flag,
                pcm_loop_filter_disabled_flag,
                no_pic_reordering_flag,
                no_bi_pred_flag,
                0,
            );

        Self(bindings::_VAPictureParameterBufferHEVC__bindgen_ty_1 {
            bits: bindings::_VAPictureParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferHEVC__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `slice_parsing_fields` bindgen field in `VAPictureParameterBufferHEVC`.
pub struct HevcSliceParsingFields(bindings::_VAPictureParameterBufferHEVC__bindgen_ty_2);

impl HevcSliceParsingFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        lists_modification_present_flag: u32,
        long_term_ref_pics_present_flag: u32,
        sps_temporal_mvp_enabled_flag: u32,
        cabac_init_present_flag: u32,
        output_flag_present_flag: u32,
        dependent_slice_segments_enabled_flag: u32,
        pps_slice_chroma_qp_offsets_present_flag: u32,
        sample_adaptive_offset_enabled_flag: u32,
        deblocking_filter_override_enabled_flag: u32,
        pps_disable_deblocking_filter_flag: u32,
        slice_segment_header_extension_present_flag: u32,
        rap_pic_flag: u32,
        idr_pic_flag: u32,
        intra_pic_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                lists_modification_present_flag,
                long_term_ref_pics_present_flag,
                sps_temporal_mvp_enabled_flag,
                cabac_init_present_flag,
                output_flag_present_flag,
                dependent_slice_segments_enabled_flag,
                pps_slice_chroma_qp_offsets_present_flag,
                sample_adaptive_offset_enabled_flag,
                deblocking_filter_override_enabled_flag,
                pps_disable_deblocking_filter_flag,
                slice_segment_header_extension_present_flag,
                rap_pic_flag,
                idr_pic_flag,
                intra_pic_flag,
                0,
            );

        Self(bindings::_VAPictureParameterBufferHEVC__bindgen_ty_2 {
            bits: bindings::_VAPictureParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferHEVC__bindgen_ty_2 {
        &self.0
    }
}

/// A wrapper over `VAPictureParameterBufferHEVC` FFI type
pub struct PictureParameterBufferHEVC(Box<bindings::VAPictureParameterBufferHEVC>);

impl PictureParameterBufferHEVC {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        curr_pic: PictureHEVC,
        reference_frames: [PictureHEVC; 15],
        pic_width_in_luma_samples: u16,
        pic_height_in_luma_samples: u16,
        pic_fields: &HevcPicFields,
        sps_max_dec_pic_buffering_minus1: u8,
        bit_depth_luma_minus8: u8,
        bit_depth_chroma_minus8: u8,
        pcm_sample_bit_depth_luma_minus1: u8,
        pcm_sample_bit_depth_chroma_minus1: u8,
        log2_min_luma_coding_block_size_minus3: u8,
        log2_diff_max_min_luma_coding_block_size: u8,
        log2_min_transform_block_size_minus2: u8,
        log2_diff_max_min_transform_block_size: u8,
        log2_min_pcm_luma_coding_block_size_minus3: u8,
        log2_diff_max_min_pcm_luma_coding_block_size: u8,
        max_transform_hierarchy_depth_intra: u8,
        max_transform_hierarchy_depth_inter: u8,
        init_qp_minus26: i8,
        diff_cu_qp_delta_depth: u8,
        pps_cb_qp_offset: i8,
        pps_cr_qp_offset: i8,
        log2_parallel_merge_level_minus2: u8,
        num_tile_columns_minus1: u8,
        num_tile_rows_minus1: u8,
        column_width_minus1: [u16; 19],
        row_height_minus1: [u16; 21],
        slice_parsing_fields: &HevcSliceParsingFields,
        log2_max_pic_order_cnt_lsb_minus4: u8,
        num_short_term_ref_pic_sets: u8,
        num_long_term_ref_pic_sps: u8,
        num_ref_idx_l0_default_active_minus1: u8,
        num_ref_idx_l1_default_active_minus1: u8,
        pps_beta_offset_div2: i8,
        pps_tc_offset_div2: i8,
        num_extra_slice_header_bits: u8,
        st_rps_bits: u32,
    ) -> Self {
        let reference_frames = (0..15usize)
            .map(|i| reference_frames[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let pic_fields = pic_fields.0;
        let slice_parsing_fields = slice_parsing_fields.0;

        Self(Box::new(bindings::VAPictureParameterBufferHEVC {
            CurrPic: curr_pic.0,
            ReferenceFrames: reference_frames,
            pic_width_in_luma_samples,
            pic_height_in_luma_samples,
            pic_fields,
            sps_max_dec_pic_buffering_minus1,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            pcm_sample_bit_depth_luma_minus1,
            pcm_sample_bit_depth_chroma_minus1,
            log2_min_luma_coding_block_size_minus3,
            log2_diff_max_min_luma_coding_block_size,
            log2_min_transform_block_size_minus2,
            log2_diff_max_min_transform_block_size,
            log2_min_pcm_luma_coding_block_size_minus3,
            log2_diff_max_min_pcm_luma_coding_block_size,
            max_transform_hierarchy_depth_intra,
            max_transform_hierarchy_depth_inter,
            init_qp_minus26,
            diff_cu_qp_delta_depth,
            pps_cb_qp_offset,
            pps_cr_qp_offset,
            log2_parallel_merge_level_minus2,
            num_tile_columns_minus1,
            num_tile_rows_minus1,
            column_width_minus1,
            row_height_minus1,
            slice_parsing_fields,
            log2_max_pic_order_cnt_lsb_minus4,
            num_short_term_ref_pic_sets,
            num_long_term_ref_pic_sps,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            pps_beta_offset_div2,
            pps_tc_offset_div2,
            num_extra_slice_header_bits,
            st_rps_bits,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferHEVC {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAPictureParameterBufferHEVC {
        self.0.as_ref()
    }
}

/// Wrapper over the `range_extension_pic_fields` bindgen field in `VAPictureParameterBufferHEVCRext`.
pub struct HevcRangeExtensionPicFields(bindings::_VAPictureParameterBufferHEVCRext__bindgen_ty_1);

impl HevcRangeExtensionPicFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transform_skip_rotation_enabled_flag: u32,
        transform_skip_context_enabled_flag: u32,
        implicit_rdpcm_enabled_flag: u32,
        explicit_rdpcm_enabled_flag: u32,
        extended_precision_processing_flag: u32,
        intra_smoothing_disabled_flag: u32,
        high_precision_offsets_enabled_flag: u32,
        persistent_rice_adaptation_enabled_flag: u32,
        cabac_bypass_alignment_enabled_flag: u32,
        cross_component_prediction_enabled_flag: u32,
        chroma_qp_offset_list_enabled_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferHEVCRext__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                transform_skip_rotation_enabled_flag,
                transform_skip_context_enabled_flag,
                implicit_rdpcm_enabled_flag,
                explicit_rdpcm_enabled_flag,
                extended_precision_processing_flag,
                intra_smoothing_disabled_flag,
                high_precision_offsets_enabled_flag,
                persistent_rice_adaptation_enabled_flag,
                cabac_bypass_alignment_enabled_flag,
                cross_component_prediction_enabled_flag,
                chroma_qp_offset_list_enabled_flag,
                0,
            );

        Self(bindings::_VAPictureParameterBufferHEVCRext__bindgen_ty_1 {
            bits: bindings::_VAPictureParameterBufferHEVCRext__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferHEVCRext__bindgen_ty_1 {
        &self.0
    }
}

/// A wrapper over `VAPictureParameterBufferHEVCRext` FFI type
pub struct PictureParameterBufferHEVCRext(Box<bindings::VAPictureParameterBufferHEVCRext>);

impl PictureParameterBufferHEVCRext {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        range_extension_pic_fields: &HevcRangeExtensionPicFields,
        diff_cu_chroma_qp_offset_depth: u8,
        chroma_qp_offset_list_len_minus1: u8,
        log2_sao_offset_scale_luma: u8,
        log2_sao_offset_scale_chroma: u8,
        log2_max_transform_skip_block_size_minus2: u8,
        cb_qp_offset_list: [i8; 6usize],
        cr_qp_offset_list: [i8; 6usize],
    ) -> Self {
        let range_extension_pic_fields = range_extension_pic_fields.0;

        Self(Box::new(bindings::VAPictureParameterBufferHEVCRext {
            range_extension_pic_fields,
            diff_cu_chroma_qp_offset_depth,
            chroma_qp_offset_list_len_minus1,
            log2_sao_offset_scale_luma,
            log2_sao_offset_scale_chroma,
            log2_max_transform_skip_block_size_minus2,
            cb_qp_offset_list,
            cr_qp_offset_list,
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferHEVCRext {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAPictureParameterBufferHEVCRext {
        self.0.as_ref()
    }
}

/// Wrapper over the `screen_content_pic_fields` bindgen field in `VAPictureParameterBufferHEVCScc`.
pub struct HevcScreenContentPicFields(bindings::_VAPictureParameterBufferHEVCScc__bindgen_ty_1);

impl HevcScreenContentPicFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pps_curr_pic_ref_enabled_flag: u32,
        palette_mode_enabled_flag: u32,
        motion_vector_resolution_control_idc: u32,
        intra_boundary_filtering_disabled_flag: u32,
        residual_adaptive_colour_transform_enabled_flag: u32,
        pps_slice_act_qp_offsets_present_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferHEVCScc__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                pps_curr_pic_ref_enabled_flag,
                palette_mode_enabled_flag,
                motion_vector_resolution_control_idc,
                intra_boundary_filtering_disabled_flag,
                residual_adaptive_colour_transform_enabled_flag,
                pps_slice_act_qp_offsets_present_flag,
                0,
            );

        Self(bindings::_VAPictureParameterBufferHEVCScc__bindgen_ty_1 {
            bits: bindings::_VAPictureParameterBufferHEVCScc__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferHEVCScc__bindgen_ty_1 {
        &self.0
    }
}

/// A wrapper over `VAPictureParameterBufferScc` FFI type
pub struct PictureParameterBufferHEVCScc(Box<bindings::VAPictureParameterBufferHEVCScc>);

impl PictureParameterBufferHEVCScc {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        screen_content_pic_fields: &HevcScreenContentPicFields,
        palette_max_size: u8,
        delta_palette_max_predictor_size: u8,
        predictor_palette_size: u8,
        predictor_palette_entries: [[u16; 128usize]; 3usize],
        pps_act_y_qp_offset_plus5: i8,
        pps_act_cb_qp_offset_plus5: i8,
        pps_act_cr_qp_offset_plus3: i8,
    ) -> Self {
        let screen_content_pic_fields = screen_content_pic_fields.0;

        Self(Box::new(bindings::VAPictureParameterBufferHEVCScc {
            screen_content_pic_fields,
            palette_max_size,
            delta_palette_max_predictor_size,
            predictor_palette_size,
            predictor_palette_entries,
            pps_act_y_qp_offset_plus5,
            pps_act_cb_qp_offset_plus5,
            pps_act_cr_qp_offset_plus3,
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferHEVCScc {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAPictureParameterBufferHEVCScc {
        self.0.as_ref()
    }
}

/// Wrapper over the `long_slice_flags` bindgen field in `VASliceParameterBufferHEVC`.
pub struct HevcLongSliceFlags(bindings::_VASliceParameterBufferHEVC__bindgen_ty_1);

impl HevcLongSliceFlags {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        last_slice_of_pic: u32,
        dependent_slice_segment_flag: u32,
        slice_type: u32,
        color_plane_id: u32,
        slice_sao_luma_flag: u32,
        slice_sao_chroma_flag: u32,
        mvd_l1_zero_flag: u32,
        cabac_init_flag: u32,
        slice_temporal_mvp_enabled_flag: u32,
        slice_deblocking_filter_disabled_flag: u32,
        collocated_from_l0_flag: u32,
        slice_loop_filter_across_slices_enabled_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VASliceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                last_slice_of_pic,
                dependent_slice_segment_flag,
                slice_type,
                color_plane_id,
                slice_sao_luma_flag,
                slice_sao_chroma_flag,
                mvd_l1_zero_flag,
                cabac_init_flag,
                slice_temporal_mvp_enabled_flag,
                slice_deblocking_filter_disabled_flag,
                collocated_from_l0_flag,
                slice_loop_filter_across_slices_enabled_flag,
                0,
            );

        Self(bindings::_VASliceParameterBufferHEVC__bindgen_ty_1 {
            fields: bindings::_VASliceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VASliceParameterBufferHEVC__bindgen_ty_1 {
        &self.0
    }
}

/// A wrapper over `VASliceParameterBufferHEVC` FFI type
pub struct SliceParameterBufferHEVC(Box<bindings::VASliceParameterBufferHEVC>);

impl SliceParameterBufferHEVC {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        slice_data_byte_offset: u32,
        slice_segment_address: u32,
        ref_pic_list: [[u8; 15usize]; 2usize],
        long_slice_flags: &HevcLongSliceFlags,
        collocated_ref_idx: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        slice_qp_delta: i8,
        slice_cb_qp_offset: i8,
        slice_cr_qp_offset: i8,
        slice_beta_offset_div2: i8,
        slice_tc_offset_div2: i8,
        luma_log2_weight_denom: u8,
        delta_chroma_log2_weight_denom: i8,
        delta_luma_weight_l0: [i8; 15usize],
        luma_offset_l0: [i8; 15usize],
        delta_chroma_weight_l0: [[i8; 2usize]; 15usize],
        chroma_offset_l0: [[i8; 2usize]; 15usize],
        delta_luma_weight_l1: [i8; 15usize],
        luma_offset_l1: [i8; 15usize],
        delta_chroma_weight_l1: [[i8; 2usize]; 15usize],
        chroma_offset_l1: [[i8; 2usize]; 15usize],
        five_minus_max_num_merge_cand: u8,
        num_entry_point_offsets: u16,
        entry_offset_to_subset_array: u16,
        slice_data_num_emu_prevn_bytes: u16,
    ) -> Self {
        let long_slice_flags = long_slice_flags.0;

        Self(Box::new(bindings::VASliceParameterBufferHEVC {
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            slice_data_byte_offset,
            slice_segment_address,
            RefPicList: ref_pic_list,
            LongSliceFlags: long_slice_flags,
            collocated_ref_idx,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            slice_qp_delta,
            slice_cb_qp_offset,
            slice_cr_qp_offset,
            slice_beta_offset_div2,
            slice_tc_offset_div2,
            luma_log2_weight_denom,
            delta_chroma_log2_weight_denom,
            delta_luma_weight_l0,
            luma_offset_l0,
            delta_chroma_weight_l0,
            ChromaOffsetL0: chroma_offset_l0,
            delta_luma_weight_l1,
            luma_offset_l1,
            delta_chroma_weight_l1,
            ChromaOffsetL1: chroma_offset_l1,
            five_minus_max_num_merge_cand,
            num_entry_point_offsets,
            entry_offset_to_subset_array,
            slice_data_num_emu_prevn_bytes,
            va_reserved: Default::default(),
        }))
    }

    /// Set this slice as the last one after creation. Implementations may only
    /// be able to conveniently see if this is the last slice after it has been
    /// created.
    pub fn set_as_last(&mut self) {
        // Safe because we know that both fields are valid at all times (just a
        // different view on the data), and we are mutating through the bindgen
        // function, respecting the padding in place.
        unsafe { self.inner_mut().LongSliceFlags.fields.set_LastSliceOfPic(1) };
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VASliceParameterBufferHEVC {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VASliceParameterBufferHEVC {
        self.0.as_ref()
    }
}

/// Wrapper over the `slice_ext_flags` bindgen field in `VASliceParameterBufferHEVCRext`.
pub struct HevcSliceExtFlags(bindings::_VASliceParameterBufferHEVCRext__bindgen_ty_1);

impl HevcSliceExtFlags {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(cu_chroma_qp_offset_enabled_flag: u32, use_integer_mv_flag: u32) -> Self {
        let _bitfield_1 =
            bindings::_VASliceParameterBufferHEVCRext__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                cu_chroma_qp_offset_enabled_flag,
                use_integer_mv_flag,
                0,
            );

        Self(bindings::_VASliceParameterBufferHEVCRext__bindgen_ty_1 {
            bits: bindings::_VASliceParameterBufferHEVCRext__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VASliceParameterBufferHEVCRext__bindgen_ty_1 {
        &self.0
    }
}

/// A wrapper over `VASliceParameterBufferHEVCRext` FFI type
pub struct SliceParameterBufferHEVCRext(Box<bindings::VASliceParameterBufferHEVCRext>);

impl SliceParameterBufferHEVCRext {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        luma_offset_l0: [i16; 15usize],
        chroma_offset_l0: [[i16; 2usize]; 15usize],
        luma_offset_l1: [i16; 15usize],
        chroma_offset_l1: [[i16; 2usize]; 15usize],
        slice_ext_flags: &HevcSliceExtFlags,
        slice_act_y_qp_offset: i8,
        slice_act_cb_qp_offset: i8,
        slice_act_cr_qp_offset: i8,
    ) -> Self {
        let slice_ext_flags = slice_ext_flags.0;

        Self(Box::new(bindings::VASliceParameterBufferHEVCRext {
            luma_offset_l0,
            ChromaOffsetL0: chroma_offset_l0,
            luma_offset_l1,
            ChromaOffsetL1: chroma_offset_l1,
            slice_ext_flags,
            slice_act_y_qp_offset,
            slice_act_cb_qp_offset,
            slice_act_cr_qp_offset,
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VASliceParameterBufferHEVCRext {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VASliceParameterBufferHEVCRext {
        self.0.as_ref()
    }
}

/// A wrapper over `VAIQMatrixBufferHEVC` FFI type
pub struct IQMatrixBufferHEVC(Box<bindings::VAIQMatrixBufferHEVC>);

impl IQMatrixBufferHEVC {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scaling_list4x4: [[u8; 16usize]; 6usize],
        scaling_list8x8: [[u8; 64usize]; 6usize],
        scaling_list16x16: [[u8; 64usize]; 6usize],
        scaling_list32x32: [[u8; 64usize]; 2usize],
        scaling_list_dc16x16: [u8; 6usize],
        scaling_list_dc32x32: [u8; 2usize],
    ) -> Self {
        Self(Box::new(bindings::VAIQMatrixBufferHEVC {
            ScalingList4x4: scaling_list4x4,
            ScalingList8x8: scaling_list8x8,
            ScalingList16x16: scaling_list16x16,
            ScalingList32x32: scaling_list32x32,
            ScalingListDC16x16: scaling_list_dc16x16,
            ScalingListDC32x32: scaling_list_dc32x32,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAIQMatrixBufferHEVC {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAIQMatrixBufferHEVC {
        self.0.as_ref()
    }
}

pub struct HEVCEncSeqFields(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_1);

impl HEVCEncSeqFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chroma_format_idc: u32,
        separate_colour_plane_flag: u32,
        bit_depth_luma_minus8: u32,
        bit_depth_chroma_minus8: u32,
        scaling_list_enabled_flag: u32,
        strong_intra_smoothing_enabled_flag: u32,
        amp_enabled_flag: u32,
        sample_adaptive_offset_enabled_flag: u32,
        pcm_enabled_flag: u32,
        pcm_loop_filter_disabled_flag: u32,
        sps_temporal_mvp_enabled_flag: u32,
        low_delay_seq: u32,
        hierachical_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                chroma_format_idc,
                separate_colour_plane_flag,
                bit_depth_luma_minus8,
                bit_depth_chroma_minus8,
                scaling_list_enabled_flag,
                strong_intra_smoothing_enabled_flag,
                amp_enabled_flag,
                sample_adaptive_offset_enabled_flag,
                pcm_enabled_flag,
                pcm_loop_filter_disabled_flag,
                sps_temporal_mvp_enabled_flag,
                low_delay_seq,
                hierachical_flag,
                Default::default(),
            );

        Self(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_1 {
            bits: bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct HevcEncVuiFields(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_2);

impl HevcEncVuiFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        aspect_ratio_info_present_flag: u32,
        neutral_chroma_indication_flag: u32,
        field_seq_flag: u32,
        vui_timing_info_present_flag: u32,
        bitstream_restriction_flag: u32,
        tiles_fixed_structure_flag: u32,
        motion_vectors_over_pic_boundaries_flag: u32,
        restricted_ref_pic_lists_flag: u32,
        log2_max_mv_length_horizontal: u32,
        log2_max_mv_length_vertical: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                aspect_ratio_info_present_flag,
                neutral_chroma_indication_flag,
                field_seq_flag,
                vui_timing_info_present_flag,
                bitstream_restriction_flag,
                tiles_fixed_structure_flag,
                motion_vectors_over_pic_boundaries_flag,
                restricted_ref_pic_lists_flag,
                log2_max_mv_length_horizontal,
                log2_max_mv_length_vertical,
            );

        Self(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_2 {
            bits: bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }
}

pub struct HevcEncSeqSccFields(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_3);

impl HevcEncSeqSccFields {
    pub fn new(palette_mode_enabled_flag: u32) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_3__bindgen_ty_1::new_bitfield_1(
                palette_mode_enabled_flag,
                Default::default(),
            );

        Self(bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_3 {
            bits: bindings::_VAEncSequenceParameterBufferHEVC__bindgen_ty_3__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

pub struct EncSequenceParameterBufferHEVC(Box<bindings::VAEncSequenceParameterBufferHEVC>);

impl EncSequenceParameterBufferHEVC {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        general_profile_idc: u8,
        general_level_idc: u8,
        general_tier_flag: u8,
        intra_period: u32,
        intra_idr_period: u32,
        ip_period: u32,
        bits_per_second: u32,
        pic_width_in_luma_samples: u16,
        pic_height_in_luma_samples: u16,
        seq_fields: &HEVCEncSeqFields,
        log2_min_luma_coding_block_size_minus3: u8,
        log2_diff_max_min_luma_coding_block_size: u8,
        log2_min_transform_block_size_minus2: u8,
        log2_diff_max_min_transform_block_size: u8,
        max_transform_hierarchy_depth_inter: u8,
        max_transform_hierarchy_depth_intra: u8,
        pcm_sample_bit_depth_luma_minus1: u32,
        pcm_sample_bit_depth_chroma_minus1: u32,
        log2_min_pcm_luma_coding_block_size_minus3: u32,
        log2_max_pcm_luma_coding_block_size_minus3: u32,
        vui_fields: Option<HevcEncVuiFields>,
        aspect_ratio_idc: u8,
        sar_width: u32,
        sar_height: u32,
        vui_num_units_in_tick: u32,
        vui_time_scale: u32,
        min_spatial_segmentation_idc: u16,
        max_bytes_per_pic_denom: u8,
        max_bits_per_min_cu_denom: u8,
        scc_fields: &HevcEncSeqSccFields,
    ) -> Self {
        let seq_fields = seq_fields.0;
        let vui_parameters_present_flag = vui_fields.is_some() as u8;
        let vui_fields = vui_fields.unwrap_or_default().0;
        let scc_fields = scc_fields.0;

        Self(Box::new(bindings::VAEncSequenceParameterBufferHEVC {
            general_profile_idc,
            general_level_idc,
            general_tier_flag,
            intra_period,
            intra_idr_period,
            ip_period,
            bits_per_second,
            pic_width_in_luma_samples,
            pic_height_in_luma_samples,
            seq_fields,
            log2_min_luma_coding_block_size_minus3,
            log2_diff_max_min_luma_coding_block_size,
            log2_min_transform_block_size_minus2,
            log2_diff_max_min_transform_block_size,
            max_transform_hierarchy_depth_inter,
            max_transform_hierarchy_depth_intra,
            pcm_sample_bit_depth_luma_minus1,
            pcm_sample_bit_depth_chroma_minus1,
            log2_min_pcm_luma_coding_block_size_minus3,
            log2_max_pcm_luma_coding_block_size_minus3,
            vui_parameters_present_flag,
            vui_fields,
            aspect_ratio_idc,
            sar_width,
            sar_height,
            vui_num_units_in_tick,
            vui_time_scale,
            min_spatial_segmentation_idc,
            max_bytes_per_pic_denom,
            max_bits_per_min_cu_denom,
            scc_fields,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSequenceParameterBufferHEVC {
        &mut self.0
    }
}

pub struct HEVCEncPicFields(bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_1);

impl HEVCEncPicFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        idr_pic_flag: u32,
        coding_type: u32,
        reference_pic_flag: u32,
        dependent_slice_segments_enabled_flag: u32,
        sign_data_hiding_enabled_flag: u32,
        constrained_intra_pred_flag: u32,
        transform_skip_enabled_flag: u32,
        cu_qp_delta_enabled_flag: u32,
        weighted_pred_flag: u32,
        weighted_bipred_flag: u32,
        transquant_bypass_enabled_flag: u32,
        tiles_enabled_flag: u32,
        entropy_coding_sync_enabled_flag: u32,
        loop_filter_across_tiles_enabled_flag: u32,
        pps_loop_filter_across_slices_enabled_flag: u32,
        scaling_list_data_present_flag: u32,
        screen_content_flag: u32,
        enable_gpu_weighted_prediction: u32,
        no_output_of_prior_pics_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                idr_pic_flag,
                coding_type,
                reference_pic_flag,
                dependent_slice_segments_enabled_flag,
                sign_data_hiding_enabled_flag,
                constrained_intra_pred_flag,
                transform_skip_enabled_flag,
                cu_qp_delta_enabled_flag,
                weighted_pred_flag,
                weighted_bipred_flag,
                transquant_bypass_enabled_flag,
                tiles_enabled_flag,
                entropy_coding_sync_enabled_flag,
                loop_filter_across_tiles_enabled_flag,
                pps_loop_filter_across_slices_enabled_flag,
                scaling_list_data_present_flag,
                screen_content_flag,
                enable_gpu_weighted_prediction,
                no_output_of_prior_pics_flag,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_1 {
            bits: bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

pub struct HevcEncPicSccFields(bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_2);

impl HevcEncPicSccFields {
    pub fn new(pps_curr_pic_ref_enabled_flag: u16) -> Self {
        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                pps_curr_pic_ref_enabled_flag,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_2 {
            bits: bindings::_VAEncPictureParameterBufferHEVC__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

pub struct EncPictureParameterBufferHEVC(Box<bindings::VAEncPictureParameterBufferHEVC>);

impl EncPictureParameterBufferHEVC {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        decoded_curr_pic: PictureHEVC,
        reference_frames: [PictureHEVC; 15usize],
        coded_buf: bindings::VABufferID,
        collocated_ref_pic_index: u8,
        last_picture: u8,
        pic_init_qp: u8,
        diff_cu_qp_delta_depth: u8,
        pps_cb_qp_offset: i8,
        pps_cr_qp_offset: i8,
        num_tile_columns_minus1: u8,
        num_tile_rows_minus1: u8,
        column_width_minus1: [u8; 19usize],
        row_height_minus1: [u8; 21usize],
        log2_parallel_merge_level_minus2: u8,
        ctu_max_bitsize_allowed: u8,
        num_ref_idx_l0_default_active_minus1: u8,
        num_ref_idx_l1_default_active_minus1: u8,
        slice_pic_parameter_set_id: u8,
        nal_unit_type: u8,
        pic_fields: &HEVCEncPicFields,
        hierarchical_level_plus1: u8,
        va_byte_reserved: u8,
        scc_fields: &HevcEncPicSccFields,
    ) -> Self {
        let decoded_curr_pic = decoded_curr_pic.0;
        let reference_frames = (0..15usize)
            .map(|i| reference_frames[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let pic_fields = pic_fields.0;
        let scc_fields = scc_fields.0;

        Self(Box::new(bindings::VAEncPictureParameterBufferHEVC {
            decoded_curr_pic,
            reference_frames,
            coded_buf,
            collocated_ref_pic_index,
            last_picture,
            pic_init_qp,
            diff_cu_qp_delta_depth,
            pps_cb_qp_offset,
            pps_cr_qp_offset,
            num_tile_columns_minus1,
            num_tile_rows_minus1,
            column_width_minus1,
            row_height_minus1,
            log2_parallel_merge_level_minus2,
            ctu_max_bitsize_allowed,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            slice_pic_parameter_set_id,
            nal_unit_type,
            pic_fields,
            hierarchical_level_plus1,
            va_byte_reserved,
            scc_fields,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncPictureParameterBufferHEVC {
        &mut self.0
    }
}

pub struct HevcEncSliceFields(bindings::_VAEncSliceParameterBufferHEVC__bindgen_ty_1);

impl HevcEncSliceFields {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        last_slice_of_pic_flag: u32,
        dependent_slice_segment_flag: u32,
        colour_plane_id: u32,
        slice_temporal_mvp_enabled_flag: u32,
        slice_sao_luma_flag: u32,
        slice_sao_chroma_flag: u32,
        num_ref_idx_active_override_flag: u32,
        mvd_l1_zero_flag: u32,
        cabac_init_flag: u32,
        slice_deblocking_filter_disabled_flag: u32,
        slice_loop_filter_across_slices_enabled_flag: u32,
        collocated_from_l0_flag: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncSliceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                last_slice_of_pic_flag,
                dependent_slice_segment_flag,
                colour_plane_id,
                slice_temporal_mvp_enabled_flag,
                slice_sao_luma_flag,
                slice_sao_chroma_flag,
                num_ref_idx_active_override_flag,
                mvd_l1_zero_flag,
                cabac_init_flag,
                slice_deblocking_filter_disabled_flag,
                slice_loop_filter_across_slices_enabled_flag,
                collocated_from_l0_flag,
            );

        Self(bindings::_VAEncSliceParameterBufferHEVC__bindgen_ty_1 {
            bits: bindings::_VAEncSliceParameterBufferHEVC__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }
}

pub struct EncSliceParameterBufferHEVC(Box<bindings::VAEncSliceParameterBufferHEVC>);

impl EncSliceParameterBufferHEVC {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slice_segment_address: u32,
        num_ctu_in_slice: u32,
        slice_type: u8,
        slice_pic_parameter_set_id: u8,
        num_ref_idx_l0_active_minus1: u8,
        num_ref_idx_l1_active_minus1: u8,
        ref_pic_list0: [PictureHEVC; 15usize],
        ref_pic_list1: [PictureHEVC; 15usize],
        luma_log2_weight_denom: u8,
        delta_chroma_log2_weight_denom: i8,
        delta_luma_weight_l0: [i8; 15usize],
        luma_offset_l0: [i8; 15usize],
        delta_chroma_weight_l0: [[i8; 2usize]; 15usize],
        chroma_offset_l0: [[i8; 2usize]; 15usize],
        delta_luma_weight_l1: [i8; 15usize],
        luma_offset_l1: [i8; 15usize],
        delta_chroma_weight_l1: [[i8; 2usize]; 15usize],
        chroma_offset_l1: [[i8; 2usize]; 15usize],
        max_num_merge_cand: u8,
        slice_qp_delta: i8,
        slice_cb_qp_offset: i8,
        slice_cr_qp_offset: i8,
        slice_beta_offset_div2: i8,
        slice_tc_offset_div2: i8,
        slice_fields: &HevcEncSliceFields,
        pred_weight_table_bit_offset: u32,
        pred_weight_table_bit_length: u32,
    ) -> Self {
        let ref_pic_list0 = (0..15usize)
            .map(|i| ref_pic_list0[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let ref_pic_list1 = (0..15usize)
            .map(|i| ref_pic_list1[i].0)
            .collect::<Vec<_>>()
            .try_into()
            // try_into is guaranteed to work because the iterator and target array have the same
            // size.
            .unwrap();

        let slice_fields = slice_fields.0;

        Self(Box::new(bindings::VAEncSliceParameterBufferHEVC {
            slice_segment_address,
            num_ctu_in_slice,
            slice_type,
            slice_pic_parameter_set_id,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            ref_pic_list0,
            ref_pic_list1,
            luma_log2_weight_denom,
            delta_chroma_log2_weight_denom,
            delta_luma_weight_l0,
            luma_offset_l0,
            delta_chroma_weight_l0,
            chroma_offset_l0,
            delta_luma_weight_l1,
            luma_offset_l1,
            delta_chroma_weight_l1,
            chroma_offset_l1,
            max_num_merge_cand,
            slice_qp_delta,
            slice_cb_qp_offset,
            slice_cr_qp_offset,
            slice_beta_offset_div2,
            slice_tc_offset_div2,
            slice_fields,
            #[cfg(libva_1_10_or_higher)]
            pred_weight_table_bit_offset,
            #[cfg(libva_1_10_or_higher)]
            pred_weight_table_bit_length,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSliceParameterBufferHEVC {
        &mut self.0
    }
}

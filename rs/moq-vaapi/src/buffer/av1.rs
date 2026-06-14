// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around AV1 `VABuffer` types.

use crate::bindings;

/// Wrapper over the `seq_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1SeqFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_1);

impl AV1SeqFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        still_picture: u32,
        use_128x128_superblock: u32,
        enable_filter_intra: u32,
        enable_intra_edge_filter: u32,
        enable_interintra_compound: u32,
        enable_masked_compound: u32,
        enable_dual_filter: u32,
        enable_order_hint: u32,
        enable_jnt_comp: u32,
        enable_cdef: u32,
        mono_chrome: u32,
        color_range: u32,
        subsampling_x: u32,
        subsampling_y: u32,
        chroma_sample_position: u32,
        film_grain_params_present: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                still_picture,
                use_128x128_superblock,
                enable_filter_intra,
                enable_intra_edge_filter,
                enable_interintra_compound,
                enable_masked_compound,
                enable_dual_filter,
                enable_order_hint,
                enable_jnt_comp,
                enable_cdef,
                mono_chrome,
                color_range,
                subsampling_x,
                subsampling_y,
                chroma_sample_position,
                film_grain_params_present,
                0,
            );

        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_1 {
            fields: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `segment_info_fields` bindgen field in `VASegmentationStructAV1`.
pub struct AV1SegmentInfoFields(bindings::_VASegmentationStructAV1__bindgen_ty_1);

impl AV1SegmentInfoFields {
    /// Creates the bindgen field
    pub fn new(enabled: u32, update_map: u32, temporal_update: u32, update_data: u32) -> Self {
        let _bitfield_1 =
            bindings::_VASegmentationStructAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                enabled,
                update_map,
                temporal_update,
                update_data,
                0,
            );

        Self(bindings::_VASegmentationStructAV1__bindgen_ty_1 {
            bits: bindings::_VASegmentationStructAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `seg_info` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1Segmentation(bindings::VASegmentationStructAV1);

impl AV1Segmentation {
    /// Creates the bindgen field
    pub fn new(
        segment_info_fields: &AV1SegmentInfoFields,
        feature_data: [[i16; 8usize]; 8usize],
        feature_mask: [u8; 8usize],
    ) -> Self {
        let segment_info_fields = segment_info_fields.0;
        Self(bindings::VASegmentationStructAV1 {
            segment_info_fields,
            feature_data,
            feature_mask,
            va_reserved: Default::default(),
        })
    }
}

/// Wrapper over the `film_grain_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1FilmGrainFields(bindings::_VAFilmGrainStructAV1__bindgen_ty_1);

impl AV1FilmGrainFields {
    /// Creates the bindgen field
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        apply_grain: u32,
        chroma_scaling_from_luma: u32,
        grain_scaling_minus_8: u32,
        ar_coeff_lag: u32,
        ar_coeff_shift_minus_6: u32,
        grain_scale_shift: u32,
        overlap_flag: u32,
        clip_to_restricted_range: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAFilmGrainStructAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                apply_grain,
                chroma_scaling_from_luma,
                grain_scaling_minus_8,
                ar_coeff_lag,
                ar_coeff_shift_minus_6,
                grain_scale_shift,
                overlap_flag,
                clip_to_restricted_range,
                0,
            );
        Self(bindings::_VAFilmGrainStructAV1__bindgen_ty_1 {
            bits: bindings::_VAFilmGrainStructAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `film_grain_info` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1FilmGrain(bindings::VAFilmGrainStructAV1);

impl AV1FilmGrain {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        film_grain_info_fields: &AV1FilmGrainFields,
        grain_seed: u16,
        num_y_points: u8,
        point_y_value: [u8; 14usize],
        point_y_scaling: [u8; 14usize],
        num_cb_points: u8,
        point_cb_value: [u8; 10usize],
        point_cb_scaling: [u8; 10usize],
        num_cr_points: u8,
        point_cr_value: [u8; 10usize],
        point_cr_scaling: [u8; 10usize],
        ar_coeffs_y: [i8; 24usize],
        ar_coeffs_cb: [i8; 25usize],
        ar_coeffs_cr: [i8; 25usize],
        cb_mult: u8,
        cb_luma_mult: u8,
        cb_offset: u16,
        cr_mult: u8,
        cr_luma_mult: u8,
        cr_offset: u16,
    ) -> Self {
        let film_grain_info_fields = film_grain_info_fields.0;
        Self(bindings::VAFilmGrainStructAV1 {
            film_grain_info_fields,
            grain_seed,
            num_y_points,
            point_y_value,
            point_y_scaling,
            num_cb_points,
            point_cb_value,
            point_cb_scaling,
            num_cr_points,
            point_cr_value,
            point_cr_scaling,
            ar_coeffs_y,
            ar_coeffs_cb,
            ar_coeffs_cr,
            cb_mult,
            cb_luma_mult,
            cb_offset,
            cr_mult,
            cr_luma_mult,
            cr_offset,
            va_reserved: Default::default(),
        })
    }
}

/// Wrapper over the `pic_info_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1PicInfoFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_2);

impl AV1PicInfoFields {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame_type: u32,
        show_frame: u32,
        showable_frame: u32,
        error_resilient_mode: u32,
        disable_cdf_update: u32,
        allow_screen_content_tools: u32,
        force_integer_mv: u32,
        allow_intrabc: u32,
        use_superres: u32,
        allow_high_precision_mv: u32,
        is_motion_mode_switchable: u32,
        use_ref_frame_mvs: u32,
        disable_frame_end_update_cdf: u32,
        uniform_tile_spacing_flag: u32,
        allow_warped_motion: u32,
        large_scale_tile: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                frame_type,
                show_frame,
                showable_frame,
                error_resilient_mode,
                disable_cdf_update,
                allow_screen_content_tools,
                force_integer_mv,
                allow_intrabc,
                use_superres,
                allow_high_precision_mv,
                is_motion_mode_switchable,
                use_ref_frame_mvs,
                disable_frame_end_update_cdf,
                uniform_tile_spacing_flag,
                allow_warped_motion,
                large_scale_tile,
                0,
            );
        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_2 {
            bits: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `loop_filter_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1LoopFilterFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_3);

impl AV1LoopFilterFields {
    /// Creates the wrapper
    pub fn new(sharpness_level: u8, mode_ref_delta_enabled: u8, mode_ref_delta_update: u8) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_3__bindgen_ty_1::new_bitfield_1(
                sharpness_level,
                mode_ref_delta_enabled,
                mode_ref_delta_update,
                0,
            );
        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_3 {
            bits: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_3__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `wm` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1WarpedMotionParams(bindings::VAWarpedMotionParamsAV1);

impl AV1WarpedMotionParams {
    /// Creates the wrapper
    pub fn new(
        wmtype: bindings::VAAV1TransformationType::Type,
        wmmat: [i32; 8],
        invalid: u8,
    ) -> Self {
        Self(bindings::VAWarpedMotionParamsAV1 {
            wmtype,
            wmmat,
            invalid,
            va_reserved: Default::default(),
        })
    }
}

/// Wrapper over the `loop_restoration_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1LoopRestorationFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_6);

impl AV1LoopRestorationFields {
    /// Creates the wrapper
    pub fn new(
        yframe_restoration_type: u16,
        cbframe_restoration_type: u16,
        crframe_restoration_type: u16,
        lr_unit_shift: u16,
        lr_uv_shift: u16,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_6__bindgen_ty_1::new_bitfield_1(
                yframe_restoration_type,
                cbframe_restoration_type,
                crframe_restoration_type,
                lr_unit_shift,
                lr_uv_shift,
                0,
            );
        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_6 {
            bits: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_6__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `mode_control_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1ModeControlFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_5);

impl AV1ModeControlFields {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        delta_q_present_flag: u32,
        log2_delta_q_res: u32,
        delta_lf_present_flag: u32,
        log2_delta_lf_res: u32,
        delta_lf_multi: u32,
        tx_mode: u32,
        reference_select: u32,
        reduced_tx_set_used: u32,
        skip_mode_present: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_5__bindgen_ty_1::new_bitfield_1(
                delta_q_present_flag,
                log2_delta_q_res,
                delta_lf_present_flag,
                log2_delta_lf_res,
                delta_lf_multi,
                tx_mode,
                reference_select,
                reduced_tx_set_used,
                skip_mode_present,
                0,
            );
        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_5 {
            bits: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_5__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// Wrapper over the `qmatrix_fields` bindgen field in `VADecPictureParameterBufferAV1`.
pub struct AV1QMatrixFields(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_4);

impl AV1QMatrixFields {
    /// Creates the wrapper
    pub fn new(using_qmatrix: u16, qm_y: u16, qm_u: u16, qm_v: u16) -> Self {
        let _bitfield_1 =
            bindings::_VADecPictureParameterBufferAV1__bindgen_ty_4__bindgen_ty_1::new_bitfield_1(
                using_qmatrix,
                qm_y,
                qm_u,
                qm_v,
                0,
            );
        Self(bindings::_VADecPictureParameterBufferAV1__bindgen_ty_4 {
            bits: bindings::_VADecPictureParameterBufferAV1__bindgen_ty_4__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

/// A wrapper over `VADecPictureParameterBufferAV1` FFI type
pub struct PictureParameterBufferAV1(Box<bindings::VADecPictureParameterBufferAV1>);

impl PictureParameterBufferAV1 {
    /// Creates the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: u8,
        order_hint_bits_minus_1: u8,
        bit_depth_idx: u8,
        matrix_coefficients: u8,
        seq_info_fields: &AV1SeqFields,
        current_frame: bindings::VASurfaceID,
        current_display_picture: bindings::VASurfaceID,
        mut anchor_frames_list: Vec<bindings::VASurfaceID>,
        frame_width_minus1: u16,
        frame_height_minus1: u16,
        output_frame_width_in_tiles_minus_1: u16,
        output_frame_height_in_tiles_minus_1: u16,
        ref_frame_map: [bindings::VASurfaceID; 8usize],
        ref_frame_idx: [u8; 7usize],
        primary_ref_frame: u8,
        order_hint: u8,
        seg_info: &AV1Segmentation,
        film_grain_info: &AV1FilmGrain,
        tile_cols: u8,
        tile_rows: u8,
        width_in_sbs_minus_1: [u16; 63usize],
        height_in_sbs_minus_1: [u16; 63usize],
        tile_count_minus_1: u16,
        context_update_tile_id: u16,
        pic_info_fields: &AV1PicInfoFields,
        superres_scale_denominator: u8,
        interp_filter: u8,
        filter_level: [u8; 2usize],
        filter_level_u: u8,
        filter_level_v: u8,
        loop_filter_info_fields: &AV1LoopFilterFields,
        ref_deltas: [i8; 8usize],
        mode_deltas: [i8; 2usize],
        base_qindex: u8,
        y_dc_delta_q: i8,
        u_dc_delta_q: i8,
        u_ac_delta_q: i8,
        v_dc_delta_q: i8,
        v_ac_delta_q: i8,
        qmatrix_fields: &AV1QMatrixFields,
        mode_control_fields: &AV1ModeControlFields,
        cdef_damping_minus_3: u8,
        cdef_bits: u8,
        cdef_y_strengths: [u8; 8usize],
        cdef_uv_strengths: [u8; 8usize],
        loop_restoration_fields: &AV1LoopRestorationFields,
        wm: &[AV1WarpedMotionParams; 7usize],
    ) -> Self {
        let seq_info_fields = seq_info_fields.0;
        let seg_info = seg_info.0;
        let pic_info_fields = pic_info_fields.0;
        let loop_filter_info_fields = loop_filter_info_fields.0;
        let qmatrix_fields = qmatrix_fields.0;
        let mode_control_fields = mode_control_fields.0;

        let anchor_frames_num = anchor_frames_list.len() as u8;
        let anchor_frames_list = anchor_frames_list.as_mut_ptr();

        let film_grain_info = film_grain_info.0;
        let loop_restoration_fields = loop_restoration_fields.0;

        // Will not panic, as the arrays are the same size.
        let wm = wm
            .iter()
            .map(|wm| wm.0)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        Self(Box::new(bindings::VADecPictureParameterBufferAV1 {
            profile,
            order_hint_bits_minus_1,
            bit_depth_idx,
            matrix_coefficients,
            seq_info_fields,
            current_frame,
            current_display_picture,
            anchor_frames_num,
            anchor_frames_list,
            frame_width_minus1,
            frame_height_minus1,
            output_frame_width_in_tiles_minus_1,
            output_frame_height_in_tiles_minus_1,
            ref_frame_map,
            ref_frame_idx,
            primary_ref_frame,
            order_hint,
            seg_info,
            film_grain_info,
            tile_cols,
            tile_rows,
            width_in_sbs_minus_1,
            height_in_sbs_minus_1,
            tile_count_minus_1,
            context_update_tile_id,
            pic_info_fields,
            superres_scale_denominator,
            interp_filter,
            filter_level,
            filter_level_u,
            filter_level_v,
            loop_filter_info_fields,
            ref_deltas,
            mode_deltas,
            base_qindex,
            y_dc_delta_q,
            u_dc_delta_q,
            u_ac_delta_q,
            v_dc_delta_q,
            v_ac_delta_q,
            qmatrix_fields,
            mode_control_fields,
            cdef_damping_minus_3,
            cdef_bits,
            cdef_y_strengths,
            cdef_uv_strengths,
            loop_restoration_fields,
            wm,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VADecPictureParameterBufferAV1 {
        self.0.as_mut()
    }
}

/// A wrapper over an array of the `VASliceParameterBufferAV1` FFI type. This
/// allows for passing all tile parameters in a single call if multiple tiles
/// are present in the tile group.
#[derive(Default)]
pub struct SliceParameterBufferAV1(Vec<bindings::VASliceParameterBufferAV1>);

impl SliceParameterBufferAV1 {
    /// Creates the wrapper
    pub fn new() -> Self {
        Default::default()
    }

    /// Adds a slice parameter to the wrapper
    #[allow(clippy::too_many_arguments)]
    pub fn add_slice_parameter(
        &mut self,
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        tile_row: u16,
        tile_column: u16,
        tg_start: u16,
        tg_end: u16,
        anchor_frame_idx: u8,
        tile_idx_in_tile_list: u16,
    ) {
        self.0.push(bindings::VASliceParameterBufferAV1 {
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            tile_row,
            tile_column,
            tg_start,
            tg_end,
            anchor_frame_idx,
            tile_idx_in_tile_list,
            va_reserved: Default::default(),
        });
    }

    pub(crate) fn inner_mut(&mut self) -> &mut Vec<bindings::VASliceParameterBufferAV1> {
        self.0.as_mut()
    }
}

pub struct AV1EncSeqFields(bindings::_VAEncSequenceParameterBufferAV1__bindgen_ty_1);

impl AV1EncSeqFields {
    pub fn new(
        still_picture: bool,
        use_128x128_superblock: bool,
        enable_filter_intra: bool,
        enable_intra_edge_filter: bool,
        enable_interintra_compound: bool,
        enable_masked_compound: bool,
        enable_warped_motion: bool,
        enable_dual_filter: bool,
        enable_order_hint: bool,
        enable_jnt_comp: bool,
        enable_ref_frame_mvs: bool,
        enable_superres: bool,
        enable_cdef: bool,
        enable_restoration: bool,
        bit_depth_minus8: u32,
        subsampling_x: bool,
        subsampling_y: bool,
        mono_chrome: bool,
    ) -> Self {
        let still_picture = still_picture as u32;
        let use_128x128_superblock = use_128x128_superblock as u32;
        let enable_filter_intra = enable_filter_intra as u32;
        let enable_intra_edge_filter = enable_intra_edge_filter as u32;
        let enable_interintra_compound = enable_interintra_compound as u32;
        let enable_masked_compound = enable_masked_compound as u32;
        let enable_warped_motion = enable_warped_motion as u32;
        let enable_dual_filter = enable_dual_filter as u32;
        let enable_order_hint = enable_order_hint as u32;
        let enable_jnt_comp = enable_jnt_comp as u32;
        let enable_ref_frame_mvs = enable_ref_frame_mvs as u32;
        let enable_superres = enable_superres as u32;
        let enable_cdef = enable_cdef as u32;
        let enable_restoration = enable_restoration as u32;
        let subsampling_x = subsampling_x as u32;
        let subsampling_y = subsampling_y as u32;
        let mono_chrome = mono_chrome as u32;

        let _bitfield_1 =
            bindings::_VAEncSequenceParameterBufferAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                still_picture,
                use_128x128_superblock,
                enable_filter_intra,
                enable_intra_edge_filter,
                enable_interintra_compound,
                enable_masked_compound,
                enable_warped_motion,
                enable_dual_filter,
                enable_order_hint,
                enable_jnt_comp,
                enable_ref_frame_mvs,
                enable_superres,
                enable_cdef,
                enable_restoration,
                #[cfg(libva_1_15_or_higher)]
                bit_depth_minus8,
                #[cfg(libva_1_15_or_higher)]
                subsampling_x,
                #[cfg(libva_1_15_or_higher)]
                subsampling_y,
                #[cfg(libva_1_19_or_higher)]
                mono_chrome,
                Default::default(),
            );

        Self(bindings::_VAEncSequenceParameterBufferAV1__bindgen_ty_1 {
            bits: bindings::_VAEncSequenceParameterBufferAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct EncSequenceParameterBufferAV1(Box<bindings::VAEncSequenceParameterBufferAV1>);

impl EncSequenceParameterBufferAV1 {
    pub fn new(
        seq_profile: u8,
        seq_level_idx: u8,
        seq_tier: u8,
        hierarchical_flag: u8,
        intra_period: u32,
        ip_period: u32,
        bits_per_second: u32,
        seq_fields: &AV1EncSeqFields,
        order_hint_bits_minus_1: u8,
    ) -> Self {
        let seq_fields = seq_fields.0;

        Self(Box::new(bindings::_VAEncSequenceParameterBufferAV1 {
            seq_profile,
            seq_level_idx,
            seq_tier,
            #[cfg(libva_1_16_or_higher)]
            hierarchical_flag,
            intra_period,
            ip_period,
            bits_per_second,
            seq_fields,
            order_hint_bits_minus_1,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSequenceParameterBufferAV1 {
        &mut self.0
    }
}

#[derive(Default)]
pub struct RefFrameCtrlAV1(bindings::VARefFrameCtrlAV1);

impl RefFrameCtrlAV1 {
    pub fn new(
        search_idx0: u32,
        search_idx1: u32,
        search_idx2: u32,
        search_idx3: u32,
        search_idx4: u32,
        search_idx5: u32,
        search_idx6: u32,
    ) -> Self {
        let _bitfield_1 = bindings::VARefFrameCtrlAV1__bindgen_ty_1::new_bitfield_1(
            search_idx0,
            search_idx1,
            search_idx2,
            search_idx3,
            search_idx4,
            search_idx5,
            search_idx6,
            Default::default(),
        );

        Self(bindings::VARefFrameCtrlAV1 {
            fields: bindings::VARefFrameCtrlAV1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct AV1EncPictureFlags(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_1);

impl AV1EncPictureFlags {
    pub fn new(
        frame_type: u32,
        error_resilient_mode: bool,
        disable_cdf_update: bool,
        use_superres: bool,
        allow_high_precision_mv: bool,
        use_ref_frame_mvs: bool,
        disable_frame_end_update_cdf: bool,
        reduced_tx_set: bool,
        enable_frame_obu: bool,
        long_term_reference: bool,
        disable_frame_recon: bool,
        allow_intrabc: bool,
        palette_mode_enable: bool,
        allow_screen_content_tools: bool,
        force_integer_mv: bool,
    ) -> Self {
        let error_resilient_mode = error_resilient_mode as u32;
        let disable_cdf_update = disable_cdf_update as u32;
        let use_superres = use_superres as u32;
        let allow_high_precision_mv = allow_high_precision_mv as u32;
        let use_ref_frame_mvs = use_ref_frame_mvs as u32;
        let disable_frame_end_update_cdf = disable_frame_end_update_cdf as u32;
        let reduced_tx_set = reduced_tx_set as u32;
        let enable_frame_obu = enable_frame_obu as u32;
        let long_term_reference = long_term_reference as u32;
        let disable_frame_recon = disable_frame_recon as u32;
        let allow_intrabc = allow_intrabc as u32;
        let palette_mode_enable = palette_mode_enable as u32;
        #[cfg(libva_1_21_or_higher)]
        let allow_screen_content_tools = allow_screen_content_tools as u32;
        #[cfg(libva_1_21_or_higher)]
        let force_integer_mv = force_integer_mv as u32;

        let _bitfield_1 = {
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                frame_type,
                error_resilient_mode,
                disable_cdf_update,
                use_superres,
                allow_high_precision_mv,
                use_ref_frame_mvs,
                disable_frame_end_update_cdf,
                reduced_tx_set,
                enable_frame_obu,
                long_term_reference,
                disable_frame_recon,
                allow_intrabc,
                palette_mode_enable,
                #[cfg(libva_1_21_or_higher)]
                allow_screen_content_tools,
                #[cfg(libva_1_21_or_higher)]
                force_integer_mv,
                Default::default(),
            )
        };
        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_1 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct AV1EncLoopFilterFlags(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_2);

impl AV1EncLoopFilterFlags {
    pub fn new(
        sharpness_level: u8,
        mode_ref_delta_enabled: bool,
        mode_ref_delta_update: bool,
    ) -> Self {
        let mode_ref_delta_enabled = mode_ref_delta_enabled as u8;
        let mode_ref_delta_update = mode_ref_delta_update as u8;

        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_2__bindgen_ty_1::new_bitfield_1(
                sharpness_level,
                mode_ref_delta_enabled,
                mode_ref_delta_update,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_2 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_2__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct AV1EncQMatrixFlags(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_3);

impl AV1EncQMatrixFlags {
    pub fn new(using_qmatrix: bool, qm_y: u16, qm_u: u16, qm_v: u16) -> Self {
        let using_qmatrix = using_qmatrix as u16;

        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_3__bindgen_ty_1::new_bitfield_1(
                using_qmatrix,
                qm_y,
                qm_u,
                qm_v,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_3 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_3__bindgen_ty_1 {
                _bitfield_1,
                _bitfield_align_1: Default::default(),
            },
        })
    }
}

#[derive(Default)]
pub struct AV1EncModeControlFlags(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_4);

impl AV1EncModeControlFlags {
    pub fn new(
        delta_q_present: bool,
        delta_q_res: u32,
        delta_lf_present: bool,
        delta_lf_res: u32,
        delta_lf_multi: bool,
        tx_mode: u32,
        reference_mode: u32,
        skip_mode_present: bool,
    ) -> Self {
        let delta_q_present = delta_q_present as u32;
        let delta_lf_present = delta_lf_present as u32;
        let delta_lf_multi = delta_lf_multi as u32;
        let skip_mode_present = skip_mode_present as u32;

        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_4__bindgen_ty_1::new_bitfield_1(
                delta_q_present,
                delta_q_res,
                delta_lf_present,
                delta_lf_res,
                delta_lf_multi,
                tx_mode,
                reference_mode,
                skip_mode_present,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_4 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_4__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct EncSegParamFlagsAV1(bindings::_VAEncSegParamAV1__bindgen_ty_1);

impl EncSegParamFlagsAV1 {
    pub fn new(
        segmentation_enabled: bool,
        segmentation_update_map: bool,
        segmentation_temporal_update: bool,
    ) -> Self {
        let segmentation_enabled = segmentation_enabled as u8;
        let segmentation_update_map = segmentation_update_map as u8;
        let segmentation_temporal_update = segmentation_temporal_update as u8;

        let _bitfield_1 = bindings::_VAEncSegParamAV1__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
            segmentation_enabled,
            segmentation_update_map,
            segmentation_temporal_update,
            Default::default(),
        );

        Self(bindings::_VAEncSegParamAV1__bindgen_ty_1 {
            bits: bindings::_VAEncSegParamAV1__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct EncSegParamAV1(bindings::VAEncSegParamAV1);

impl EncSegParamAV1 {
    pub fn new(
        seg_flags: &EncSegParamFlagsAV1,
        segment_number: u8,
        feature_data: [[i16; 8usize]; 8usize],
        feature_mask: [u8; 8usize],
    ) -> Self {
        let seg_flags = seg_flags.0;

        Self(bindings::_VAEncSegParamAV1 {
            seg_flags,
            segment_number,
            feature_data,
            feature_mask,
            va_reserved: Default::default(),
        })
    }
}

pub struct AV1EncLoopRestorationFlags(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_5);

impl AV1EncLoopRestorationFlags {
    pub fn new(
        yframe_restoration_type: u16,
        cbframe_restoration_type: u16,
        crframe_restoration_type: u16,
        lr_unit_shift: u16,
        lr_uv_shift: bool,
    ) -> Self {
        let lr_uv_shift = lr_uv_shift as u16;

        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_5__bindgen_ty_1::new_bitfield_1(
                yframe_restoration_type,
                cbframe_restoration_type,
                crframe_restoration_type,
                lr_unit_shift,
                lr_uv_shift,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_5 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_5__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default, Copy, Clone)]
pub struct EncWarpedMotionParamsAV1(bindings::VAEncWarpedMotionParamsAV1);

impl EncWarpedMotionParamsAV1 {
    pub fn new(
        wmtype: bindings::VAEncTransformationTypeAV1::Type,
        wmmat: [i32; 8usize],
        invalid: u8,
    ) -> Self {
        Self(bindings::_VAEncWarpedMotionParamsAV1 {
            wmtype,
            wmmat,
            invalid,
            va_reserved: Default::default(),
        })
    }
}

pub struct AV1EncTileGroupObuHdrInfo(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_6);

impl AV1EncTileGroupObuHdrInfo {
    pub fn new(
        obu_extension_flag: bool,
        obu_has_size_field: bool,
        temporal_id: u8,
        spatial_id: u8,
    ) -> Self {
        let obu_extension_flag = obu_extension_flag as u8;
        let obu_has_size_field = obu_has_size_field as u8;

        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_6__bindgen_ty_1::new_bitfield_1(
                obu_extension_flag,
                obu_has_size_field,
                temporal_id,
                spatial_id,
                Default::default(),
            );

        Self(bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_6 {
            bits: bindings::_VAEncPictureParameterBufferAV1__bindgen_ty_6__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct EncPictureParameterBufferAV1(Box<bindings::VAEncPictureParameterBufferAV1>);

impl EncPictureParameterBufferAV1 {
    pub fn new(
        frame_width_minus_1: u16,
        frame_height_minus_1: u16,
        reconstructed_frame: bindings::VASurfaceID,
        coded_buf: bindings::VABufferID,
        reference_frames: [bindings::VASurfaceID; 8usize],
        ref_frame_idx: [u8; 7usize],
        hierarchical_level_plus1: u8,
        primary_ref_frame: u8,
        order_hint: u8,
        refresh_frame_flags: u8,
        ref_frame_ctrl_l0: &RefFrameCtrlAV1,
        ref_frame_ctrl_l1: &RefFrameCtrlAV1,
        picture_flags: &AV1EncPictureFlags,
        seg_id_block_size: u8,
        num_tile_groups_minus1: u8,
        temporal_id: u8,
        filter_level: [u8; 2usize],
        filter_level_u: u8,
        filter_level_v: u8,
        loop_filter_flags: &AV1EncLoopFilterFlags,
        superres_scale_denominator: u8,
        interpolation_filter: u8,
        ref_deltas: [i8; 8usize],
        mode_deltas: [i8; 2usize],
        base_qindex: u8,
        y_dc_delta_q: i8,
        u_dc_delta_q: i8,
        u_ac_delta_q: i8,
        v_dc_delta_q: i8,
        v_ac_delta_q: i8,
        min_base_qindex: u8,
        max_base_qindex: u8,
        qmatrix_flags: &AV1EncQMatrixFlags,
        mode_control_flags: &AV1EncModeControlFlags,
        segments: &EncSegParamAV1,
        tile_cols: u8,
        tile_rows: u8,
        width_in_sbs_minus_1: [u16; 63usize],
        height_in_sbs_minus_1: [u16; 63usize],
        context_update_tile_id: u16,
        cdef_damping_minus_3: u8,
        cdef_bits: u8,
        cdef_y_strengths: [u8; 8usize],
        cdef_uv_strengths: [u8; 8usize],
        loop_restoration_flags: &AV1EncLoopRestorationFlags,
        wm: [EncWarpedMotionParamsAV1; 7usize],
        bit_offset_qindex: u32,
        bit_offset_segmentation: u32,
        bit_offset_loopfilter_params: u32,
        bit_offset_cdef_params: u32,
        size_in_bits_cdef_params: u32,
        byte_offset_frame_hdr_obu_size: u32,
        size_in_bits_frame_hdr_obu: u32,
        tile_group_obu_hdr_info: &AV1EncTileGroupObuHdrInfo,
        number_skip_frames: u8,
        skip_frames_reduced_size: i32,
    ) -> Self {
        let ref_frame_ctrl_l0 = ref_frame_ctrl_l0.0;
        let ref_frame_ctrl_l1 = ref_frame_ctrl_l1.0;
        let picture_flags = picture_flags.0;
        let loop_filter_flags = loop_filter_flags.0;
        let qmatrix_flags = qmatrix_flags.0;
        let mode_control_flags = mode_control_flags.0;
        let segments = segments.0;
        let loop_restoration_flags = loop_restoration_flags.0;
        let wm = wm.map(|e| e.0);
        let tile_group_obu_hdr_info = tile_group_obu_hdr_info.0;

        Self(Box::new(bindings::_VAEncPictureParameterBufferAV1 {
            frame_width_minus_1,
            frame_height_minus_1,
            reconstructed_frame,
            coded_buf,
            reference_frames,
            ref_frame_idx,
            #[cfg(libva_1_19_or_higher)]
            hierarchical_level_plus1,
            primary_ref_frame,
            order_hint,
            #[cfg(libva_1_15_or_higher)]
            refresh_frame_flags,
            ref_frame_ctrl_l0,
            ref_frame_ctrl_l1,
            picture_flags,
            seg_id_block_size,
            num_tile_groups_minus1,
            temporal_id,
            filter_level,
            filter_level_u,
            filter_level_v,
            loop_filter_flags,
            superres_scale_denominator,
            interpolation_filter,
            ref_deltas,
            mode_deltas,
            base_qindex,
            y_dc_delta_q,
            u_dc_delta_q,
            u_ac_delta_q,
            v_dc_delta_q,
            v_ac_delta_q,
            min_base_qindex,
            max_base_qindex,
            qmatrix_flags,
            mode_control_flags,
            segments,
            tile_cols,
            tile_rows,
            width_in_sbs_minus_1,
            height_in_sbs_minus_1,
            context_update_tile_id,
            cdef_damping_minus_3,
            cdef_bits,
            cdef_y_strengths,
            cdef_uv_strengths,
            loop_restoration_flags,
            wm,
            bit_offset_qindex,
            bit_offset_segmentation,
            bit_offset_loopfilter_params,
            bit_offset_cdef_params,
            size_in_bits_cdef_params,
            byte_offset_frame_hdr_obu_size,
            size_in_bits_frame_hdr_obu,
            tile_group_obu_hdr_info,
            number_skip_frames,
            skip_frames_reduced_size,
            ..Default::default()
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncPictureParameterBufferAV1 {
        &mut self.0
    }
}

pub struct EncTileGroupBufferAV1(Box<bindings::VAEncTileGroupBufferAV1>);

impl EncTileGroupBufferAV1 {
    pub fn new(tg_start: u8, tg_end: u8) -> Self {
        Self(Box::new(bindings::VAEncTileGroupBufferAV1 {
            tg_start,
            tg_end,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncTileGroupBufferAV1 {
        &mut self.0
    }
}

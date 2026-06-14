// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around MPEG2 `VABuffer` types.

use crate::bindings;

/// Wrapper over the `picture_coding_extension` bindgen field in `VAPictureParameterBufferMPEG2`.
pub struct MPEG2PictureCodingExtension(bindings::_VAPictureParameterBufferMPEG2__bindgen_ty_1);

impl MPEG2PictureCodingExtension {
    /// Creates the bindgen field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        intra_dc_precision: u32,
        picture_structure: u32,
        top_field_first: u32,
        frame_pred_frame_dct: u32,
        concealment_motion_vectors: u32,
        q_scale_type: u32,
        intra_vlc_format: u32,
        alternate_scan: u32,
        repeat_first_field: u32,
        progressive_frame: u32,
        is_first_field: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAPictureParameterBufferMPEG2__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                intra_dc_precision,
                picture_structure,
                top_field_first,
                frame_pred_frame_dct,
                concealment_motion_vectors,
                q_scale_type,
                intra_vlc_format,
                alternate_scan,
                repeat_first_field,
                progressive_frame,
                is_first_field,
            );

        Self(bindings::_VAPictureParameterBufferMPEG2__bindgen_ty_1 {
            bits: bindings::_VAPictureParameterBufferMPEG2__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferMPEG2__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `PictureParameterBufferMPEG2` FFI type.
pub struct PictureParameterBufferMPEG2(Box<bindings::VAPictureParameterBufferMPEG2>);

impl PictureParameterBufferMPEG2 {
    /// Creates the wrapper.
    pub fn new(
        horizontal_size: u16,
        vertical_size: u16,
        forward_reference_picture: bindings::VASurfaceID,
        backward_reference_picture: bindings::VASurfaceID,
        picture_coding_type: i32,
        f_code: i32,
        picture_coding_extension: &MPEG2PictureCodingExtension,
    ) -> Self {
        let picture_coding_extension = picture_coding_extension.0;

        Self(Box::new(bindings::VAPictureParameterBufferMPEG2 {
            horizontal_size,
            vertical_size,
            forward_reference_picture,
            backward_reference_picture,
            picture_coding_type,
            f_code,
            picture_coding_extension,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferMPEG2 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::VAPictureParameterBufferMPEG2 {
        self.0.as_ref()
    }
}

/// Wrapper over the `VASliceParameterBufferMPEG2` FFI type.
pub struct SliceParameterBufferMPEG2(Box<bindings::VASliceParameterBufferMPEG2>);

impl SliceParameterBufferMPEG2 {
    /// Creates the wrapper.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        macroblock_offset: u32,
        slice_horizontal_position: u32,
        slice_vertical_position: u32,
        quantiser_scale_code: i32,
        intra_slice_flag: i32,
    ) -> Self {
        Self(Box::new(bindings::VASliceParameterBufferMPEG2 {
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            macroblock_offset,
            slice_horizontal_position,
            slice_vertical_position,
            quantiser_scale_code,
            intra_slice_flag,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VASliceParameterBufferMPEG2 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VASliceParameterBufferMPEG2 {
        self.0.as_ref()
    }
}

/// Wrapper over the `VAIQMatrixBufferMPEG2` FFI type.
pub struct IQMatrixBufferMPEG2(Box<bindings::VAIQMatrixBufferMPEG2>);

impl IQMatrixBufferMPEG2 {
    /// Creates the wrapper.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        load_intra_quantiser_matrix: i32,
        load_non_intra_quantiser_matrix: i32,
        load_chroma_intra_quantiser_matrix: i32,
        load_chroma_non_intra_quantiser_matrix: i32,
        intra_quantiser_matrix: [u8; 64usize],
        non_intra_quantiser_matrix: [u8; 64usize],
        chroma_intra_quantiser_matrix: [u8; 64usize],
        chroma_non_intra_quantiser_matrix: [u8; 64usize],
    ) -> Self {
        Self(Box::new(bindings::VAIQMatrixBufferMPEG2 {
            load_intra_quantiser_matrix,
            load_non_intra_quantiser_matrix,
            load_chroma_intra_quantiser_matrix,
            load_chroma_non_intra_quantiser_matrix,
            intra_quantiser_matrix,
            non_intra_quantiser_matrix,
            chroma_intra_quantiser_matrix,
            chroma_non_intra_quantiser_matrix,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAIQMatrixBufferMPEG2 {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAIQMatrixBufferMPEG2 {
        self.0.as_ref()
    }
}

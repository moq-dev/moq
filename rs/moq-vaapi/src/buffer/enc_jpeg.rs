// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around JPEG-Encoding `VABuffer` types.

use crate::bindings;

/// Wrapper over the `pic_flags` bindgen field in `VAEncPictureParameterBufferJPEG`
pub struct PicFlags(bindings::_VAEncPictureParameterBufferJPEG__bindgen_ty_1);

impl PicFlags {
    pub fn new(
        profile: u32,
        progressive: u32,
        huffman: u32,
        interleaved: u32,
        differential: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncPictureParameterBufferJPEG__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                profile,
                progressive,
                huffman,
                interleaved,
                differential,
            );
        Self(bindings::_VAEncPictureParameterBufferJPEG__bindgen_ty_1 {
            bits: bindings::_VAEncPictureParameterBufferJPEG__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
                __bindgen_padding_0: Default::default(),
            },
        })
    }
}

/// Wrapper over the `VAEncPictureParameterBufferJPEG` FFI type.
pub struct EncPictureParameterBufferJPEG(Box<bindings::VAEncPictureParameterBufferJPEG>);

impl EncPictureParameterBufferJPEG {
    /// Creates the wrapper.
    pub fn new(
        reconstructed_picture: bindings::VASurfaceID,
        picture_width: u16,
        picture_height: u16,
        coded_buf: bindings::VABufferID,
        pic_flags: PicFlags,
        sample_bit_depth: u8,
        num_scan: u8,
        num_components: u16,
        component_id: [u8; 4usize],
        quantiser_table_selector: [u8; 4usize],
        quality: u8,
    ) -> Self {
        Self(Box::new(bindings::VAEncPictureParameterBufferJPEG {
            reconstructed_picture,
            picture_width,
            picture_height,
            coded_buf,
            pic_flags: pic_flags.0,
            sample_bit_depth,
            num_scan,
            num_components,
            component_id,
            quantiser_table_selector,
            quality,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncPictureParameterBufferJPEG {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAEncPictureParameterBufferJPEG {
        self.0.as_ref()
    }
}

/// Wrapper over the `components` bindgen field in `VAEncSliceParameterBufferJPEG`.
pub struct EncSliceParameterBufferJPEGComponent(
    bindings::_VAEncSliceParameterBufferJPEG__bindgen_ty_1,
);

impl EncSliceParameterBufferJPEGComponent {
    /// Creates the bindgen field.
    pub fn new(component_selector: u8, dc_table_selector: u8, ac_table_selector: u8) -> Self {
        Self(bindings::_VAEncSliceParameterBufferJPEG__bindgen_ty_1 {
            component_selector,
            dc_table_selector,
            ac_table_selector,
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::_VAEncSliceParameterBufferJPEG__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `VAEncSliceParameterBufferJPEG` FFI type.
pub struct EncSliceParameterBufferJPEG(Box<bindings::VAEncSliceParameterBufferJPEG>);

impl EncSliceParameterBufferJPEG {
    /// Creates the wrapper.
    pub fn new(
        restart_interval: u16,
        num_components: u16,
        components: [EncSliceParameterBufferJPEGComponent; 4usize],
    ) -> Self {
        Self(Box::new(bindings::VAEncSliceParameterBufferJPEG {
            restart_interval,
            num_components,
            components: components.map(|component| component.0),
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAEncSliceParameterBufferJPEG {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAEncSliceParameterBufferJPEG {
        self.0.as_ref()
    }
}

/// Wrapper over the `VAQMatrixBufferJPEG` FFI type.
pub struct QMatrixBufferJPEG(Box<bindings::VAQMatrixBufferJPEG>);

impl QMatrixBufferJPEG {
    /// Creates the wrapper.
    pub fn new(
        load_lum_quantiser_matrix: i32,
        load_chroma_quantiser_matrix: i32,
        lum_quantiser_matrix: [u8; 64usize],
        chroma_quantiser_matrix: [u8; 64usize],
    ) -> Self {
        Self(Box::new(bindings::VAQMatrixBufferJPEG {
            load_lum_quantiser_matrix,
            load_chroma_quantiser_matrix,
            lum_quantiser_matrix,
            chroma_quantiser_matrix,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAQMatrixBufferJPEG {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAQMatrixBufferJPEG {
        self.0.as_ref()
    }
}

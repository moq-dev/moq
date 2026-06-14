// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around JPEGBaseline `VABuffer` types.

use crate::bindings;

/// Wrapper over the `components` bindgen field in `VAPictureParameterBufferJPEGBaseline`.
pub struct PictureParameterBufferJPEGBaselineComponent(
    bindings::_VAPictureParameterBufferJPEGBaseline__bindgen_ty_1,
);

impl PictureParameterBufferJPEGBaselineComponent {
    /// Creates the bindgen field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        component_id: u8,
        h_sampling_factor: u8,
        v_sampling_factor: u8,
        quantiser_table_selector: u8,
    ) -> Self {
        Self(
            bindings::_VAPictureParameterBufferJPEGBaseline__bindgen_ty_1 {
                component_id,
                h_sampling_factor,
                v_sampling_factor,
                quantiser_table_selector,
            },
        )
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAPictureParameterBufferJPEGBaseline__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `VAPictureParameterBufferJPEGBaseline` FFI type.
pub struct PictureParameterBufferJPEGBaseline(Box<bindings::VAPictureParameterBufferJPEGBaseline>);

impl PictureParameterBufferJPEGBaseline {
    /// Creates the wrapper.
    pub fn new(
        picture_width: u16,
        picture_height: u16,
        components: [PictureParameterBufferJPEGBaselineComponent; 255usize],
        num_components: u8,
        color_space: u8,
        rotation: u32,
        crop_rectangle: bindings::VARectangle,
    ) -> Self {
        Self(Box::new(bindings::VAPictureParameterBufferJPEGBaseline {
            picture_width,
            picture_height,
            components: components.map(|component| component.0),
            num_components,
            color_space,
            rotation,
            #[cfg(libva_1_20_or_higher)]
            crop_rectangle,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAPictureParameterBufferJPEGBaseline {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::VAPictureParameterBufferJPEGBaseline {
        self.0.as_ref()
    }
}

/// Wrapper over the `components` bindgen field in `VASliceParameterBufferJPEGBaseline`.
pub struct VASliceParameterBufferJPEGBaselineComponent(
    bindings::_VASliceParameterBufferJPEGBaseline__bindgen_ty_1,
);

impl VASliceParameterBufferJPEGBaselineComponent {
    /// Creates the bindgen field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(component_selector: u8, dc_table_selector: u8, ac_table_selector: u8) -> Self {
        Self(
            bindings::_VASliceParameterBufferJPEGBaseline__bindgen_ty_1 {
                component_selector,
                dc_table_selector,
                ac_table_selector,
            },
        )
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VASliceParameterBufferJPEGBaseline__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `VASliceParameterBufferJPEGBaseline` FFI type.
pub struct SliceParameterBufferJPEGBaseline(Box<bindings::VASliceParameterBufferJPEGBaseline>);

impl SliceParameterBufferJPEGBaseline {
    /// Creates the wrapper.
    pub fn new(
        slice_data_size: u32,
        slice_data_offset: u32,
        slice_data_flag: u32,
        slice_horizontal_position: u32,
        slice_vertical_position: u32,
        components: [VASliceParameterBufferJPEGBaselineComponent; 4usize],
        num_components: u8,
        restart_interval: u16,
        num_mcus: u32,
    ) -> Self {
        Self(Box::new(bindings::VASliceParameterBufferJPEGBaseline {
            slice_data_size,
            slice_data_offset,
            slice_data_flag,
            slice_horizontal_position,
            slice_vertical_position,
            components: components.map(|component| component.0),
            num_components,
            restart_interval,
            num_mcus,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VASliceParameterBufferJPEGBaseline {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::VASliceParameterBufferJPEGBaseline {
        self.0.as_ref()
    }
}

/// Wrapper over the `VAIQMatrixBufferJPEGBaseline` FFI type.
pub struct IQMatrixBufferJPEGBaseline(Box<bindings::VAIQMatrixBufferJPEGBaseline>);

impl IQMatrixBufferJPEGBaseline {
    /// Creates the wrapper.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        load_quantiser_table: [u8; 4usize],
        quantiser_table: [[u8; 64usize]; 4usize],
    ) -> Self {
        Self(Box::new(bindings::VAIQMatrixBufferJPEGBaseline {
            load_quantiser_table,
            quantiser_table,
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAIQMatrixBufferJPEGBaseline {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAIQMatrixBufferJPEGBaseline {
        self.0.as_ref()
    }
}

/// Wrapper over the `huffman_table` bindgen field in `VAHuffmanTableBufferJPEGBaseline`.
pub struct HuffmanTableBufferJPEGBaselineHuffmanTable(
    bindings::_VAHuffmanTableBufferJPEGBaseline__bindgen_ty_1,
);

impl HuffmanTableBufferJPEGBaselineHuffmanTable {
    /// Creates the bindgen field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        num_dc_codes: [u8; 16usize],
        dc_values: [u8; 12usize],
        num_ac_codes: [u8; 16usize],
        ac_values: [u8; 162usize],
        pad: [u8; 2usize],
    ) -> Self {
        Self(bindings::_VAHuffmanTableBufferJPEGBaseline__bindgen_ty_1 {
            num_dc_codes,
            dc_values,
            num_ac_codes,
            ac_values,
            pad,
        })
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::_VAHuffmanTableBufferJPEGBaseline__bindgen_ty_1 {
        &self.0
    }
}

/// Wrapper over the `VAHuffmanTableBufferJPEGBaseline` FFI type.
pub struct HuffmanTableBufferJPEGBaseline(Box<bindings::VAHuffmanTableBufferJPEGBaseline>);

impl HuffmanTableBufferJPEGBaseline {
    /// Creates the wrapper.
    pub fn new(
        load_huffman_table: [u8; 2usize],
        huffman_table: [HuffmanTableBufferJPEGBaselineHuffmanTable; 2usize],
    ) -> Self {
        Self(Box::new(bindings::VAHuffmanTableBufferJPEGBaseline {
            load_huffman_table,
            huffman_table: huffman_table.map(|component| component.0),
            va_reserved: Default::default(),
        }))
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAHuffmanTableBufferJPEGBaseline {
        self.0.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&mut self) -> &bindings::VAHuffmanTableBufferJPEGBaseline {
        self.0.as_ref()
    }
}

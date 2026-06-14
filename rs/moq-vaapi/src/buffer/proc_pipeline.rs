// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around `VAProcPipeline` types.

use crate::bindings;
use std::{marker::PhantomData, ptr};

/// Wrapper over the `VABlendState` ffi type.
pub struct BlendState(bindings::VABlendState);

impl BlendState {
    /// Creates the bindgen field
    pub fn new(flags: u32, global_alpha: f32, min_luma: f32, max_luma: f32) -> Self {
        Self(bindings::VABlendState {
            flags,
            global_alpha,
            min_luma,
            max_luma,
        })
    }
}

/// Wrapper over the `VAProcColorProperties` ffi type.
pub struct ProcColorProperties(bindings::VAProcColorProperties);

impl ProcColorProperties {
    /// Creates the bindgen field
    pub fn new(
        chroma_sample_location: u8,
        color_range: u8,
        colour_primaries: u8,
        transfer_characteristics: u8,
        matrix_coefficients: u8,
    ) -> Self {
        Self(bindings::VAProcColorProperties {
            chroma_sample_location,
            color_range,
            colour_primaries,
            transfer_characteristics,
            matrix_coefficients,
            reserved: Default::default(),
        })
    }
}

impl Default for ProcColorProperties {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0)
    }
}

/// Wrapper over the `VAHdrMetaData` ffi type.
pub struct HdrMetaData(bindings::VAHdrMetaData);

impl<'a> HdrMetaData {
    /// Creates the bindgen field
    pub fn new(metadata_type: u32, metadata: Option<&'a [u8]>, metadata_size: u32) -> Self {
        Self(bindings::VAHdrMetaData {
            metadata_type: metadata_type as _,
            metadata: metadata.map_or(ptr::null_mut(), |f| f.as_ptr() as *mut _),
            metadata_size,
            reserved: Default::default(),
        })
    }
}

/// Wrapper over the `VAProcPipelineParameterBuffer` FFI type.
pub struct ProcPipelineParameterBuffer {
    c_params: Box<bindings::VAProcPipelineParameterBuffer>,

    // Fields that own the data for the pointers in `c_params`.
    surface_region: Option<Box<bindings::VARectangle>>,
    output_region: Option<Box<bindings::VARectangle>>,
    filters: Option<Vec<bindings::VABufferID>>,
    forward_references: Option<Vec<bindings::VASurfaceID>>,
    backward_references: Option<Vec<bindings::VASurfaceID>>,
    blend_state: Option<Vec<BlendState>>,
    additional_outputs: Option<Vec<bindings::VASurfaceID>>,
    output_hdr_metadata: Option<Vec<HdrMetaData>>,
}

impl ProcPipelineParameterBuffer {
    /// Creates the wrapper.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        surface: bindings::VASurfaceID,
        surface_region: Option<bindings::VARectangle>,
        surface_color_standard: u8,
        output_region: Option<bindings::VARectangle>,
        output_background_color: u32,
        output_color_standard: u8,
        pipeline_flags: u32,
        filter_flags: u32,
        filters: Option<Vec<bindings::VABufferID>>,
        forward_references: Option<Vec<bindings::VASurfaceID>>,
        backward_references: Option<Vec<bindings::VASurfaceID>>,
        rotation_state: u32,
        blend_state: Option<Vec<BlendState>>,
        mirror_state: u32,
        additional_outputs: Option<Vec<bindings::VASurfaceID>>,
        input_surface_flag: u32,
        output_surface_flag: u32,
        input_color_properties: ProcColorProperties,
        output_color_properties: ProcColorProperties,
        processing_mode: u32,
        output_hdr_metadata: Option<Vec<HdrMetaData>>,
    ) -> Self {
        let mut slf = Self {
            // SAFETY: The VA-API structures are C-compatible so zeroing is safe.
            c_params: Box::new(unsafe { std::mem::zeroed() }),
            surface_region: surface_region.map(Box::new),
            output_region: output_region.map(Box::new),
            filters,
            forward_references,
            backward_references,
            blend_state,
            additional_outputs,
            output_hdr_metadata,
        };

        slf.c_params = Box::new(bindings::VAProcPipelineParameterBuffer {
            surface,
            surface_region: slf
                .surface_region
                .as_deref()
                .map_or(ptr::null_mut(), |r| r as *const _ as *mut _),
            surface_color_standard: surface_color_standard as _,
            output_region: slf
                .output_region
                .as_deref()
                .map_or(ptr::null_mut(), |r| r as *const _ as *mut _),
            output_background_color,
            output_color_standard: output_color_standard as _,
            pipeline_flags,
            filter_flags,
            filters: slf
                .filters
                .as_deref()
                .map_or(ptr::null_mut(), |f| f.as_ptr() as *mut _),
            num_filters: slf.filters.as_ref().map_or(0, |f| f.len() as u32),
            forward_references: slf
                .forward_references
                .as_deref()
                .map_or(ptr::null_mut(), |r| r.as_ptr() as *mut _),
            num_forward_references: slf
                .forward_references
                .as_ref()
                .map_or(0, |f| f.len() as u32),
            backward_references: slf
                .backward_references
                .as_deref()
                .map_or(ptr::null_mut(), |r| r.as_ptr() as *mut _),
            num_backward_references: slf
                .backward_references
                .as_ref()
                .map_or(0, |b| b.len() as u32),
            rotation_state,
            blend_state: slf
                .blend_state
                .as_deref()
                .map_or(ptr::null(), |b| b.as_ptr() as *const bindings::VABlendState),
            mirror_state,
            additional_outputs: slf
                .additional_outputs
                .as_deref()
                .map_or(ptr::null_mut(), |a| a.as_ptr() as *mut _),
            num_additional_outputs: slf
                .additional_outputs
                .as_ref()
                .map_or(0, |a| a.len() as u32),
            input_surface_flag,
            output_surface_flag,
            input_color_properties: input_color_properties.0,
            output_color_properties: output_color_properties.0,
            processing_mode,
            output_hdr_metadata: slf
                .output_hdr_metadata
                .as_deref()
                .map_or(ptr::null_mut(), |o| {
                    o.as_ptr() as *mut bindings::VAHdrMetaData
                }),
            va_reserved: Default::default(),
        });

        slf
    }

    pub(crate) fn inner_mut(&mut self) -> &mut bindings::VAProcPipelineParameterBuffer {
        self.c_params.as_mut()
    }

    /// Returns the inner FFI type. Useful for testing purposes.
    pub fn inner(&self) -> &bindings::VAProcPipelineParameterBuffer {
        self.c_params.as_ref()
    }
}

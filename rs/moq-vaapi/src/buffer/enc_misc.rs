// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers around `VAEncMisc*` types.

use crate::bindings;

#[repr(C)]
#[derive(Default)]
pub struct MiscEncParamBuffer<T> {
    hdr: bindings::VAEncMiscParameterBuffer,
    value: T,
}

impl<T> MiscEncParamBuffer<T> {
    fn new(type_: bindings::VAEncMiscParameterType::Type, value: T) -> Self {
        Self {
            hdr: bindings::_VAEncMiscParameterBuffer {
                type_,
                data: Default::default(),
            },
            value,
        }
    }

    fn new_boxed(type_: bindings::VAEncMiscParameterType::Type, value: T) -> Box<Self> {
        Box::new(Self::new(type_, value))
    }
}

#[derive(Default)]
pub struct EncMiscParameterFrameRate(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterFrameRate>>,
);

impl EncMiscParameterFrameRate {
    pub fn new(framerate: u32, temporal_id: u32) -> Self {
        // libva expects `(denominator << 16) | numerator`, and we set framerate denominator to 1.
        let framerate = (1 << 16) | (framerate & 0xFFFF);

        let _bitfield_1 =
            bindings::_VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                temporal_id,
                Default::default(),
            );

        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeFrameRate,
            bindings::_VAEncMiscParameterFrameRate {
                framerate,
                framerate_flags: bindings::_VAEncMiscParameterFrameRate__bindgen_ty_1 {
                    bits: bindings::_VAEncMiscParameterFrameRate__bindgen_ty_1__bindgen_ty_1 {
                        _bitfield_align_1: Default::default(),
                        _bitfield_1,
                    },
                },
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterFrameRate> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterFrameRate> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct RcFlags(bindings::_VAEncMiscParameterRateControl__bindgen_ty_1);

impl RcFlags {
    pub fn new(
        reset: u32,
        disable_frame_skip: u32,
        disable_bit_stuffing: u32,
        mb_rate_control: u32,
        temporal_id: u32,
        cfs_i_frames: u32,
        enable_parallel_brc: u32,
        enable_dynamic_scaling: u32,
        frame_tolerance_mode: u32,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                reset,
                disable_frame_skip,
                disable_bit_stuffing,
                mb_rate_control,
                temporal_id,
                cfs_i_frames,
                enable_parallel_brc,
                enable_dynamic_scaling,
                frame_tolerance_mode,
                Default::default(),
            );

        Self(bindings::_VAEncMiscParameterRateControl__bindgen_ty_1 {
            bits: bindings::_VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1 {
                _bitfield_align_1: Default::default(),
                _bitfield_1,
            },
        })
    }
}

#[derive(Default)]
pub struct EncMiscParameterRateControl(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterRateControl>>,
);

impl EncMiscParameterRateControl {
    pub fn new(
        bits_per_second: u32,
        target_percentage: u32,
        window_size: u32,
        initial_qp: u32,
        min_qp: u32,
        basic_unit_size: u32,
        rc_flags: RcFlags,
        icq_quality_factor: u32,
        max_qp: u32,
        quality_factor: u32,
        target_frame_size: u32,
    ) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeRateControl,
            bindings::VAEncMiscParameterRateControl {
                bits_per_second,
                target_percentage,
                window_size,
                initial_qp,
                min_qp,
                basic_unit_size,
                rc_flags: rc_flags.0,
                ICQ_quality_factor: icq_quality_factor,
                max_qp,
                quality_factor,
                #[cfg(libva_1_10_or_higher)]
                target_frame_size,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterRateControl> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterRateControl> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterMaxSliceSize(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterMaxSliceSize>>,
);

impl EncMiscParameterMaxSliceSize {
    pub fn new(max_slice_size: u32) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeMaxFrameSize,
            bindings::VAEncMiscParameterMaxSliceSize {
                max_slice_size,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterMaxSliceSize> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterMaxSliceSize> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterBufferMaxFrameSize(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterBufferMaxFrameSize>>,
);

impl EncMiscParameterBufferMaxFrameSize {
    pub fn new(max_frame_size: u32) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeMaxFrameSize,
            bindings::VAEncMiscParameterBufferMaxFrameSize {
                type_: bindings::VAEncMiscParameterType::VAEncMiscParameterTypeMaxFrameSize,
                max_frame_size,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterBufferMaxFrameSize> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterBufferMaxFrameSize> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterSkipFrame(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterSkipFrame>>,
);

impl EncMiscParameterSkipFrame {
    pub fn new(skip_frame_flag: u8, num_skip_frames: u8, size_skip_frames: u32) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeSkipFrame,
            bindings::VAEncMiscParameterSkipFrame {
                skip_frame_flag,
                num_skip_frames,
                size_skip_frames,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterSkipFrame> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterSkipFrame> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterHRD(Box<MiscEncParamBuffer<bindings::VAEncMiscParameterHRD>>);

impl EncMiscParameterHRD {
    pub fn new(initial_buffer_fullness: u32, buffer_size: u32) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeHRD,
            bindings::VAEncMiscParameterHRD {
                initial_buffer_fullness,
                buffer_size,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterHRD> {
        &self.0
    }

    pub(crate) fn inner_mut(&mut self) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterHRD> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterBufferQualityLevel(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterBufferQualityLevel>>,
);

impl EncMiscParameterBufferQualityLevel {
    pub fn new(quality_level: u32) -> Self {
        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeQualityLevel,
            bindings::VAEncMiscParameterBufferQualityLevel {
                quality_level,
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterBufferQualityLevel> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterBufferQualityLevel> {
        &mut self.0
    }
}

#[derive(Default)]
pub struct EncMiscParameterQuantization(
    Box<MiscEncParamBuffer<bindings::VAEncMiscParameterQuantization>>,
);

impl EncMiscParameterQuantization {
    pub fn new(
        disable_trellis: bool,
        enable_trellis_i: bool,
        enable_trellis_p: bool,
        enable_trellis_b: bool,
    ) -> Self {
        let _bitfield_1 =
            bindings::_VAEncMiscParameterQuantization__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                disable_trellis as u32,
                enable_trellis_i as u32,
                enable_trellis_p as u32,
                enable_trellis_b as u32,
                Default::default(),
            );

        Self(MiscEncParamBuffer::new_boxed(
            bindings::VAEncMiscParameterType::VAEncMiscParameterTypeQuantization,
            bindings::VAEncMiscParameterQuantization {
                quantization_flags: bindings::_VAEncMiscParameterQuantization__bindgen_ty_1 {
                    bits: bindings::_VAEncMiscParameterQuantization__bindgen_ty_1__bindgen_ty_1 {
                        _bitfield_align_1: Default::default(),
                        _bitfield_1,
                    },
                },
                ..Default::default()
            },
        ))
    }

    pub fn inner(&self) -> &MiscEncParamBuffer<bindings::VAEncMiscParameterQuantization> {
        &self.0
    }

    pub(crate) fn inner_mut(
        &mut self,
    ) -> &mut MiscEncParamBuffer<bindings::VAEncMiscParameterQuantization> {
        &mut self.0
    }
}

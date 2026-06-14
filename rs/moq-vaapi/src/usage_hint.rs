// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;

use crate::bindings;

bitflags! {
    /// Gives the driver a hint of intended usage to optimize allocation (e.g. tiling).
    #[derive(Debug, Clone, Copy)]
    pub struct UsageHint: u32 {
        /// Surface used by video decoder.
        const USAGE_HINT_DECODER = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_DECODER;
        /// Surface used by video encoder.
        const USAGE_HINT_ENCODER = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER;
        /// Surface read by video post-processing.
        const USAGE_HINT_VPP_READ = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_READ;
        /// Surface written by video post-processing.
        const USAGE_HINT_VPP_WRITE = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE;
        /// Surface used for display.
        const USAGE_HINT_DISPLAY = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_DISPLAY;
        /// Surface used for export to third-party APIs, e.g. via `vaExportSurfaceHandle()`.
        const USAGE_HINT_EXPORT = bindings::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT;
    }
}

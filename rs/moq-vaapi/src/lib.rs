// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! VA-API H.264 and H.265 encode and decode, with libva loaded at runtime.
//!
//! [`encode::Encoder`] and [`decode::Decoder`] each open their own [`Display`], and
//! [`vpp::Processor`] imports, scales, and converts [`dmabuf::DmaBuf`]s on the GPU.

#![allow(
	dead_code,
	unused_imports,
	unexpected_cfgs,
	mismatched_lifetime_syntaxes,
	clippy::all,
	reason = "vendored from discord/cros-libva and discord/cros-codecs; kept close to upstream so fixes port cleanly"
)]
// VA-API and DMA-BUF are Linux-only, so elsewhere the crate is an empty stub and
// moq-video only depends on it there.
#![cfg_attr(
	not(target_os = "linux"),
	allow(
		rustdoc::broken_intra_doc_links,
		reason = "the items these docs link to are Linux-only"
	)
)]
#![cfg(target_os = "linux")]

mod bindings;
mod buffer;
mod config;
mod context;
mod display;
mod generic_value;
mod image;
mod picture;
mod pool;
mod surface;
mod usage_hint;

// Vendored from discord/cros-codecs (BSD-3-Clause): the backend-agnostic H.264
// bitstream layer (SPS/PPS/slice synthesis) plus a thin VA-API encode driver.
mod bitstream_utils;
mod codec;
pub mod decode;
pub mod encode;

// Written for this crate: DMA-BUF import, the color space an encoder labels its
// stream with, and the VA-API video post-processing entrypoint, which scales,
// converts, and re-tiles surfaces on the GPU.
mod color;
pub mod dmabuf;
pub mod vpp;

// The vendored modules name each other's items through these crate-root globs,
// as upstream does. Only the items below are public.
use bindings::*;
use buffer::*;
use config::*;
use context::*;
use generic_value::*;
use picture::*;
use surface::*;

pub use bindings::{
	VAEntrypoint, VAImage, VAImageFormat, VAProfile, VA_FOURCC_BGRA, VA_FOURCC_BGRX, VA_FOURCC_I420, VA_FOURCC_NV12,
	VA_FOURCC_RGBA, VA_FOURCC_RGBX, VA_RT_FORMAT_RGB32, VA_RT_FORMAT_YUV420,
};
pub use codec::Codec;
pub use color::{Color, Matrix};
pub use display::{Display, DrmDeviceIterator, OpenDrmDisplayError};
pub use image::Image;
pub use surface::{DrmPrimeSurfaceDescriptor, DrmPrimeSurfaceDescriptorLayer, DrmPrimeSurfaceDescriptorObject, Surface};
pub use usage_hint::UsageHint;

/// A frame resolution in pixels. (Vendored from discord/cros-codecs, BSD-3-Clause.)
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolution {
	pub width: u32,
	pub height: u32,
}

impl Resolution {
	/// Whether `self` can contain `other`.
	pub fn can_contain(&self, other: Self) -> bool {
		self.width >= other.width && self.height >= other.height
	}

	pub fn get_area(&self) -> usize {
		(self.width as usize) * (self.height as usize)
	}
}

impl From<(u32, u32)> for Resolution {
	fn from(value: (u32, u32)) -> Self {
		Self {
			width: value.0,
			height: value.1,
		}
	}
}

impl From<Resolution> for (u32, u32) {
	fn from(value: Resolution) -> Self {
		(value.width, value.height)
	}
}

use std::num::NonZeroI32;

/// A `VAStatus` that is guaranteed to not be `VA_STATUS_SUCCESS`.
#[derive(Debug)]
pub struct VaError(NonZeroI32);

impl VaError {
	/// Returns the `VAStatus` of this error.
	pub fn va_status(&self) -> VAStatus {
		self.0.get() as VAStatus
	}
}

impl std::fmt::Display for VaError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		use std::ffi::CStr;

		// Safe because `vaErrorStr` will return a pointer to a statically allocated, null
		// terminated C string. The pointer is guaranteed to never be null.
		let err_str = unsafe { CStr::from_ptr(bindings::va().vaErrorStr(self.0.get())) }
			.to_str()
			.unwrap();
		f.write_str(err_str)
	}
}

impl std::error::Error for VaError {}

/// Checks a VA return value and returns a `VaError` if it is not `VA_STATUS_SUCCESS`.
///
/// This can be used on the return value of any VA function returning `VAStatus` in order to
/// convert it to a proper Rust `Result`.
fn va_check(code: VAStatus) -> Result<(), VaError> {
	match code as u32 {
		bindings::VA_STATUS_SUCCESS => Ok(()),
		_ => Err(VaError(unsafe { NonZeroI32::new_unchecked(code) })),
	}
}

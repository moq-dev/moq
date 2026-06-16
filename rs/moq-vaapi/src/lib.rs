// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements a lightweight and safe interface over `libva`.
//!
//! The starting point to using this crate is to open a [`Display`], from which a [`Context`] and
//! [`Surface`]s can be allocated and used for doing actual work.

// Most of this crate is vendored verbatim from cros-libva + discord/cros-codecs
// (BSD-3-Clause). Don't subject upstream code to this workspace's strict lints;
// the build script also emits custom `libva_*_or_higher` cfgs.
#![allow(dead_code, unused_imports, unexpected_cfgs, mismatched_lifetime_syntaxes)]
#![allow(clippy::all)]

mod bindings;
pub mod buffer;
mod config;
mod context;
mod display;
mod generic_value;
mod image;
mod picture;
mod surface;
mod usage_hint;

// Vendored from discord/cros-codecs (BSD-3-Clause): the backend-agnostic H.264
// bitstream layer (SPS/PPS/slice synthesis) plus a thin VA-API encode driver.
pub mod bitstream_utils;
pub mod codec;
pub mod encode;

pub use bindings::_VADRMPRIMESurfaceDescriptor__bindgen_ty_1 as VADRMPRIMESurfaceDescriptorObject;
pub use bindings::_VADRMPRIMESurfaceDescriptor__bindgen_ty_2 as VADRMPRIMESurfaceDescriptorLayer;
pub use bindings::*;
pub use buffer::*;
pub use config::*;
pub use context::*;
pub use display::*;
pub use generic_value::*;
pub use image::*;
pub use picture::*;
pub use surface::*;
pub use usage_hint::*;

/// A frame resolution in pixels. (Vendored from discord/cros-codecs, BSD-3-Clause.)
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolution {
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
		let err_str = unsafe { CStr::from_ptr(bindings::vaErrorStr(self.0.get())) }
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

// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use crate::bindings;
use crate::va_check;
use crate::Display;
use crate::Surface;
use crate::SurfaceMemoryDescriptor;
use crate::VaError;

/// Wrapper around `VAImage` that is tied to the lifetime of a given `Picture`.
///
/// An image is used to either get the surface data to client memory, or to copy image data in
/// client memory to a surface.
pub struct Image<'a> {
    /// The display from which the image was created, so we can unmap it upon destruction.
    display: Arc<Display>,
    /// The `VAImage` returned by libva.
    image: bindings::VAImage,
    /// The mapped surface data.
    ///
    /// The lifetime also allows us to ensure that the `Surface` we have been created from
    /// does not drop or get changed while we exist.
    data: &'a mut [u8],
    /// Whether the image was derived using the `vaDeriveImage` API or created using the
    /// `vaCreateImage` API.
    derived: bool,
    /// The display resolution requested by the client. The implementation is
    /// free to enlarge this value as needed. In any case, we guarantee that an
    /// image at least as large is returned.
    display_resolution: (u32, u32),
    /// Tracks whether the underlying data has possibly been written to, i.e. an encoder will create
    /// an image and map its buffer in order to write to it, so we must writeback later.
    dirty: bool,
    /// The ID of the `Surface` we have been created from.
    surface_id: u32,
}

impl<'a> Image<'a> {
    /// Helper method to map a `VAImage` using `vaMapBuffer` and return an `Image`.
    ///
    /// Returns an error if the mapping failed.
    fn new<D: SurfaceMemoryDescriptor>(
        surface: &'a Surface<D>,
        image: bindings::VAImage,
        derived: bool,
        display_resolution: (u32, u32),
    ) -> Result<Self, VaError> {
        let mut addr = std::ptr::null_mut();

        // Safe since `picture.inner.context` represents a valid `VAContext` and `image` has been
        // successfully created at this point.
        match va_check(unsafe {
            bindings::vaMapBuffer(surface.display().handle(), image.buf, &mut addr)
        }) {
            Ok(_) => {
                // Assert that libva provided us with a coded resolution that is
                // at least as large as `display_resolution`.
                assert!(u32::from(image.width) >= display_resolution.0);
                assert!(u32::from(image.height) >= display_resolution.1);

                // Safe since `addr` points to data mapped onto our address space since we called
                // `vaMapBuffer` above, which also guarantees that the data is valid for
                // `image.data_size`.
                let data =
                    unsafe { std::slice::from_raw_parts_mut(addr as _, image.data_size as usize) };
                Ok(Image {
                    display: Arc::clone(surface.display()),
                    image,
                    data,
                    derived,
                    display_resolution,
                    dirty: false,
                    surface_id: surface.id(),
                })
            }
            Err(e) => {
                // Safe because `picture.inner.context` represents a valid `VAContext` and `image`
                // represents a valid `VAImage`.
                unsafe {
                    bindings::vaDestroyImage(surface.display().handle(), image.image_id);
                }

                Err(e)
            }
        }
    }

    /// Create a new derived image from this `surface` using `vaDeriveImage`.
    ///
    /// Derived images are a direct view (i.e. without any copy) on the buffer content of
    /// `surface`. On the other hand, not all `Surface`s can be derived.
    ///
    /// `visible_rect` is the visible rectangle inside `surface` that we want to access.
    pub fn derive_from<D: SurfaceMemoryDescriptor>(
        surface: &'a Surface<D>,
        visible_rect: (u32, u32),
    ) -> Result<Self, VaError> {
        // An all-zero byte-pattern is a valid initial value for `VAImage`.
        let mut image: bindings::VAImage = Default::default();

        // Safe because `self` has a valid display handle and ID.
        va_check(unsafe {
            bindings::vaDeriveImage(surface.display().handle(), surface.id(), &mut image)
        })?;

        Self::new(surface, image, true, visible_rect)
    }

    /// Create new image from `surface` using `vaCreateImage` and `vaGetImage`.
    ///
    /// `visible_rect` is the visible rectangle inside `surface` that we want to access.
    ///
    /// The image will contain a copy of `surface`'s data' in the desired `format` and
    /// `coded_resolution`, meaning the data can be scaled if `coded_resolution` and `visible_rect`
    /// differ.
    pub fn create_from<D: SurfaceMemoryDescriptor>(
        surface: &'a Surface<D>,
        mut format: bindings::VAImageFormat,
        coded_resolution: (u32, u32),
        visible_rect: (u32, u32),
    ) -> Result<Image<'a>, VaError> {
        // An all-zero byte-pattern is a valid initial value for `VAImage`.
        let mut image: bindings::VAImage = Default::default();
        let dpy = surface.display().handle();

        // Safe because `dpy` is a valid display handle.
        va_check(unsafe {
            bindings::vaCreateImage(
                dpy,
                &mut format,
                coded_resolution.0 as i32,
                coded_resolution.1 as i32,
                &mut image,
            )
        })?;

        // Safe because `dpy` is a valid display handle, `picture.surface` is a valid VASurface and
        // `image` is a valid `VAImage`.
        match va_check(unsafe {
            bindings::vaGetImage(
                dpy,
                surface.id(),
                0,
                0,
                coded_resolution.0,
                coded_resolution.1,
                image.image_id,
            )
        }) {
            Ok(()) => Self::new(surface, image, false, visible_rect),

            Err(e) => {
                // Safe because `image` is a valid `VAImage`.
                unsafe {
                    bindings::vaDestroyImage(dpy, image.image_id);
                }

                Err(e)
            }
        }
    }

    /// Get a reference to the underlying `VAImage` that describes this image.
    pub fn image(&self) -> &bindings::VAImage {
        &self.image
    }

    /// Returns whether this image is directly derived from its underlying `Picture`, as opposed to
    /// being a view/copy of said `Picture` in a guaranteed pixel format.
    pub fn is_derived(&self) -> bool {
        self.derived
    }

    /// Returns the display resolution as passed in by the client. This is a
    /// value that is less than or equal to the coded resolution.
    pub fn display_resolution(&self) -> (u32, u32) {
        self.display_resolution
    }

    /// Returns the coded resolution. This value can be larger than the value
    /// passed in when the image was created if the driver needs to.
    pub fn coded_resolution(&self) -> (u32, u32) {
        (self.image.width.into(), self.image.height.into())
    }
}

impl<'a> AsRef<[u8]> for Image<'a> {
    fn as_ref(&self) -> &[u8] {
        self.data
    }
}

impl<'a> AsMut<[u8]> for Image<'a> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.dirty = true;
        self.data
    }
}

impl<'a> Drop for Image<'a> {
    fn drop(&mut self) {
        if !self.derived && self.dirty {
            // Safe because `picture.inner.context` represents a valid `VAContext`,
            // `picture.surface` represents a valid `VASurface` and `image` represents a valid
            // `VAImage`.
            unsafe {
                bindings::vaPutImage(
                    self.display.handle(),
                    self.surface_id,
                    self.image.image_id,
                    0,
                    0,
                    self.image.width as u32,
                    self.image.height as u32,
                    0,
                    0,
                    self.image.width as u32,
                    self.image.height as u32,
                );
            }
        }

        unsafe {
            // Safe since the buffer is mapped in `Image::new`, so `self.image.buf` points to a
            // valid `VABufferID`.
            bindings::vaUnmapBuffer(self.display.handle(), self.image.buf);
            // Safe since `self.image` represents a valid `VAImage`.
            bindings::vaDestroyImage(self.display.handle(), self.image.image_id);
        }
    }
}

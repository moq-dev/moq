// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::any::Any;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::raw::c_void;
use std::sync::Arc;

use crate::bindings;
use crate::display::Display;
use crate::va_check;
use crate::UsageHint;
use crate::VASurfaceID;
use crate::VaError;

/// Trait describing a memory backing for surfaces.
///
/// Using external memory for backing a VA surface is done in two steps:
///
/// 1. Build the descriptor specific to the memory type we want to use,
/// 2. Mention this descriptor as an attribute to be passed to `vaDeriveImage`.
///
/// This trait allows to do that in a as-safe-as-possible way, by adding the required attributes
/// and returning a heap-allocated object containing the data pointed to by the attributes. That
/// way, the caller can keep that object alive until `vaCreateSurfaces` has returned.
pub trait SurfaceMemoryDescriptor {
    /// Add the required attributes to `attr` in order to attach the memory of this descriptor to
    /// the surface when it is created.
    ///
    /// The returned object, if any, is the descriptor pointed to by the attributes. The caller
    /// must keep it around and unmoved until `vaCreateSurfaces` has returned.
    fn add_attrs(&mut self, attrs: &mut Vec<bindings::VASurfaceAttrib>) -> Option<Box<dyn Any>>;
}

/// VA memory types, aka `VA_SURFACE_ATTRIB_MEM_TYPE_*`.
#[repr(u32)]
pub enum MemoryType {
    Va = bindings::VA_SURFACE_ATTRIB_MEM_TYPE_VA,
    V4L2 = bindings::VA_SURFACE_ATTRIB_MEM_TYPE_V4L2,
    UserPtr = bindings::VA_SURFACE_ATTRIB_MEM_TYPE_USER_PTR,
    DrmPrime2 = bindings::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
}

/// Used when we want the VA driver to allocate surface memory for us. In this case we don't need
/// to add any specific attribute for surface creation.
impl SurfaceMemoryDescriptor for () {
    fn add_attrs(&mut self, _: &mut Vec<bindings::VASurfaceAttrib>) -> Option<Box<dyn Any>> {
        None
    }
}

/// Sealed trait pattern to avoid reimplementation of our local traits.
mod private {
    pub trait Sealed {}
}

/// Trait for types that can be used as a `VASurfaceAttribExternalBufferDescriptor`.
pub trait SurfaceExternalDescriptor: private::Sealed {}
impl private::Sealed for bindings::VASurfaceAttribExternalBuffers {}
impl SurfaceExternalDescriptor for bindings::VASurfaceAttribExternalBuffers {}
impl private::Sealed for bindings::VADRMPRIMESurfaceDescriptor {}
impl SurfaceExternalDescriptor for bindings::VADRMPRIMESurfaceDescriptor {}

/// Trait allowing to import an external memory source to use with a surface by setting the
/// `VASurfaceAttribMemoryType` and `VASurfaceAttribExternalBuffers` attributes.
pub trait ExternalBufferDescriptor {
    /// Memory type to set for `VASurfaceAttribMemoryType`.
    const MEMORY_TYPE: MemoryType;
    /// Type of the descriptor to be set with `VASurfaceAttribExternalBuffers`.
    type DescriptorAttribute: SurfaceExternalDescriptor;

    /// Returns the `Self::DescriptorAttribute` instance allowing this memory to be imported
    /// into VAAPI.
    fn va_surface_attribute(&mut self) -> Self::DescriptorAttribute;
}

impl<T> SurfaceMemoryDescriptor for T
where
    T: ExternalBufferDescriptor,
    <T as ExternalBufferDescriptor>::DescriptorAttribute: 'static,
{
    fn add_attrs(&mut self, attrs: &mut Vec<bindings::VASurfaceAttrib>) -> Option<Box<dyn Any>> {
        let mut desc = Box::new(self.va_surface_attribute());

        attrs.push(bindings::VASurfaceAttrib::new_memory_type(
            Self::MEMORY_TYPE,
        ));
        attrs.push(bindings::VASurfaceAttrib::new_buffer_descriptor(
            desc.as_mut(),
        ));

        Some(desc)
    }
}

/// Decode error type aka `VADecodeErrorType`
#[repr(u32)]
#[derive(Debug)]
pub enum DecodeErrorType {
    SliceMissing = bindings::VADecodeErrorType::VADecodeSliceMissing,
    MBError = bindings::VADecodeErrorType::VADecodeMBError,
    #[cfg(libva_1_20_or_higher)]
    Reset = bindings::VADecodeErrorType::VADecodeReset,
}

/// Decode error details extracted from `VASurfaceDecodeMBErrors`, result of vaQuerySurfaceError.
#[derive(Debug)]
pub struct SurfaceDecodeMBError {
    /// Start mb address with errors
    pub start_mb: u32,

    /// End mb address with errors
    pub end_mb: u32,

    pub decode_error_type: DecodeErrorType,

    /// Number of mbs with errors
    pub num_mb: u32,
}

/// An owned VA surface that is tied to a particular `Display`.
pub struct Surface<D: SurfaceMemoryDescriptor> {
    display: Arc<Display>,
    id: bindings::VASurfaceID,
    descriptor: D,
    width: u32,
    height: u32,
}

impl From<i32> for bindings::VAGenericValue {
    fn from(i: i32) -> Self {
        Self {
            type_: bindings::VAGenericValueType::VAGenericValueTypeInteger,
            value: bindings::_VAGenericValue__bindgen_ty_1 { i },
        }
    }
}

impl From<f32> for bindings::VAGenericValue {
    fn from(f: f32) -> Self {
        Self {
            type_: bindings::VAGenericValueType::VAGenericValueTypeFloat,
            value: bindings::_VAGenericValue__bindgen_ty_1 { f },
        }
    }
}

impl From<*mut c_void> for bindings::VAGenericValue {
    fn from(p: *mut c_void) -> Self {
        Self {
            type_: bindings::VAGenericValueType::VAGenericValueTypePointer,
            value: bindings::_VAGenericValue__bindgen_ty_1 { p },
        }
    }
}

/// Helpers to build valid `VASurfaceAttrib`s.
impl bindings::VASurfaceAttrib {
    pub fn new_pixel_format(fourcc: u32) -> Self {
        Self {
            type_: bindings::VASurfaceAttribType::VASurfaceAttribPixelFormat,
            flags: bindings::VA_SURFACE_ATTRIB_SETTABLE,
            value: bindings::VAGenericValue::from(fourcc as i32),
        }
    }

    pub fn new_usage_hint(usage_hint: UsageHint) -> Self {
        Self {
            type_: bindings::VASurfaceAttribType::VASurfaceAttribUsageHint,
            flags: bindings::VA_SURFACE_ATTRIB_SETTABLE,
            value: bindings::VAGenericValue::from(usage_hint.bits() as i32),
        }
    }

    pub fn new_memory_type(mem_type: MemoryType) -> Self {
        Self {
            type_: bindings::VASurfaceAttribType::VASurfaceAttribMemoryType,
            flags: bindings::VA_SURFACE_ATTRIB_SETTABLE,
            value: bindings::VAGenericValue::from(mem_type as i32),
        }
    }

    pub fn new_buffer_descriptor<T: SurfaceExternalDescriptor>(desc: &mut T) -> Self {
        Self {
            type_: bindings::VASurfaceAttribType::VASurfaceAttribExternalBufferDescriptor,
            flags: bindings::VA_SURFACE_ATTRIB_SETTABLE,
            value: bindings::VAGenericValue::from(desc as *mut _ as *mut c_void),
        }
    }
}

impl<D: SurfaceMemoryDescriptor> Surface<D> {
    /// Create `Surfaces` by wrapping around a `vaCreateSurfaces` call. This is just a helper for
    /// [`Display::create_surfaces`].
    pub(crate) fn new(
        display: Arc<Display>,
        rt_format: u32,
        va_fourcc: Option<u32>,
        width: u32,
        height: u32,
        usage_hint: Option<UsageHint>,
        descriptors: Vec<D>,
    ) -> Result<Vec<Self>, VaError> {
        let mut surfaces = vec![];

        for mut descriptor in descriptors {
            let mut attrs = vec![];

            if let Some(usage_hint) = usage_hint {
                attrs.push(bindings::VASurfaceAttrib::new_usage_hint(usage_hint));
            }

            if let Some(fourcc) = va_fourcc {
                attrs.push(bindings::VASurfaceAttrib::new_pixel_format(fourcc));
            }

            // Just to be kept alive until we call `vaCreateSurfaces`...
            let mut _va_desc = descriptor.add_attrs(&mut attrs);
            let mut surface_id: VASurfaceID = 0;

            // Safe because `self` represents a valid VADisplay. The `surface` and `attrs` vectors are
            // properly initialized and valid sizes are passed to the C function, so it is impossible to
            // write past the end of their storage by mistake.
            //
            // Also all the pointers in `attrs` are pointing to valid objects that haven't been
            // moved or destroyed.
            match va_check(unsafe {
                bindings::vaCreateSurfaces(
                    display.handle(),
                    rt_format,
                    width,
                    height,
                    &mut surface_id,
                    1,
                    attrs.as_mut_ptr(),
                    attrs.len() as u32,
                )
            }) {
                Ok(()) => surfaces.push(Self {
                    display: Arc::clone(&display),
                    id: surface_id,
                    descriptor,
                    width,
                    height,
                }),
                Err(e) => return Err(e),
            }
        }

        Ok(surfaces)
    }

    pub fn display(&self) -> &Arc<Display> {
        &self.display
    }

    /// Wrapper around `vaSyncSurface` that blocks until all pending operations on the render
    /// target have been completed.
    ///
    /// Upon return it
    /// is safe to use the render target for a different picture.
    pub fn sync(&self) -> Result<(), VaError> {
        // Safe because `self` represents a valid VASurface.
        va_check(unsafe { bindings::vaSyncSurface(self.display.handle(), self.id) })
    }

    /// Convenience function to return a VASurfaceID vector. Useful to interface with the C API
    /// where a surface array might be needed.
    pub fn as_id_vec(surfaces: &[Self]) -> Vec<bindings::VASurfaceID> {
        surfaces.iter().map(|surface| surface.id).collect()
    }

    /// Wrapper over `vaQuerySurfaceStatus` to find out any pending ops on the render target.
    pub fn query_status(&self) -> Result<bindings::VASurfaceStatus::Type, VaError> {
        let mut status: bindings::VASurfaceStatus::Type = 0;
        // Safe because `self` represents a valid VASurface.
        va_check(unsafe {
            bindings::vaQuerySurfaceStatus(self.display.handle(), self.id, &mut status)
        })?;

        Ok(status)
    }

    pub fn query_error(&self) -> Result<Vec<SurfaceDecodeMBError>, VaError> {
        let mut raw: *const bindings::VASurfaceDecodeMBErrors = std::ptr::null();

        // Safe because `self` represents a valid VASurface.
        va_check(unsafe {
            bindings::vaQuerySurfaceError(
                self.display.handle(),
                self.id,
                bindings::VA_STATUS_ERROR_DECODING_ERROR as i32,
                (&mut raw) as *mut _ as *mut _,
            )
        })?;

        let mut errors = vec![];

        while !raw.is_null() {
            // Safe because raw is a valid pointer
            let error = unsafe { *raw };
            if error.status == -1 {
                break;
            }

            let type_ = match error.decode_error_type {
                bindings::VADecodeErrorType::VADecodeSliceMissing => DecodeErrorType::SliceMissing,
                bindings::VADecodeErrorType::VADecodeMBError => DecodeErrorType::MBError,
                #[cfg(libva_1_20_or_higher)]
                bindings::VADecodeErrorType::VADecodeReset => DecodeErrorType::Reset,
                _ => {
                    log::warn!(
                        "Unrecognized `decode_error_type` value ({})",
                        error.decode_error_type
                    );

                    // Safe because status != -1
                    raw = unsafe { raw.offset(1) };
                    continue;
                }
            };

            errors.push(SurfaceDecodeMBError {
                start_mb: error.start_mb,
                end_mb: error.end_mb,
                decode_error_type: type_,
                num_mb: error.num_mb,
            });

            // Safe because status != -1
            raw = unsafe { raw.offset(1) };
        }

        Ok(errors)
    }

    /// Returns the ID of this surface.
    pub fn id(&self) -> bindings::VASurfaceID {
        self.id
    }

    /// Returns the dimensions of this surface.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Returns a PRIME descriptor for this surface.
    pub fn export_prime(&self) -> Result<DrmPrimeSurfaceDescriptor, VaError> {
        let mut desc: bindings::VADRMPRIMESurfaceDescriptor = Default::default();

        va_check(unsafe {
            bindings::vaExportSurfaceHandle(
                self.display.handle(),
                self.id(),
                bindings::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                bindings::VA_EXPORT_SURFACE_READ_ONLY | bindings::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                &mut desc as *mut _ as *mut c_void,
            )
        })?;

        // We do not use a `From<VADRMPRIMESurfaceDescriptor>` implementation as this would allow
        // to create "safe" descriptors outside of this method and thus from made up values,
        // violating the safety guarantee that our FDs are legit.

        let objects = (0..desc.num_objects as usize)
            // Make sure we don't go out of bounds.
            .take(4)
            .map(|i| desc.objects[i])
            .map(|o| {
                DrmPrimeSurfaceDescriptorObject {
                    // Safe because `o.fd` is a valid file descriptor returned by
                    // `vaExportSurfaceHandle`.
                    fd: unsafe { OwnedFd::from_raw_fd(o.fd) },
                    size: o.size,
                    drm_format_modifier: o.drm_format_modifier,
                }
            })
            .collect();

        let layers = (0..desc.num_layers as usize)
            // Make sure we don't go out of bounds.
            .take(4)
            .map(|i| desc.layers[i])
            .map(|l| DrmPrimeSurfaceDescriptorLayer {
                drm_format: l.drm_format,
                num_planes: l.num_planes,
                object_index: [
                    l.object_index[0] as u8,
                    l.object_index[1] as u8,
                    l.object_index[2] as u8,
                    l.object_index[3] as u8,
                ],
                offset: l.offset,
                pitch: l.pitch,
            })
            .collect();

        Ok(DrmPrimeSurfaceDescriptor {
            fourcc: desc.fourcc,
            width: desc.width,
            height: desc.height,
            objects,
            layers,
        })
    }
}

impl<D: SurfaceMemoryDescriptor> AsRef<D> for Surface<D> {
    fn as_ref(&self) -> &D {
        &self.descriptor
    }
}

impl<D: SurfaceMemoryDescriptor> AsMut<D> for Surface<D> {
    fn as_mut(&mut self) -> &mut D {
        &mut self.descriptor
    }
}

impl<D: SurfaceMemoryDescriptor> Drop for Surface<D> {
    fn drop(&mut self) {
        // Safe because `self` represents a valid VASurface.
        unsafe { bindings::vaDestroySurfaces(self.display.handle(), &mut self.id, 1) };
    }
}

/// Safe wrapper for the `object` member of `VADRMPRIMESurfaceDescriptor`.
pub struct DrmPrimeSurfaceDescriptorObject {
    pub fd: OwnedFd,
    pub size: u32,
    pub drm_format_modifier: u64,
}

/// Safe wrapper for the `layers` member of `VADRMPRIMESurfaceDescriptor`.
pub struct DrmPrimeSurfaceDescriptorLayer {
    pub drm_format: u32,
    pub num_planes: u32,
    pub object_index: [u8; 4],
    pub offset: [u32; 4],
    pub pitch: [u32; 4],
}

/// Safe wrapper around `VADRMPRIMESurfaceDescriptor`.
pub struct DrmPrimeSurfaceDescriptor {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub objects: Vec<DrmPrimeSurfaceDescriptorObject>,
    pub layers: Vec<DrmPrimeSurfaceDescriptorLayer>,
}

// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Borrow;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::bindings;
use crate::buffer::Buffer;
use crate::context::Context;
use crate::surface::Surface;
use crate::va_check;
use crate::Image;
use crate::SurfaceMemoryDescriptor;
use crate::VaError;

// Use the sealed trait pattern to make sure that new states are not created in caller code. More
// information about the sealed trait pattern can be found at
// <https://rust-lang.github.io/api-guidelines/future-proofing.html#sealed-traits-protect-against-downstream-implementations-c-sealed>
mod private {
    pub trait Sealed {}
}

/// A `Picture` will only have valid YUV data after a sequence of operations are performed in a
/// particular order. This order correspond to the following VA-API calls: `vaBeginPicture`,
/// `vaRenderPicture`, `vaEndPicture` and `vaSyncSurface`. This trait enforces this ordering by
/// implementing the Typestate pattern to constrain what operations are available in what particular
/// states.
///
/// The states for the state machine are:
///
/// * PictureNew -> PictureBegin
/// * PictureBegin -> PictureRender
/// * PictureRender ->PictureEnd
/// * PictureEnd -> PictureSync
///
/// Where the surface can be reclaimed in both `PictureNew` and `PictureSync`, as either no
/// operation took place (as in `PictureNew`), or it is guaranteed that the operation has already
/// completed (as in `PictureSync`).
///
/// More information about the Typestate pattern can be found at
/// <http://cliffle.com/blog/rust-typestate/>
pub trait PictureState: private::Sealed {}

/// Represents a `Picture` that has just been created.
pub enum PictureNew {}
impl PictureState for PictureNew {}
impl private::Sealed for PictureNew {}

/// Represents a `Picture` after `vaBeginPicture` has been called.
pub enum PictureBegin {}
impl PictureState for PictureBegin {}
impl private::Sealed for PictureBegin {}

/// Represents a `Picture` after `vaRenderPicture` has been called.
pub enum PictureRender {}
impl PictureState for PictureRender {}
impl private::Sealed for PictureRender {}

/// Represents a `Picture` after `vaEndPicture` has been called.
pub enum PictureEnd {}
impl PictureState for PictureEnd {}
impl private::Sealed for PictureEnd {}

/// Represents a `Picture` after `vaSyncSurface` has been called on the underlying surface.
pub enum PictureSync {}
impl PictureState for PictureSync {}
impl private::Sealed for PictureSync {}

/// Represents a state where one can reclaim the underlying `Surface` for this `Picture`. This is
/// true when either no decoding has been initiated or, alternatively, when the decoding operation
/// has completed for the underlying `vaSurface`
pub trait PictureReclaimableSurface: PictureState + private::Sealed {}
impl PictureReclaimableSurface for PictureNew {}
impl PictureReclaimableSurface for PictureSync {}

/// Inner type for [`Picture`], that is, the part that exists in all states.
struct PictureInner<T> {
    /// Timestamp of the picture.
    timestamp: u64,
    /// A context associated with this picture.
    context: Rc<Context>,
    /// Contains the buffers used to decode the data.
    buffers: Vec<Buffer>,
    /// Contains the actual decoded data. Note that the surface may be shared in
    /// interlaced decoding.
    surface: Rc<T>,
}

/// A `Surface` that is being rendered into.
///
/// This struct abstracts the decoding flow using `vaBeginPicture`, `vaRenderPicture`,
/// `vaEndPicture` and `vaSyncSurface` in a type-safe way.
///
/// The surface will have valid picture data after all the stages of decoding are called.
///
/// The `T` generic parameter must be `Borrow<Surface<_>>`, i.e. it can be [`Surface`] directly or
/// some other type that contains one.
///
/// No constraint on `T` is specified in this declaration because specifying it here would force us
/// to add the generic argument of [`Surface`] to this type as well, turning it into a type with 3
/// generics, one of which is redundant. To avoid that we leave `T` unconstrained and instead
/// constrain the methods that require to act on it as a [`Surface`].
pub struct Picture<S: PictureState, T> {
    inner: Box<PictureInner<T>>,
    phantom: std::marker::PhantomData<S>,
}

impl<T> Picture<PictureNew, T> {
    /// Creates a new Picture with a given `timestamp`. `surface` is the underlying surface that
    /// libva will render to.
    pub fn new<D: SurfaceMemoryDescriptor>(timestamp: u64, context: Rc<Context>, surface: T) -> Self
    where
        T: Borrow<Surface<D>>,
    {
        Self {
            inner: Box::new(PictureInner {
                timestamp,
                context,
                buffers: Default::default(),
                surface: Rc::new(surface),
            }),

            phantom: PhantomData,
        }
    }

    /// Creates a new Picture with a given `timestamp` to identify it,
    /// reusing the Surface from `picture`. This is useful for interlaced
    /// decoding as one can render both fields to the same underlying surface.
    pub fn new_from_same_surface<S: PictureState>(timestamp: u64, picture: &Picture<S, T>) -> Self {
        let context = Rc::clone(&picture.inner.context);
        Picture {
            inner: Box::new(PictureInner {
                timestamp,
                context,
                buffers: Default::default(),
                surface: Rc::clone(&picture.inner.surface),
            }),

            phantom: PhantomData,
        }
    }

    /// Add `buffer` to the picture.
    pub fn add_buffer(&mut self, buffer: Buffer) {
        self.inner.buffers.push(buffer);
    }

    /// Wrapper around `vaBeginPicture`.
    pub fn begin<D: SurfaceMemoryDescriptor>(self) -> Result<Picture<PictureBegin, T>, VaError>
    where
        T: Borrow<Surface<D>>,
    {
        // Safe because `self.inner.context` represents a valid VAContext and
        // `self.inner.surface` represents a valid VASurface.
        let res = va_check(unsafe {
            bindings::vaBeginPicture(
                self.inner.context.display().handle(),
                self.inner.context.id(),
                self.surface().id(),
            )
        });

        res.map(|()| Picture {
            inner: self.inner,
            phantom: PhantomData,
        })
    }
}

impl<T> Picture<PictureBegin, T> {
    /// Wrapper around `vaRenderPicture`.
    pub fn render(self) -> Result<Picture<PictureRender, T>, VaError> {
        // Safe because `self.inner.context` represents a valid `VAContext` and `self.inner.surface`
        // represents a valid `VASurface`. `buffers` point to a Rust struct and the vector length is
        // passed to the C function, so it is impossible to write past the end of the vector's
        // storage by mistake.
        va_check(unsafe {
            bindings::vaRenderPicture(
                self.inner.context.display().handle(),
                self.inner.context.id(),
                Buffer::as_id_vec(&self.inner.buffers).as_mut_ptr(),
                self.inner.buffers.len() as i32,
            )
        })
        .map(|()| Picture {
            inner: self.inner,
            phantom: PhantomData,
        })
    }
}

impl<T> Picture<PictureRender, T> {
    /// Wrapper around `vaEndPicture`.
    pub fn end(self) -> Result<Picture<PictureEnd, T>, VaError> {
        // Safe because `self.inner.context` represents a valid `VAContext`.
        va_check(unsafe {
            bindings::vaEndPicture(
                self.inner.context.display().handle(),
                self.inner.context.id(),
            )
        })
        .map(|()| Picture {
            inner: self.inner,
            phantom: PhantomData,
        })
    }
}

impl<T> Picture<PictureEnd, T> {
    /// Syncs the picture, ensuring that all pending operations are complete when this call returns.
    pub fn sync<D: SurfaceMemoryDescriptor>(
        self,
    ) -> Result<Picture<PictureSync, T>, (VaError, Self)>
    where
        T: Borrow<Surface<D>>,
    {
        let res = self.surface().sync();

        match res {
            Ok(()) => Ok(Picture {
                inner: self.inner,
                phantom: PhantomData,
            }),
            Err(e) => Err((e, self)),
        }
    }
}

impl<S: PictureState, T> Picture<S, T> {
    /// Returns the timestamp of this picture.
    pub fn timestamp(&self) -> u64 {
        self.inner.timestamp
    }

    /// Returns a reference to the underlying `Surface`.
    ///
    /// If you are interested in obtaining the container of the `Surface`, use `as_ref()` instead.
    /// This is a convenience method to avoid having to call `borrow()` every time the surface is
    /// needed.
    pub fn surface<D: SurfaceMemoryDescriptor>(&self) -> &Surface<D>
    where
        T: Borrow<Surface<D>>,
    {
        self.as_ref().borrow()
    }
}

impl<S: PictureReclaimableSurface, T> Picture<S, T> {
    /// Reclaim ownership of the Surface this picture has been created from, consuming the picture
    /// in the process. Useful if the Surface is part of a pool.
    ///
    /// This will fail and return the passed object if there are more than one reference to the
    /// underlying surface.
    pub fn take_surface(self) -> Result<T, Self> {
        let inner = self.inner;
        match Rc::try_unwrap(inner.surface) {
            Ok(surface) => Ok(surface),
            Err(surface) => Err(Self {
                inner: Box::new(PictureInner {
                    surface,
                    context: inner.context,
                    buffers: inner.buffers,
                    timestamp: inner.timestamp,
                }),
                phantom: PhantomData,
            }),
        }
    }

    /// Create a new derived image from this `Picture` using `vaDeriveImage`.
    ///
    /// Derived images are a direct view (i.e. without any copy) on the buffer content of the
    /// `Picture`. On the other hand, not all `Pictures` can be derived.
    pub fn derive_image<'a, D: SurfaceMemoryDescriptor + 'a>(
        &'a self,
        visible_rect: (u32, u32),
    ) -> Result<Image<'a>, VaError>
    where
        T: Borrow<Surface<D>>,
    {
        Image::derive_from(self.surface(), visible_rect)
    }

    /// Create new image from the `Picture` using `vaCreateImage` and `vaGetImage`.
    ///
    /// The image will contain a copy of the `Picture` in the desired `format` and `coded_resolution`.
    pub fn create_image<'a, D: SurfaceMemoryDescriptor + 'a>(
        &'a self,
        format: bindings::VAImageFormat,
        coded_resolution: (u32, u32),
        visible_rect: (u32, u32),
    ) -> Result<Image<'a>, VaError>
    where
        T: Borrow<Surface<D>>,
    {
        Image::create_from(self.surface(), format, coded_resolution, visible_rect)
    }
}

impl<S: PictureState, T> AsRef<T> for Picture<S, T> {
    fn as_ref(&self) -> &T {
        (*self.inner.surface).borrow()
    }
}

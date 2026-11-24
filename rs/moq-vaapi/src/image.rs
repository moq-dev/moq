// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::bindings;
use crate::va_check;
use crate::Display;
use crate::Surface;
use crate::SurfaceMemoryDescriptor;
use crate::VaError;

/// A `VAImage` libva created, destroyed on drop.
struct Created {
	display: Arc<Display>,
	image: bindings::VAImage,
}

impl Drop for Created {
	fn drop(&mut self) {
		// Safe since `self.image` is a valid `VAImage` of `self.display`.
		unsafe {
			bindings::va().vaDestroyImage(self.display.handle(), self.image.image_id);
		}
	}
}

/// A [`Created`] image whose buffer is mapped into our address space, unmapped
/// on drop before the image is destroyed.
struct Mapping {
	created: Created,
	data: *mut u8,
	/// Cleared once the buffer is unmapped, so drop never unmaps it twice.
	mapped: bool,
}

// Safe since `data` is plain client memory owned by the mapping, and the libva calls
// that release it go through `Display`, which is thread-safe. `Image` reads it only
// through `&self` and `ImageMut` writes it only through `&mut self`, so the borrow
// rules still exclude a data race.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
	/// Maps `created`'s buffer, destroying the image if that fails.
	fn new(created: Created, display_resolution: (u32, u32)) -> Result<Self, VaError> {
		let mut addr = std::ptr::null_mut();
		// Safe since `created.image` is a valid `VAImage` of `created.display`.
		va_check(unsafe { bindings::va().vaMapBuffer(created.display.handle(), created.image.buf, &mut addr) })?;
		let mapping = Self {
			created,
			data: addr.cast(),
			mapped: true,
		};

		// The guard unmaps and destroys the image when the driver hands back a
		// mapping we cannot use.
		let image = &mapping.created.image;
		if mapping.data.is_null()
			|| u32::from(image.width) < display_resolution.0
			|| u32::from(image.height) < display_resolution.1
		{
			return Err(VaError::status(bindings::VA_STATUS_ERROR_INVALID_IMAGE));
		}
		Ok(mapping)
	}

	fn image(&self) -> &bindings::VAImage {
		&self.created.image
	}

	fn len(&self) -> usize {
		self.created.image.data_size as usize
	}

	/// Unmaps the buffer, leaving the image to be destroyed on drop.
	fn unmap(&mut self) -> Result<(), VaError> {
		self.mapped = false;
		// Safe since the buffer was mapped in `new` and not unmapped since.
		va_check(unsafe { bindings::va().vaUnmapBuffer(self.created.display.handle(), self.created.image.buf) })
	}
}

impl Drop for Mapping {
	fn drop(&mut self) {
		if self.mapped {
			// Nothing to report a failure to; the image is destroyed regardless.
			let _ = self.unmap();
		}
	}
}

/// Creates a `VAImage` in `format` at `coded_resolution`.
fn create(
	display: &Arc<Display>,
	mut format: bindings::VAImageFormat,
	coded_resolution: (u32, u32),
) -> Result<Created, VaError> {
	// An all-zero byte-pattern is a valid initial value for `VAImage`.
	let mut image: bindings::VAImage = Default::default();
	// Safe because `display` is a valid display handle.
	va_check(unsafe {
		bindings::va().vaCreateImage(
			display.handle(),
			&mut format,
			coded_resolution.0 as i32,
			coded_resolution.1 as i32,
			&mut image,
		)
	})?;
	Ok(Created {
		display: Arc::clone(display),
		image,
	})
}

/// A read-only view of a surface's pixels in client memory, borrowing the surface.
///
/// For writing pixels into a surface, see [`ImageMut`].
pub struct Image<'a> {
	mapping: Mapping,
	/// Whether the image was derived using the `vaDeriveImage` API or created using the
	/// `vaCreateImage` API.
	derived: bool,
	/// The display resolution requested by the client. The implementation is
	/// free to enlarge this value as needed. In any case, we guarantee that an
	/// image at least as large is returned.
	display_resolution: (u32, u32),
	/// Keeps the surface from being dropped or written through an [`ImageMut`] while we exist.
	surface: PhantomData<&'a ()>,
}

impl<'a> Image<'a> {
	fn new(created: Created, derived: bool, display_resolution: (u32, u32)) -> Result<Self, VaError> {
		Ok(Image {
			mapping: Mapping::new(created, display_resolution)?,
			derived,
			display_resolution,
			surface: PhantomData,
		})
	}

	/// Create a new derived image from this `surface` using `vaDeriveImage`.
	///
	/// Derived images are a direct view (i.e. without any copy) on the buffer content of
	/// `surface`. On the other hand, not all `Surface`s can be derived.
	///
	/// `visible_rect` is the visible rectangle inside `surface` that we want to access.
	///
	/// The view is the surface's own memory, so it borrows the surface mutably:
	/// nothing, not even a GPU operation taking `&Surface`, can write it while the
	/// view is alive. It first waits for any write still in flight.
	pub fn derive_from<D: SurfaceMemoryDescriptor>(
		surface: &'a mut Surface<D>,
		visible_rect: (u32, u32),
	) -> Result<Self, VaError> {
		surface.sync()?;
		// Safe since the mutable borrow excludes every writer for `'a`, and the
		// sync finished any that came before it.
		unsafe { Self::derive_shared(surface, visible_rect) }
	}

	/// [`derive_from`](Self::derive_from) through a shared borrow.
	///
	/// # Safety
	///
	/// Nothing may write `surface`, through VA-API or otherwise, while the image
	/// lives, nor still be writing it, such as a synced decoded picture the pool
	/// hands out again only once every reference to it has dropped.
	pub(crate) unsafe fn derive_shared<D: SurfaceMemoryDescriptor>(
		surface: &'a Surface<D>,
		visible_rect: (u32, u32),
	) -> Result<Self, VaError> {
		// An all-zero byte-pattern is a valid initial value for `VAImage`.
		let mut image: bindings::VAImage = Default::default();

		// Safe because `surface` has a valid display handle and ID.
		va_check(unsafe { bindings::va().vaDeriveImage(surface.display().handle(), surface.id(), &mut image) })?;
		let created = Created {
			display: Arc::clone(surface.display()),
			image,
		};

		Self::new(created, true, visible_rect)
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
		format: bindings::VAImageFormat,
		coded_resolution: (u32, u32),
		visible_rect: (u32, u32),
	) -> Result<Image<'a>, VaError> {
		let created = create(surface.display(), format, coded_resolution)?;

		// Safe because the display handle is valid, `surface` is a valid VASurface and
		// `created.image` is a valid `VAImage`.
		va_check(unsafe {
			bindings::va().vaGetImage(
				surface.display().handle(),
				surface.id(),
				0,
				0,
				coded_resolution.0,
				coded_resolution.1,
				created.image.image_id,
			)
		})?;

		Self::new(created, false, visible_rect)
	}

	/// Get a reference to the underlying `VAImage` that describes this image.
	pub fn image(&self) -> &bindings::VAImage {
		self.mapping.image()
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
		(self.image().width.into(), self.image().height.into())
	}
}

impl AsRef<[u8]> for Image<'_> {
	fn as_ref(&self) -> &[u8] {
		// Safe since the buffer stays mapped for `data_size` bytes while `self` lives, and
		// nothing writes it: a created image is our own copy, and a derived one excludes
		// every writer of the surface (see `derive_from`).
		unsafe { std::slice::from_raw_parts(self.mapping.data, self.mapping.len()) }
	}
}

/// Pixels to write into a surface, borrowing it mutably until [`put`](Self::put)
/// copies them in.
///
/// Dropped without `put`, the image is discarded and the surface left as it was.
///
/// No other image of the surface, read or write, can exist alongside it:
///
/// ```compile_fail,E0502
/// fn alias(surface: &mut moq_vaapi::Surface<()>, format: moq_vaapi::VAImageFormat) {
/// 	let read = moq_vaapi::Image::create_from(surface, format, surface.size(), surface.size()).unwrap();
/// 	let write = moq_vaapi::ImageMut::create_uninitialized(surface, format).unwrap();
/// 	drop((read, write));
/// }
/// ```
///
/// Nor can a derived image share its surface with anything that writes it, a
/// GPU operation through `&Surface` included:
///
/// ```compile_fail,E0502
/// fn alias(surface: &mut moq_vaapi::Surface<()>) {
/// 	let size = surface.size();
/// 	let view = moq_vaapi::Image::derive_from(surface, size).unwrap();
/// 	let shared: &moq_vaapi::Surface<()> = surface;
/// 	drop((view, shared));
/// }
/// ```
///
/// An image unmaps and destroys itself only before its surface is destroyed:
///
/// ```compile_fail,E0505
/// fn destroy_first(mut surface: moq_vaapi::Surface<()>) {
/// 	let size = surface.size();
/// 	let _view = moq_vaapi::Image::derive_from(&mut surface, size).unwrap();
/// 	drop(surface);
/// }
/// ```
///
/// ```compile_fail,E0505
/// fn destroy_first(mut surface: moq_vaapi::Surface<()>, format: moq_vaapi::VAImageFormat) {
/// 	let _write = moq_vaapi::ImageMut::create_uninitialized(&mut surface, format).unwrap();
/// 	drop(surface);
/// }
/// ```
///
/// ```compile_fail,E0499
/// fn alias(surface: &mut moq_vaapi::Surface<()>, format: moq_vaapi::VAImageFormat) {
/// 	let first = moq_vaapi::ImageMut::create_uninitialized(surface, format).unwrap();
/// 	let second = moq_vaapi::ImageMut::create_uninitialized(surface, format).unwrap();
/// 	drop((first, second));
/// }
/// ```
#[must_use = "an image is written into its surface only by `put`"]
pub struct ImageMut<'a> {
	mapping: Mapping,
	surface_id: bindings::VASurfaceID,
	/// The surface's size, which the driver may have enlarged the image past.
	size: (u32, u32),
	surface: PhantomData<&'a mut ()>,
}

impl<'a> ImageMut<'a> {
	/// Creates an image in `format` at `surface`'s size without reading the
	/// surface back first, so every byte starts unspecified.
	///
	/// Write every pixel [`put`](Self::put) should not leave unspecified: it
	/// copies the image's top-left area, the surface's size, into the surface.
	pub fn create_uninitialized<D: SurfaceMemoryDescriptor>(
		surface: &'a mut Surface<D>,
		format: bindings::VAImageFormat,
	) -> Result<Self, VaError> {
		let size = surface.size();
		let created = create(surface.display(), format, size)?;
		Ok(Self {
			mapping: Mapping::new(created, size)?,
			surface_id: surface.id(),
			size,
			surface: PhantomData,
		})
	}

	/// Get a reference to the underlying `VAImage` that describes this image.
	pub fn image(&self) -> &bindings::VAImage {
		self.mapping.image()
	}

	/// Copies the image into its surface with `vaPutImage`, returning once the
	/// surface holds it.
	pub fn put(mut self) -> Result<(), VaError> {
		// libva requires the buffer unmapped before the driver reads it.
		self.mapping.unmap()?;
		let display = self.mapping.created.display.handle();
		let (width, height) = self.size;
		// Safe because the display handle is valid, `self.surface_id` is a surface we
		// borrow, and the image is a valid, unmapped `VAImage` at least this large.
		va_check(unsafe {
			bindings::va().vaPutImage(
				display,
				self.surface_id,
				self.mapping.image().image_id,
				0,
				0,
				width,
				height,
				0,
				0,
				width,
				height,
			)
		})?;
		// The copy may still be in flight, and the borrow ends with `self`, so
		// nothing after would know to wait for it.
		// Safe for the same reasons as above.
		va_check(unsafe { bindings::va().vaSyncSurface(display, self.surface_id) })
	}
}

// The mapping that unmaps and destroys the image carries no lifetime, so these empty
// drops are what make the drop checker hold the surface borrowed until it has run: libva
// destroys an image only while its surface lives.
impl Drop for Image<'_> {
	fn drop(&mut self) {}
}

impl Drop for ImageMut<'_> {
	fn drop(&mut self) {}
}

impl AsMut<[u8]> for ImageMut<'_> {
	fn as_mut(&mut self) -> &mut [u8] {
		// Safe since the buffer stays mapped for `data_size` bytes while `self` lives, and
		// `&mut self` makes this the only reference to it.
		unsafe { std::slice::from_raw_parts_mut(self.mapping.data, self.mapping.len()) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn images_can_cross_threads() {
		fn assert_send_sync<T: Send + Sync>() {}
		assert_send_sync::<Image<'static>>();
		assert_send_sync::<ImageMut<'static>>();
	}
}

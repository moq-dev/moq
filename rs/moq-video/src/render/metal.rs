//! Zero-copy import of a decoded `CVPixelBuffer` into `wgpu`, on macOS.
//!
//! VideoToolbox hands back IOSurface-backed pixel buffers, and Metal can address
//! an IOSurface directly. `CVMetalTextureCache` is the bridge: it wraps one
//! plane of a pixel buffer as an `MTLTexture` aliasing the same memory, and
//! recycles those wrappers as the decoder cycles its pool. `wgpu` then adopts
//! the texture through its `hal` seam. No download, no upload, no copy.
//!
//! The IOSurface backing is the load-bearing part: a pixel buffer allocated
//! without it (a plain `CVPixelBufferCreate`, as
//! [`Surface::into_pixel_buffer`](crate::Surface::into_pixel_buffer) does when
//! uploading a CPU frame) has no handle Metal can address, and the import fails.
//! Capture and decoder pools always set it, so frames off the fast path arrive
//! importable; anything else lands on the CPU fallback, which is the right
//! answer for pixels that were on the CPU to begin with.

use std::ptr::NonNull;

use objc2_core_foundation::CFRetained;
use objc2_core_video::{
	CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer, CVPixelBufferGetHeightOfPlane,
	CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidthOfPlane, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
	kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr8Planar,
};
use objc2_metal::{MTLPixelFormat, MTLTextureType};

use super::source::{Layout, Source};
use crate::frame::macos::PixelBuffer;
use crate::{Color, Error, Size};

fn err(message: impl std::fmt::Display) -> Error {
	Error::Render(anyhow::anyhow!("{message}"))
}

/// Holds the CoreVideo wrapper for as long as wgpu keeps the texture made from
/// it.
///
/// CoreVideo's contract is that the `CVMetalTexture` outlives the GPU's use of
/// the `MTLTexture` inside it, and it is the wrapper (not the Metal texture)
/// that holds the source `CVPixelBuffer` open. Release it when the import
/// returns and the buffer goes back to the decoder's pool as soon as the caller
/// drops the frame, free to be handed out and overwritten while a submitted draw
/// is still sampling it. That is a torn frame under pool pressure and nothing at
/// all when the pool is idle, which is the worst way for a bug to behave.
///
/// So the wrapper rides along in the texture's drop callback, which wgpu invokes
/// once it no longer references the texture (after the submissions using it
/// retire).
struct Keepalive(#[expect(dead_code, reason = "held for its release, never read")] CFRetained<CVMetalTexture>);

// SAFETY: CoreVideo objects are CoreFoundation types whose retain/release are
// thread-safe. Nothing here dereferences the wrapper after construction; it is
// moved into the drop callback and only ever dropped, which wgpu may do from
// whichever thread retires the submission.
unsafe impl Send for Keepalive {}
unsafe impl Sync for Keepalive {}

/// A `CVMetalTextureCache` bound to the renderer's Metal device.
///
/// Held across frames on purpose: the cache is what recycles the per-IOSurface
/// texture wrappers, so recreating it per frame would defeat the point.
pub(super) struct Import {
	cache: CFRetained<CVMetalTextureCache>,
}

// SAFETY: CVMetalTextureCache is a CoreFoundation type with thread-safe
// retain/release, and every use here goes through `&mut Import` (the renderer
// owns exactly one), so the cache is never touched concurrently. objc2 leaves
// CoreVideo types !Send out of conservatism, but a Renderer has to be movable
// between threads like the rest of the crate's handles.
unsafe impl Send for Import {}

impl Import {
	/// Build a texture cache on the same `MTLDevice` wgpu is rendering with.
	///
	/// Errors when the device is not a Metal one, which is how a caller running
	/// wgpu's Vulkan or GL backend on macOS lands on the CPU path.
	pub fn new(device: &wgpu::Device) -> Result<Self, Error> {
		// SAFETY: the handle is only read to create the cache, and it is
		// dropped before this returns, so nothing outlives the guard.
		let metal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
			.ok_or_else(|| err("wgpu device is not a Metal device"))?;

		let mut ptr: *mut CVMetalTextureCache = std::ptr::null_mut();
		// SAFETY: no attribute dictionaries are passed (so there are no generics
		// to get wrong), and `ptr` is a valid out-pointer for the duration.
		let status = unsafe {
			CVMetalTextureCache::create(
				None,
				None,
				metal.raw_device(),
				None,
				NonNull::new(&mut ptr).expect("stack pointer is non-null"),
			)
		};
		drop(metal);

		let cache = NonNull::new(ptr)
			.filter(|_| status == 0)
			// SAFETY: CVMetalTextureCacheCreate returns a +1 reference.
			.map(|ptr| unsafe { CFRetained::from_raw(ptr) })
			.ok_or_else(|| err(format!("CVMetalTextureCacheCreate failed: {status}")))?;

		Ok(Self { cache })
	}

	/// Alias each plane of `buffer` as a texture.
	pub fn import(&mut self, device: &wgpu::Device, buffer: &PixelBuffer) -> Result<Source, Error> {
		let size = Size::new(buffer.width(), buffer.height());
		let format = CVPixelBufferGetPixelFormatType(buffer.buffer());

		// The pixel format names the layout and the range (video swing vs full
		// swing).
		let (layout, range_is_full) = match format {
			f if f == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange => (Layout::Nv12, false),
			f if f == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange => (Layout::Nv12, true),
			// Rare: a pool configured for planar output rather than the NV12
			// every hardware decoder produces. Cheap to support and it keeps the
			// import from being NV12-only by construction.
			f if f == kCVPixelFormatType_420YpCbCr8Planar => (Layout::I420, false),
			f => return Err(err(format!("cannot import pixel format {f:#x}"))),
		};

		// The matrix comes off the buffer's own attachment, which VideoToolbox
		// copies out of the stream's VUI, so an imported frame and the same frame
		// downloaded to the CPU are read the same way. The planar format carries no
		// such attachment, so it falls back to the size guess.
		let color = buffer
			.color()
			.unwrap_or_else(|| Color::infer(size).with_range(!range_is_full));

		let (plane0, plane1, plane2) = match layout {
			Layout::Nv12 => {
				let y = self.plane(device, buffer, 0, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?;
				let uv = self.plane(
					device,
					buffer,
					1,
					MTLPixelFormat::RG8Unorm,
					wgpu::TextureFormat::Rg8Unorm,
				)?;
				// The I420 binding goes unread by the NV12 shader, so it just
				// needs something of the right kind bound.
				(y, uv.clone(), uv)
			}
			Layout::I420 => (
				self.plane(device, buffer, 0, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?,
				self.plane(device, buffer, 1, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?,
				self.plane(device, buffer, 2, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?,
			),
		};

		// Release the wrappers the cache has been holding for buffers the
		// decoder has already recycled. Cheap, and skipping it grows the cache
		// without bound.
		self.cache.flush(0);

		Ok(Source {
			layout,
			color,
			plane0,
			plane1,
			plane2,
		})
	}

	/// Wrap one plane as an `MTLTexture` and hand it to wgpu.
	fn plane(
		&self,
		device: &wgpu::Device,
		buffer: &PixelBuffer,
		index: usize,
		metal: MTLPixelFormat,
		format: wgpu::TextureFormat,
	) -> Result<wgpu::TextureView, Error> {
		let image: &CVPixelBuffer = buffer.buffer();
		let width = CVPixelBufferGetWidthOfPlane(image, index);
		let height = CVPixelBufferGetHeightOfPlane(image, index);
		if width == 0 || height == 0 {
			return Err(err(format!("pixel buffer has no plane {index}")));
		}

		let mut ptr: *mut CVMetalTexture = std::ptr::null_mut();
		// SAFETY: `image` is a live pixel buffer, no attribute dictionary is
		// passed, the plane index is in range (checked above via its extent),
		// and `ptr` is a valid out-pointer.
		let status = unsafe {
			CVMetalTextureCache::create_texture_from_image(
				None,
				&self.cache,
				image,
				None,
				metal,
				width,
				height,
				index,
				NonNull::new(&mut ptr).expect("stack pointer is non-null"),
			)
		};
		let texture = NonNull::new(ptr)
			.filter(|_| status == 0)
			// SAFETY: the create call returns a +1 reference.
			.map(|ptr| unsafe { CFRetained::from_raw(ptr) })
			.ok_or_else(|| err(format!("CVMetalTextureCacheCreateTextureFromImage failed: {status}")))?;

		let raw =
			CVMetalTextureGetTexture(&texture).ok_or_else(|| err("CVMetalTextureGetTexture returned no texture"))?;
		let keepalive = Keepalive(texture);

		let extent = wgpu::Extent3d {
			width: width as u32,
			height: height as u32,
			depth_or_array_layers: 1,
		};
		let descriptor = wgpu::TextureDescriptor {
			label: Some("moq-video imported plane"),
			size: extent,
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		};

		// SAFETY: the texture was created by CoreVideo on this device's
		// MTLDevice as a 2D, single-level, single-layer texture of exactly this
		// format and extent, matching `descriptor`. It holds decoded pixels, so
		// it is initialized, and `RESOURCE` is the state a sampled texture is
		// already in. `raw` transfers ownership of our retain into the texture,
		// and the drop callback holds the CoreVideo wrapper for as long as wgpu
		// keeps that texture (see `Keepalive`).
		let texture = unsafe {
			let hal = wgpu::hal::metal::Device::texture_from_raw(
				raw,
				format,
				MTLTextureType::Type2D,
				1,
				1,
				wgpu::hal::CopyExtent {
					width: extent.width,
					height: extent.height,
					depth: 1,
				},
				Some(Box::new(move || drop(keepalive))),
			);
			device.create_texture_from_hal::<wgpu::hal::api::Metal>(hal, &descriptor, wgpu::TextureUses::RESOURCE)
		};

		Ok(texture.create_view(&Default::default()))
	}
}

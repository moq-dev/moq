//! The Metal route: NV12 pixel buffers backed by IOSurfaces, which VideoToolbox
//! reads in place.
//!
//! A `CVPixelBufferPool` owns the buffers, so one goes back to it when the
//! encoder and every wgpu texture aliasing it let go, and its allocation
//! threshold is the exporter's depth. `CVMetalTextureCache` aliases each
//! buffer's two planes as Metal textures wgpu draws into, converting RGBA to
//! NV12 on the way, so the encoder gets the format every other Apple producer
//! hands it.

use std::ptr::NonNull;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
	CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer, CVPixelBufferPool,
	kCVMetalTextureUsage, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
	kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferPoolAllocationThresholdKey,
	kCVPixelBufferWidthKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnWouldExceedAllocationThreshold,
};
use objc2_metal::{MTLPixelFormat, MTLTexture, MTLTextureType, MTLTextureUsage};

use super::nv12::Nv12;
use super::{Exporter, err, extent, finish};
use crate::frame::apple::PixelBuffer;
use crate::render::metal::{Keepalive, texture_cache};
use crate::{Color, Error, Size, Surface};

pub(super) struct Export {
	device: wgpu::Device,
	queue: wgpu::Queue,
	size: Size,
	/// The space the conversion writes: what [`Surface::rgba`] would pick.
	color: Color,
	pool: CFRetained<CVPixelBufferPool>,
	/// Makes textures a render pass can draw into.
	cache: CFRetained<CVMetalTextureCache>,
	/// Caps the pool's buffers in use at [`Exporter::DEPTH`].
	threshold: CFRetained<Dictionary>,
	nv12: Nv12,
}

// SAFETY: the CoreFoundation types are reference counted with thread-safe
// retain/release, and every use goes through `&mut Export`, so the pool and the
// cache are never touched from two threads at once. objc2 leaves them !Send out
// of conservatism, but the exporter moves to whichever thread draws.
unsafe impl Send for Export {}

impl Export {
	/// `None` when `device` is not a Metal device, `size` has an odd edge NV12
	/// cannot hold, or the driver will not draw into a pixel buffer.
	pub fn open(device: &wgpu::Device, queue: &wgpu::Queue, size: Size) -> Result<Option<Self>, Error> {
		// SAFETY: only checked for presence, then dropped.
		if unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.is_none() {
			return Ok(None);
		}
		if !size.width.is_multiple_of(2) || !size.height.is_multiple_of(2) {
			return Ok(None);
		}

		let usage = CFNumber::new_i64((MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget).0 as i64);
		// SAFETY: CoreVideo's key constants are immutable statics.
		let textures = dictionary(&[(unsafe { kCVMetalTextureUsage }, &usage)]);
		let threshold = CFNumber::new_i32(Exporter::DEPTH as i32);
		// SAFETY: as above.
		let threshold = dictionary(&[(unsafe { kCVPixelBufferPoolAllocationThresholdKey }, &threshold)]);
		let export = Self {
			device: device.clone(),
			queue: queue.clone(),
			size,
			color: Color::infer(size),
			pool: pool(size)?,
			cache: texture_cache(device, Some(textures.as_opaque()))?,
			threshold,
			nv12: Nv12::new(device, Color::infer(size)),
		};

		// A driver that hands back a texture it cannot draw into is the readback's.
		let buffer = export.buffer()?.expect("an unused pool has room");
		let plane = export.alias(&buffer, 0, MTLPixelFormat::R8Unorm, size)?;
		let raw = CVMetalTextureGetTexture(&plane).ok_or_else(|| err("CVMetalTextureGetTexture returned none"))?;
		if !raw.usage().contains(MTLTextureUsage::RenderTarget) {
			tracing::info!(usage = ?raw.usage(), "pixel buffer textures are not drawable; reading frames back");
			return Ok(None);
		}
		Ok(Some(export))
	}

	pub fn copy(&mut self, texture: &wgpu::Texture) -> Result<Option<Copy>, Error> {
		let Some(buffer) = self.buffer()? else {
			return Ok(None);
		};
		let chroma = Size::new(self.size.width / 2, self.size.height / 2);
		let luma = self.plane(
			&buffer,
			0,
			MTLPixelFormat::R8Unorm,
			wgpu::TextureFormat::R8Unorm,
			self.size,
		)?;
		let chroma = self.plane(
			&buffer,
			1,
			MTLPixelFormat::RG8Unorm,
			wgpu::TextureFormat::Rg8Unorm,
			chroma,
		)?;

		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-video export"),
		});
		let view = |texture: &wgpu::Texture| texture.create_view(&Default::default());
		self.nv12
			.encode(&self.device, &mut encoder, texture, &view(&luma), &view(&chroma));
		let submission = self.queue.submit([encoder.finish()]);
		// Lets go of the wrappers for buffers the pool has recycled.
		self.cache.flush(0);

		let mut buffer = PixelBuffer::new(buffer, self.size.width, self.size.height);
		buffer.color = Some(self.color);
		Ok(Some(Copy { buffer, submission }))
	}

	/// A buffer from the pool, or `None` when [`Exporter::DEPTH`] are in use.
	fn buffer(&self) -> Result<Option<CFRetained<CVPixelBuffer>>, Error> {
		let mut ptr: *mut CVPixelBuffer = std::ptr::null_mut();
		// SAFETY: the aux attributes are a CFDictionary of pool keys, and `ptr`
		// is a valid out-pointer.
		let status = unsafe {
			CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
				None,
				&self.pool,
				Some(self.threshold.as_opaque()),
				NonNull::new(&mut ptr).expect("stack pointer is non-null"),
			)
		};
		if status == kCVReturnWouldExceedAllocationThreshold {
			return Ok(None);
		}
		NonNull::new(ptr)
			.filter(|_| status == 0)
			// SAFETY: the create call returns a +1 reference.
			.map(|ptr| Some(unsafe { CFRetained::from_raw(ptr) }))
			.ok_or_else(|| err(format!("CVPixelBufferPoolCreatePixelBuffer failed: {status}")))
	}

	/// Alias plane `index` of `buffer` as a `size` Metal texture.
	fn alias(
		&self,
		buffer: &CVPixelBuffer,
		index: usize,
		format: MTLPixelFormat,
		size: Size,
	) -> Result<CFRetained<CVMetalTexture>, Error> {
		let mut ptr: *mut CVMetalTexture = std::ptr::null_mut();
		// SAFETY: `buffer` is a live NV12 pixel buffer from this pool, which has
		// planes 0 and 1 of the sizes asked for, no attribute dictionary is
		// passed, and `ptr` is a valid out-pointer.
		let status = unsafe {
			CVMetalTextureCache::create_texture_from_image(
				None,
				&self.cache,
				buffer,
				None,
				format,
				size.width as usize,
				size.height as usize,
				index,
				NonNull::new(&mut ptr).expect("stack pointer is non-null"),
			)
		};
		NonNull::new(ptr)
			.filter(|_| status == 0)
			// SAFETY: the create call returns a +1 reference.
			.map(|ptr| unsafe { CFRetained::from_raw(ptr) })
			.ok_or_else(|| err(format!("CVMetalTextureCacheCreateTextureFromImage failed: {status}")))
	}

	/// Plane `index` of `buffer` as a texture wgpu draws into.
	fn plane(
		&self,
		buffer: &CVPixelBuffer,
		index: usize,
		metal: MTLPixelFormat,
		format: wgpu::TextureFormat,
		size: Size,
	) -> Result<wgpu::Texture, Error> {
		let alias = self.alias(buffer, index, metal, size)?;
		let raw = CVMetalTextureGetTexture(&alias).ok_or_else(|| err("CVMetalTextureGetTexture returned none"))?;
		let keepalive = Keepalive(alias);
		let descriptor = wgpu::TextureDescriptor {
			label: Some("moq-video export plane"),
			size: extent(size),
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
			view_formats: &[],
		};
		// SAFETY: CoreVideo made `raw` on this device's MTLDevice as a 2D,
		// single-level, single-layer texture of this format and extent, drawable
		// per the cache's usage (checked when the export opened). The drop
		// callback holds the CoreVideo wrapper, and through it the pixel buffer,
		// for as long as wgpu keeps the texture.
		let texture = unsafe {
			let hal = wgpu::hal::metal::Device::texture_from_raw(
				raw,
				format,
				MTLTextureType::Type2D,
				1,
				1,
				wgpu::hal::CopyExtent {
					width: size.width,
					height: size.height,
					depth: 1,
				},
				Some(Box::new(move || drop(keepalive))),
			);
			self.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
				hal,
				&descriptor,
				wgpu::TextureUses::UNINITIALIZED,
			)
		};
		Ok(texture)
	}
}

/// One conversion in flight. Dropping it gives the buffer back to the pool.
pub(super) struct Copy {
	buffer: PixelBuffer,
	submission: wgpu::SubmissionIndex,
}

impl Copy {
	pub fn wait(self, device: &wgpu::Device) -> Result<Surface, Error> {
		finish(device, self.submission)?;
		Ok(Surface::PixelBuffer(self.buffer))
	}
}

/// A pool of IOSurface-backed NV12 buffers Metal can alias, for `size` frames.
fn pool(size: Size) -> Result<CFRetained<CVPixelBufferPool>, Error> {
	let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
	let width = CFNumber::new_i32(size.width as i32);
	let height = CFNumber::new_i32(size.height as i32);
	let iosurface = Dictionary::empty();
	// SAFETY: CoreVideo's key constants are immutable statics.
	let attributes = unsafe {
		dictionary(&[
			(kCVPixelBufferPixelFormatTypeKey, &format),
			(kCVPixelBufferWidthKey, &width),
			(kCVPixelBufferHeightKey, &height),
			(kCVPixelBufferIOSurfacePropertiesKey, &iosurface),
			(kCVPixelBufferMetalCompatibilityKey, CFBoolean::new(true)),
		])
	};

	let mut ptr: *mut CVPixelBufferPool = std::ptr::null_mut();
	// SAFETY: the attributes are a CFDictionary of pixel buffer keys, and `ptr`
	// is a valid out-pointer.
	let status = unsafe {
		CVPixelBufferPool::create(
			None,
			None,
			Some(attributes.as_opaque()),
			NonNull::new(&mut ptr).expect("stack pointer is non-null"),
		)
	};
	NonNull::new(ptr)
		.filter(|_| status == 0)
		// SAFETY: the create call returns a +1 reference.
		.map(|ptr| unsafe { CFRetained::from_raw(ptr) })
		.ok_or_else(|| err(format!("CVPixelBufferPoolCreate failed: {status}")))
}

type Dictionary = CFDictionary<CFString, CFType>;

/// A CoreFoundation dictionary of `entries`.
fn dictionary(entries: &[(&CFString, &CFType)]) -> CFRetained<Dictionary> {
	let keys: Vec<&CFString> = entries.iter().map(|(key, _)| *key).collect();
	let values: Vec<&CFType> = entries.iter().map(|(_, value)| *value).collect();
	Dictionary::from_slices(&keys, &values)
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::render::export::Config;
	use crate::render::export::tests::{center, gpu, near, texture};

	/// VideoToolbox takes the export as the IOSurface it was drawn into, and the
	/// picture survives the conversion to NV12.
	#[tokio::test]
	async fn videotoolbox_reads_the_export_in_place() {
		let (device, queue) = gpu().await.expect("a Metal device");
		let size = Size::new(64, 48);
		let mut exporter = Exporter::new(&device, &queue, size, &Config::default()).unwrap();
		assert!(exporter.zero_copy(), "a Metal device exports pixel buffers");

		let mut config = crate::encode::Config::new(size.width, size.height, crate::FrameRate::new(30, 1).unwrap());
		exporter.probe(&mut config).await.unwrap();
		assert!(exporter.zero_copy(), "VideoToolbox reads pixel buffers");
		let mut encoder = crate::encode::Encoder::new(&config).unwrap();
		assert_eq!(encoder.name(), "videotoolbox");

		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let mut held: Vec<_> = (0..Exporter::DEPTH)
			.map(|_| exporter.copy(&red, Timestamp::ZERO).unwrap().expect("a free buffer"))
			.collect();
		assert!(
			exporter.copy(&red, Timestamp::ZERO).unwrap().is_none(),
			"the pool is capped"
		);

		let frame = held.pop().unwrap().wait().unwrap();
		assert!(matches!(frame.surface, Surface::PixelBuffer(_)));
		let actual = center(&frame);
		assert!(near(actual, [255, 0, 0, 255]), "{actual:?}");

		let mut encoded = encoder.encode(&frame).unwrap();
		drop(frame);
		drop(held);
		for index in 1..12u64 {
			let timestamp = Timestamp::from_micros(index * 33_333).unwrap();
			let frame = exporter
				.copy(&red, timestamp)
				.unwrap()
				.expect("a free buffer")
				.wait()
				.unwrap();
			encoded.extend(encoder.encode(&frame).unwrap());
		}
		encoded.extend(encoder.finish().unwrap());
		assert!(encoded.first().is_some_and(|encoded| encoded.keyframe));
	}
}

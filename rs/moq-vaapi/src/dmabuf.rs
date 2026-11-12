//! Importing a Linux DMA-BUF as a VA surface, without copying it.
//!
//! A [`DmaBuf`] describes an allocation some other API made (a PipeWire
//! producer, a camera, a V4L2 device, another VA-API context that exported
//! its surface) well enough for the driver to wrap it as a [`Surface`]. The
//! surface reads the producer's memory directly: an encoder given one encodes
//! the producer's pixels, and a post-processor given one converts them, without
//! the frame passing through the CPU.
//!
//! The format is named the DRM way, which is what every producer of a DMA-BUF
//! speaks. [`va_fourcc`] maps it to the VA-API name, which differs for packed
//! RGB (DRM names the channels in little-endian word order, VA-API in byte
//! order) and for planar 4:2:0 (`YU12` against `I420`).

use std::io::{Seek, SeekFrom};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;

use crate::{
	Color, Display, DrmPrimeSurfaceDescriptor, ExternalBufferDescriptor, MemoryType, Surface, UsageHint,
	VADRMPRIMESurfaceDescriptor, VA_FOURCC_ARGB, VA_FOURCC_BGRA, VA_FOURCC_BGRX, VA_FOURCC_I420, VA_FOURCC_NV12,
	VA_FOURCC_P010, VA_FOURCC_RGBA, VA_FOURCC_RGBX, VA_FOURCC_XRGB, VA_FOURCC_YUY2, VA_FOURCC_YV12, VA_RT_FORMAT_RGB32,
	VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV422,
};

/// DRM fourcc codes for the formats [`va_fourcc`] maps.
///
/// These are the `DRM_FORMAT_*` values from `drm_fourcc.h`, which the VA-API
/// headers do not carry.
pub mod drm {
	const fn fourcc(code: &[u8; 4]) -> u32 {
		u32::from_le_bytes(*code)
	}

	/// Semi-planar 4:2:0, a luma plane and an interleaved Cb/Cr plane.
	pub const NV12: u32 = fourcc(b"NV12");
	/// Semi-planar 4:2:0 with 10 bits per sample in 16-bit words.
	pub const P010: u32 = fourcc(b"P010");
	/// Planar 4:2:0: luma, then Cb, then Cr.
	pub const YUV420: u32 = fourcc(b"YU12");
	/// Planar 4:2:0: luma, then Cr, then Cb.
	pub const YVU420: u32 = fourcc(b"YV12");
	/// Packed 4:2:2, `Y0 Cb Y1 Cr` in memory.
	pub const YUYV: u32 = fourcc(b"YUYV");
	/// Packed 32-bit, `B G R X` in memory.
	pub const XRGB8888: u32 = fourcc(b"XR24");
	/// Packed 32-bit, `B G R A` in memory.
	pub const ARGB8888: u32 = fourcc(b"AR24");
	/// Packed 32-bit, `R G B X` in memory.
	pub const XBGR8888: u32 = fourcc(b"XB24");
	/// Packed 32-bit, `R G B A` in memory.
	pub const ABGR8888: u32 = fourcc(b"AB24");
	/// Packed 32-bit, `X R G B` in memory.
	pub const BGRX8888: u32 = fourcc(b"BX24");
	/// Packed 32-bit, `A R G B` in memory.
	pub const BGRA8888: u32 = fourcc(b"BA24");
}

/// Returns the `VA_FOURCC_*` naming the same layout as the DRM format `drm_format`.
///
/// Returns `None` for a format this crate has no mapping for.
pub fn va_fourcc(drm_format: u32) -> Option<u32> {
	Some(match drm_format {
		drm::NV12 => VA_FOURCC_NV12,
		drm::P010 => VA_FOURCC_P010,
		drm::YUV420 => VA_FOURCC_I420,
		drm::YVU420 => VA_FOURCC_YV12,
		drm::YUYV => VA_FOURCC_YUY2,
		drm::XRGB8888 => VA_FOURCC_BGRX,
		drm::ARGB8888 => VA_FOURCC_BGRA,
		drm::XBGR8888 => VA_FOURCC_RGBX,
		drm::ABGR8888 => VA_FOURCC_RGBA,
		drm::BGRX8888 => VA_FOURCC_XRGB,
		drm::BGRA8888 => VA_FOURCC_ARGB,
		_ => return None,
	})
}

/// Returns the `VA_RT_FORMAT_*` a surface of the VA fourcc `fourcc` is allocated as.
///
/// Returns `None` for a format this crate has no mapping for. Guessing the
/// render-target format wrong makes `vaCreateSurfaces` fail with an error that
/// names neither, so an unknown format is refused up front instead.
pub fn rt_format(fourcc: u32) -> Option<u32> {
	match fourcc {
		VA_FOURCC_NV12 | VA_FOURCC_I420 | VA_FOURCC_YV12 => Some(VA_RT_FORMAT_YUV420),
		VA_FOURCC_P010 => Some(VA_RT_FORMAT_YUV420_10),
		VA_FOURCC_YUY2 => Some(VA_RT_FORMAT_YUV422),
		VA_FOURCC_BGRA | VA_FOURCC_BGRX | VA_FOURCC_RGBA | VA_FOURCC_RGBX | VA_FOURCC_ARGB | VA_FOURCC_XRGB => {
			Some(VA_RT_FORMAT_RGB32)
		}
		_ => None,
	}
}

/// One plane of a DMA-BUF, within the single object holding them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
	/// Byte offset of the plane from the start of the object.
	pub offset: u32,
	/// Bytes between adjacent rows of the plane.
	pub pitch: u32,
}

/// An externally allocated DMA-BUF, described well enough to import as a surface.
///
/// One object holding every plane, which is what a VA-API decoder exports and
/// what a PipeWire producer negotiating a single modifier hands out. A buffer
/// split across several objects is not describable here.
///
/// The descriptor owns the file descriptor and keeps it open for the life of the
/// surface built from it. libva does not take ownership (the driver resolves the
/// buffer during `vaCreateSurfaces` and leaves the fd alone), so holding it
/// costs one descriptor and removes the question of whether closing it early is
/// safe.
#[derive(Debug)]
pub struct DmaBuf {
	/// Pixel format, a `DRM_FORMAT_*` code; see [`drm`].
	pub drm_format: u32,
	/// Format modifier describing the tiling of the object.
	pub modifier: u64,
	/// Width of the picture in pixels.
	///
	/// The visible width, not the allocated one: the padding lives in each
	/// plane's pitch and offset.
	pub width: u32,
	/// Height of the picture in pixels, likewise the visible height.
	pub height: u32,
	/// Plane offsets and pitches, in format order. One to four.
	pub planes: Vec<Plane>,
	/// The exported descriptor, held open until the surface is destroyed.
	pub fd: OwnedFd,
	/// The YUV color space of the pixels, where the producer knows it.
	///
	/// Ignored for packed RGB. The encoder converts a YUV buffer whose space
	/// differs from the stream's, and takes one with `None` to be in the
	/// stream's space already.
	pub color: Option<Color>,
}

impl DmaBuf {
	/// Describes a surface exported by [`Surface::export_prime`] for import elsewhere.
	///
	/// The color is left unknown; set [`color`](Self::color) when the
	/// producer knows it.
	///
	/// # Errors
	///
	/// Fails unless the export is one object holding one composed layer, which
	/// is what `export_prime` asks the driver for.
	pub fn from_prime(mut exported: DrmPrimeSurfaceDescriptor) -> anyhow::Result<Self> {
		if exported.objects.len() != 1 || exported.layers.len() != 1 {
			anyhow::bail!(
				"expected one object and one layer, the export has {} and {}",
				exported.objects.len(),
				exported.layers.len()
			);
		}
		let object = exported.objects.remove(0);
		let layer = &exported.layers[0];
		Ok(Self {
			drm_format: layer.drm_format,
			modifier: object.drm_format_modifier,
			width: exported.width,
			height: exported.height,
			planes: (0..(layer.num_planes as usize).min(4))
				.map(|i| Plane {
					offset: layer.offset[i],
					pitch: layer.pitch[i],
				})
				.collect(),
			fd: object.fd,
			color: None,
		})
	}

	/// Returns the `VA_FOURCC_*` of this buffer's format, if it has one.
	pub fn fourcc(&self) -> Option<u32> {
		va_fourcc(self.drm_format)
	}

	/// Imports this buffer as a surface on `display`, without copying it.
	///
	/// The returned surface owns the buffer, so the descriptor stays open
	/// exactly as long as the surface referring to it. `usage_hint` tells the
	/// driver what the surface is for; some drivers only accept an import as an
	/// encoder source when it says so.
	///
	/// # Errors
	///
	/// Fails when the format has no VA-API mapping, when the plane count is not
	/// one to four, and when the driver refuses the import, which it does for a
	/// modifier or pitch it cannot address.
	pub fn import(self, display: &Arc<Display>, usage_hint: Option<UsageHint>) -> anyhow::Result<Surface<DmaBuf>> {
		let fourcc = self
			.fourcc()
			.ok_or_else(|| anyhow::anyhow!("no VA-API format for DRM format {:#010x}", self.drm_format))?;
		let rt_format =
			rt_format(fourcc).ok_or_else(|| anyhow::anyhow!("no VA-API render target format for {fourcc:#010x}"))?;
		if self.planes.is_empty() || self.planes.len() > 4 {
			anyhow::bail!("a DMA-BUF import takes one to four planes, got {}", self.planes.len());
		}
		let (width, height) = (self.width, self.height);

		display
			.create_surfaces(rt_format, Some(fourcc), width, height, usage_hint, vec![self])
			.map_err(|e| anyhow::anyhow!("import a {width}x{height} DMA-BUF as a surface: {e:?}"))?
			.pop()
			.ok_or_else(|| anyhow::anyhow!("vaCreateSurfaces returned no surface"))
	}
}

impl ExternalBufferDescriptor for DmaBuf {
	const MEMORY_TYPE: MemoryType = MemoryType::DrmPrime2;
	type DescriptorAttribute = VADRMPRIMESurfaceDescriptor;

	fn va_surface_attribute(&mut self) -> Self::DescriptorAttribute {
		// `import` has checked the format and the plane count; an unmapped format
		// reaching here anyway goes out as zero, which the driver refuses.
		let mut descriptor = VADRMPRIMESurfaceDescriptor {
			fourcc: self.fourcc().unwrap_or(0),
			width: self.width,
			height: self.height,
			num_objects: 1,
			num_layers: 1,
			..Default::default()
		};

		descriptor.objects[0].fd = self.fd.as_raw_fd();
		descriptor.objects[0].size = buffer_size(&self.fd);
		descriptor.objects[0].drm_format_modifier = self.modifier;

		let layer = &mut descriptor.layers[0];
		layer.drm_format = self.drm_format;
		layer.num_planes = self.planes.len().min(4) as u32;
		for (index, plane) in self.planes.iter().take(4).enumerate() {
			layer.object_index[index] = 0;
			layer.offset[index] = plane.offset;
			layer.pitch[index] = plane.pitch;
		}

		descriptor
	}
}

/// Returns the size of the DMA-BUF behind `fd`, or zero when the kernel will not say.
///
/// A DMA-BUF reports its size through `lseek(SEEK_END)`. Intel's iHD driver
/// refuses to import packed RGB with a size of zero, where for YUV formats it
/// works the size out from the layout; zero stays the fallback because it is
/// what the other drivers expect when the size is unknown. The seek moves the
/// file offset shared with the producer's descriptor. The kernel accepts only
/// the start and the end as a DMA-BUF offset (not even `SEEK_CUR`), so the
/// offset was at the start before, and seeking back there restores it.
fn buffer_size(fd: &OwnedFd) -> u32 {
	let Ok(clone) = fd.try_clone() else {
		return 0;
	};
	let mut file = std::fs::File::from(clone);
	let size = file
		.seek(SeekFrom::End(0))
		.ok()
		.and_then(|size| u32::try_from(size).ok());
	let _ = file.seek(SeekFrom::Start(0));
	size.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_mapped_format_has_a_render_target() {
		for format in [
			drm::NV12,
			drm::P010,
			drm::YUV420,
			drm::YVU420,
			drm::YUYV,
			drm::XRGB8888,
			drm::ARGB8888,
			drm::XBGR8888,
			drm::ABGR8888,
			drm::BGRX8888,
			drm::BGRA8888,
		] {
			let fourcc = va_fourcc(format).expect("mapped");
			assert!(rt_format(fourcc).is_some(), "{format:#010x} has no render target");
		}
	}

	#[test]
	fn packed_rgb_maps_to_the_byte_order_name() {
		// DRM XRGB8888 is a little-endian word, so memory holds B G R X, which
		// VA-API calls BGRX.
		assert_eq!(va_fourcc(drm::XRGB8888), Some(VA_FOURCC_BGRX));
		assert_eq!(va_fourcc(drm::ABGR8888), Some(VA_FOURCC_RGBA));
		assert_eq!(va_fourcc(drm::YUV420), Some(VA_FOURCC_I420));
		assert_eq!(va_fourcc(u32::from_le_bytes(*b"ZZZZ")), None);
	}
}

//! Zero-copy import of Linux DMA-BUFs into wgpu's Vulkan backend.
//!
//! wgpu owns the Vulkan images and imported descriptors once wrapping succeeds.
//! The [`DmaBuf`] itself rides with the submitted render work separately, keeping
//! the dequeued producer buffer out of PipeWire's pool until the GPU is done.
//!
//! wgpu-hal's importer describes one image made of one memory plane, which for a
//! packed format is the whole buffer and for a planar one is not. Mesa reports
//! two memory planes for `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM` under every
//! modifier it lists, so NV12 has no route through that importer as a single
//! image. Taken a plane at a time it does: the same driver reports one memory
//! plane for `R8_UNORM` and `R8G8_UNORM`, so a luma import and a chroma import of
//! the same object, each at its own offset and row pitch, alias the buffer as the
//! two textures the NV12 shader wants. Nothing is copied and nothing is
//! reformatted; only the descriptor is duplicated, once per plane.
//!
//! What that leaves is the modifier itself. A buffer whose modifier the driver
//! does not list for those formats at all still has nowhere to go, and with the
//! `vaapi` feature it gets a second chance: blitted on the GPU into an allocation
//! the driver lays out itself. The direct import is
//! always tried first, since it moves nothing at all.

use std::os::fd::{AsFd, BorrowedFd};

use wgpu::hal::MemoryFlags;

use super::source::{Layout, Source};
use crate::{Color, DmaBuf, DmaBufPlane, DrmFormat, Error, Size};

fn err(message: impl std::fmt::Display) -> Error {
	Error::Render(anyhow::anyhow!("{message}"))
}

/// Alias one DMA-BUF allocation as the sampled Vulkan textures of a frame.
pub(super) fn import(device: &wgpu::Device, buffer: &DmaBuf) -> Result<Option<Source>, Error> {
	if !device
		.features()
		.contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
	{
		return Ok(None);
	}

	// SAFETY: the guard is only asked whether it exists, and drops here.
	// A device carrying the feature above is Vulkan-backed, so this is a
	// belt-and-braces check rather than a route anything takes.
	if unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }.is_none() {
		return Ok(None);
	}

	let size = Size::new(buffer.width(), buffer.height());
	let shape = Shape::of(buffer.format(), size)?;
	let planes = shape.planes(buffer.planes())?;
	// Packed RGB carries its own primaries; only YUV needs a matrix, and the
	// producer rarely names one, so the frame size is usually all there is.
	let color = match shape.layout {
		Layout::Rgba => None,
		_ => Some(buffer.color().unwrap_or_else(|| Color::infer(size))),
	};

	// One export, and so one wait on the producer's write fence, however
	// many import attempts follow it.
	let export = buffer
		.export()
		.map_err(|e| Error::Render(anyhow::Error::new(e).context("export DMA-BUF")))?;
	let (fd, lease) = export.into_parts();

	// SAFETY: `fd` is a live descriptor for this DMA-BUF, and `adopt`
	// duplicates it per plane rather than consuming it. Export waited for
	// producer writes, and the modifier, extent, stride, and offset of every
	// plane come from the producer's own buffer metadata.
	let direct = unsafe {
		adopt(
			device,
			fd.as_fd(),
			buffer.modifier(),
			shape.layout,
			color,
			&planes,
			Some(Box::new(lease.clone())),
		)
	};

	direct.map(Some)
}

/// How a DRM format is sampled: the shader layout it feeds, and the Vulkan
/// format and extent of each of its memory planes, in the order the buffer
/// describes them.
#[derive(Clone, Copy)]
struct Shape {
	layout: Layout,
	/// Luma, or the whole picture for a packed format.
	plane0: (wgpu::TextureFormat, Size),
	/// Interleaved chroma for NV12, Cb for I420.
	plane1: Option<(wgpu::TextureFormat, Size)>,
	/// Cr for I420.
	plane2: Option<(wgpu::TextureFormat, Size)>,
}

impl Shape {
	/// The shape a `width` x `height` buffer of `format` imports as.
	///
	/// Every entry is a one- or two-component format, which is the point: those
	/// are what Mesa reports a single memory plane for, so each one is a whole
	/// image as far as wgpu-hal's importer is concerned.
	fn of(format: DrmFormat, size: Size) -> Result<Self, Error> {
		let packed = |format| Self {
			layout: Layout::Rgba,
			plane0: (format, size),
			plane1: None,
			plane2: None,
		};

		let subsampled = || -> Result<Size, Error> {
			size.validate("4:2:0 DMA-BUF")?;
			Ok(Size::new(size.width / 2, size.height / 2))
		};

		Ok(match format {
			DrmFormat::XRGB8888 | DrmFormat::ARGB8888 => packed(wgpu::TextureFormat::Bgra8Unorm),
			DrmFormat::XBGR8888 | DrmFormat::ABGR8888 => packed(wgpu::TextureFormat::Rgba8Unorm),
			DrmFormat::NV12 => Self {
				layout: Layout::Nv12,
				plane0: (wgpu::TextureFormat::R8Unorm, size),
				plane1: Some((wgpu::TextureFormat::Rg8Unorm, subsampled()?)),
				plane2: None,
			},
			DrmFormat::YUV420 => {
				let half = subsampled()?;
				Self {
					layout: Layout::I420,
					plane0: (wgpu::TextureFormat::R8Unorm, size),
					plane1: Some((wgpu::TextureFormat::R8Unorm, half)),
					plane2: Some((wgpu::TextureFormat::R8Unorm, half)),
				}
			}
			format => return Err(err(format!("cannot import DMA-BUF format {:#x}", format.as_raw()))),
		})
	}

	/// Pair each plane's format and extent with the producer's offset and pitch.
	///
	/// Errors when the buffer does not describe the number of planes its format
	/// calls for, rather than importing the ones it does describe and sampling
	/// whatever the missing one's binding happens to hold. A frame short a chroma
	/// plane renders as a uniform green picture, which looks enough like video to
	/// be missed.
	fn planes(&self, described: &[DmaBufPlane]) -> Result<Vec<Plane>, Error> {
		let wanted = [Some(self.plane0), self.plane1, self.plane2];
		let wanted = wanted.iter().flatten();
		let count = 1 + usize::from(self.plane1.is_some()) + usize::from(self.plane2.is_some());
		if described.len() != count {
			return Err(err(format!(
				"DMA-BUF describes {} planes, but its format has {count}",
				described.len()
			)));
		}

		Ok(wanted
			.zip(described)
			.map(|(&(format, size), plane)| Plane {
				format,
				size,
				stride: plane.stride(),
				offset: plane.offset(),
			})
			.collect())
	}
}

/// One memory plane as it is imported: what to sample it as, how big it is, and
/// where in the object it lives.
struct Plane {
	format: wgpu::TextureFormat,
	size: Size,
	stride: u32,
	offset: u32,
}

/// Alias each of an object's planes as a Vulkan image and bind them as a source.
///
/// # Safety
///
/// `fd` must be a descriptor for a DMA-BUF whose pixels are written and
/// readable, laid out exactly as `modifier` and each [`Plane`] say. The
/// descriptor is duplicated per import, so the caller keeps its own and Vulkan
/// keeps the copies, closing them itself if an import fails.
unsafe fn adopt(
	device: &wgpu::Device,
	fd: BorrowedFd<'_>,
	modifier: u64,
	layout: Layout,
	color: Option<Color>,
	planes: &[Plane],
	keepalive: Option<Box<dyn Send + Sync>>,
) -> Result<Source, Error> {
	let mut views = Vec::with_capacity(planes.len());
	for plane in planes {
		// A fresh descriptor per plane: Vulkan takes ownership of the one it is
		// handed, on success by holding it and on failure by closing it, so one
		// cannot serve two imports.
		let dup = fd
			.try_clone_to_owned()
			.map_err(|e| Error::Render(anyhow::Error::new(e).context("duplicate DMA-BUF")))?;
		// SAFETY: the caller's contract, narrowed to this plane.
		views.push(unsafe { adopt_plane(device, dup, modifier, plane) }?);
	}

	// The NV12 shader reads plane0 and plane1 and the RGBA one reads plane0
	// alone, but a single bind group layout serves all three, so the unread
	// bindings get the last real view rather than a texture of their own.
	let filler = views.last().expect("a format has at least one plane").clone();
	let mut views = views.into_iter();
	let plane0 = views.next().expect("a format has at least one plane");
	let plane1 = views.next().unwrap_or_else(|| filler.clone());
	let plane2 = views.next().unwrap_or(filler);

	Ok(Source {
		layout,
		color,
		plane0,
		plane1,
		plane2,
		keepalive,
	})
}

/// Hand one plane's descriptor to wgpu and wrap the result as a sampled texture.
///
/// # Safety
///
/// As [`adopt`], for this plane. Vulkan consumes `fd` on success and wgpu-hal
/// closes it on error, so ownership passes either way.
unsafe fn adopt_plane(
	device: &wgpu::Device,
	fd: std::os::fd::OwnedFd,
	modifier: u64,
	plane: &Plane,
) -> Result<wgpu::TextureView, Error> {
	let extent = wgpu::Extent3d {
		width: plane.size.width,
		height: plane.size.height,
		depth_or_array_layers: 1,
	};
	let descriptor = wgpu::TextureDescriptor {
		label: Some("moq-video imported DMA-BUF"),
		size: extent,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: plane.format,
		usage: wgpu::TextureUsages::TEXTURE_BINDING,
		view_formats: &[],
	};
	let hal_descriptor = wgpu::hal::TextureDescriptor {
		label: descriptor.label,
		size: extent,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: plane.format,
		usage: wgpu::TextureUses::RESOURCE,
		memory_flags: MemoryFlags::empty(),
		view_formats: Vec::new(),
	};

	// SAFETY: the guard is only used to import a descriptor into the same
	// Vulkan device. It drops before the resulting HAL texture is wrapped.
	let hal = (unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() })
		.ok_or_else(|| err("wgpu device is not a Vulkan device"))?;
	// SAFETY: the caller's contract, forwarded.
	let texture =
		unsafe { hal.texture_from_dmabuf_fd(fd, &hal_descriptor, modifier, plane.stride as u64, plane.offset as u64) }
			.map_err(|e| {
				err(format!(
					"Vulkan DMA-BUF import of a {:?} {} plane at modifier {modifier:#x}: {e:?}",
					plane.format, plane.size
				))
			})?;
	drop(hal);

	// SAFETY: wgpu-hal created `texture` on this device from `hal_descriptor`,
	// which exactly matches the public descriptor. Imported pixels are already
	// initialized and will first be used as a sampled resource.
	let texture = unsafe {
		device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(texture, &descriptor, wgpu::TextureUses::RESOURCE)
	};
	Ok(texture.create_view(&Default::default()))
}

/// Builds DMA-BUFs to import, for tests that would otherwise need a decoder.
///
/// VA-API is the allocator because it is the one already in the dependency
/// graph that hands out the kind of buffer this module exists for: an NV12
/// surface laid out however the driver lays out a decode target, tiling and
/// padding included. What the caller gets is the same shape a hardware decoder
/// would produce, with pixels it chose itself.
#[cfg(all(test, feature = "vaapi"))]
pub(super) mod fixture {
	use std::os::fd::OwnedFd;
	use std::sync::Arc;

	use super::*;

	/// A DMA-BUF built from an already exported descriptor.
	///
	/// The export holds its own reference on the underlying allocation, so the
	/// VA-API surface it came from is long gone by the time anything imports
	/// this. Downloading is not implemented: the point of the fixture is the
	/// zero-copy path, and a test comparing against the CPU one builds its
	/// reference from the bytes it uploaded rather than reading them back.
	struct Exported(OwnedFd);

	impl crate::frame::DmaBufFrame for Exported {
		fn export(&self) -> std::io::Result<OwnedFd> {
			self.0.try_clone()
		}

		fn download_i420(&self) -> Result<crate::frame::I420, Error> {
			Err(err("the DMA-BUF fixture does not implement downloading"))
		}
	}

	/// The rows and row length of each plane of `format` at `size`, tightly
	/// packed, in the order the format lists them.
	///
	/// NV12's chroma row is as long as its luma row despite covering half the
	/// pixels, since it holds a Cb and a Cr per pair; I420's two chroma rows are
	/// each half as long.
	fn rows(format: DrmFormat, size: Size) -> Vec<(usize, usize)> {
		let (width, height) = (size.width as usize, size.height as usize);
		match format {
			DrmFormat::NV12 => vec![(height, width), (height / 2, width)],
			DrmFormat::YUV420 => vec![(height, width), (height / 2, width / 2), (height / 2, width / 2)],
			format => panic!("the fixture cannot build DMA-BUF format {:#x}", format.as_raw()),
		}
	}

	/// A DMA-BUF of `format` and `size` holding `pixels`, which are the planes
	/// tightly packed one after another.
	///
	/// `None` when the host has no VA-API device or its driver will not allocate
	/// or export such a surface, which is how this skips on a builder rather
	/// than failing there. It says which on the way out.
	pub(in crate::render) fn surface(format: DrmFormat, size: Size, pixels: &[u8]) -> Option<DmaBuf> {
		let fourcc = va_fourcc(format).expect("a VA-API format");
		let display = moq_vaapi::Display::open()?;
		let surface = display
			.create_surfaces::<()>(
				moq_vaapi::VA_RT_FORMAT_YUV420,
				Some(fourcc),
				size.width,
				size.height,
				// The hint a decoder uses, so the driver picks the layout it
				// would pick for real decoded frames rather than a friendlier
				// one it keeps for uploads.
				Some(moq_vaapi::UsageHint::USAGE_HINT_DECODER | moq_vaapi::UsageHint::USAGE_HINT_EXPORT),
				vec![()],
			)
			.map_err(|e| eprintln!("no {fourcc:#x} surface on this driver: {e}"))
			.ok()?
			.pop()
			.expect("one surface");

		upload(&display, &surface, format, size, pixels)?;

		let exported = surface
			.export_prime()
			.map_err(|e| eprintln!("this driver will not export a {fourcc:#x} surface: {e}"))
			.ok()?;
		let layer = exported.layers.first().expect("an exported layer");
		let planes = (0..layer.num_planes as usize)
			.map(|index| DmaBufPlane::new(layer.offset[index], layer.pitch[index]))
			.collect();
		let object = exported.objects.first().expect("an exported object");
		let modifier = object.drm_format_modifier;
		let fd = object.fd.try_clone().expect("duplicate the exported descriptor");

		// The exported extent is the driver's padded allocation; the frame is
		// the size that was asked for, and imports at that size.
		Some(
			DmaBuf::new(
				format,
				modifier,
				size.width,
				size.height,
				planes,
				None,
				Arc::new(Exported(fd)),
			)
			.expect("a well-formed DMA-BUF"),
		)
	}

	/// Copy tightly packed planes into a surface, honoring the image's own plane
	/// offsets and pitches. A `vaCreateImage` upload rather than a derived one,
	/// so the driver writes the pixels into whatever layout the surface has.
	fn upload(
		display: &Arc<moq_vaapi::Display>,
		surface: &moq_vaapi::Surface<()>,
		format: DrmFormat,
		size: Size,
		pixels: &[u8],
	) -> Option<()> {
		let fourcc = va_fourcc(format).expect("a VA-API format");
		let image_format = display
			.query_image_formats()
			.expect("query image formats")
			.into_iter()
			.find(|image| image.fourcc == fourcc)
			.or_else(|| {
				eprintln!("this driver has no {fourcc:#x} image format");
				None
			})?;

		let mut image = moq_vaapi::Image::create_from(surface, image_format, surface.size(), surface.size())
			.map_err(|e| eprintln!("this driver will not create a {fourcc:#x} image: {e}"))
			.ok()?;
		let va_image = *image.image();
		let destination: &mut [u8] = image.as_mut();

		let mut source = pixels;
		for (plane, (rows, length)) in rows(format, size).into_iter().enumerate() {
			let (pitch, offset) = (va_image.pitches[plane] as usize, va_image.offsets[plane] as usize);
			for row in 0..rows {
				let start = offset + row * pitch;
				destination[start..start + length].copy_from_slice(&source[row * length..(row + 1) * length]);
			}
			source = &source[rows * length..];
		}

		// `vaPutImage` runs on drop, and the surface has to be idle before
		// anything exports it.
		drop(image);
		surface.sync().expect("sync the uploaded surface");
		Some(())
	}
}

/// The VA-API fourcc naming the same layout as a DRM format.
///
/// DRM spells a packed pixel from its most significant byte down and VA-API from
/// its first byte in memory, so one layout has two reversed names. The planar
/// formats' two vocabularies agree.
#[cfg(all(test, feature = "vaapi"))]
fn va_fourcc(format: DrmFormat) -> Result<u32, Error> {
	match format {
		DrmFormat::XRGB8888 => Ok(moq_vaapi::VA_FOURCC_BGRX),
		DrmFormat::ARGB8888 => Ok(moq_vaapi::VA_FOURCC_BGRA),
		DrmFormat::XBGR8888 => Ok(moq_vaapi::VA_FOURCC_RGBX),
		DrmFormat::ABGR8888 => Ok(moq_vaapi::VA_FOURCC_RGBA),
		DrmFormat::NV12 => Ok(moq_vaapi::VA_FOURCC_NV12),
		DrmFormat::YUV420 => Ok(moq_vaapi::VA_FOURCC_I420),
		format => Err(err(format!(
			"no VA-API format for DMA-BUF format {:#x}",
			format.as_raw()
		))),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The plane count a format calls for has to match the buffer's, or a
	/// missing chroma plane renders as a plausible-looking green picture instead
	/// of an error.
	#[test]
	fn a_buffer_short_a_plane_is_refused() {
		let size = Size::new(64, 64);
		let shape = Shape::of(DrmFormat::NV12, size).expect("NV12 is importable");

		let luma_only = [DmaBufPlane::new(0, 64)];
		assert!(shape.planes(&luma_only).is_err());

		let both = [DmaBufPlane::new(0, 64), DmaBufPlane::new(64 * 64, 64)];
		let planes = shape.planes(&both).expect("a complete NV12 buffer");
		assert_eq!(planes.len(), 2);
		assert_eq!(planes[0].format, wgpu::TextureFormat::R8Unorm);
		assert_eq!(planes[0].size, size);
		assert_eq!(planes[1].format, wgpu::TextureFormat::Rg8Unorm);
		// Half in both axes: 4:2:0 chroma is one two-channel texel per 2x2 luma.
		assert_eq!(planes[1].size, Size::new(32, 32));
		assert_eq!(planes[1].offset, 64 * 64);
	}

	/// The producer's pitch is what addresses a row, not the width, so it has to
	/// survive into the import unchanged. A buffer padded to a tile boundary
	/// shears progressively down the frame when this is dropped.
	#[test]
	fn the_producers_pitch_reaches_each_plane() {
		let shape = Shape::of(DrmFormat::NV12, Size::new(60, 40)).expect("NV12 is importable");
		let planes = shape
			.planes(&[DmaBufPlane::new(0, 64), DmaBufPlane::new(64 * 48, 64)])
			.expect("a complete NV12 buffer");

		assert_eq!(planes[0].stride, 64, "luma keeps the padded pitch, not the width");
		assert_eq!(planes[1].stride, 64, "chroma is half as wide but two bytes a texel");
	}

	/// Packed formats stay one image, and needs no color matrix.
	#[test]
	fn packed_formats_import_as_a_single_plane() {
		let size = Size::new(64, 64);
		for (format, expected) in [
			(DrmFormat::XRGB8888, wgpu::TextureFormat::Bgra8Unorm),
			(DrmFormat::ABGR8888, wgpu::TextureFormat::Rgba8Unorm),
		] {
			let shape = Shape::of(format, size).expect("a packed format is importable");
			assert_eq!(shape.layout, Layout::Rgba);
			let planes = shape.planes(&[DmaBufPlane::new(0, 64 * 4)]).expect("one plane");
			assert_eq!(planes.len(), 1);
			assert_eq!(planes[0].format, expected);
		}
	}

	/// 4:2:0 chroma has no meaning at an odd width, so the format is refused
	/// rather than imported with a plane rounded down a pixel.
	#[test]
	fn odd_dimensions_are_refused_for_subsampled_formats() {
		assert!(Shape::of(DrmFormat::NV12, Size::new(65, 64)).is_err());
		assert!(Shape::of(DrmFormat::YUV420, Size::new(64, 65)).is_err());
		assert!(Shape::of(DrmFormat::XRGB8888, Size::new(65, 65)).is_ok());
	}
}

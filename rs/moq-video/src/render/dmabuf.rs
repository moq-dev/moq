//! Zero-copy import of packed Linux DMA-BUFs into wgpu's Vulkan backend.
//!
//! wgpu owns the Vulkan image and imported fd once wrapping succeeds. The
//! [`DmaBuf`] itself rides with the submitted render work separately, keeping
//! the dequeued producer buffer out of PipeWire's pool until the GPU is done.

use wgpu::hal::MemoryFlags;

use super::source::{Layout, Source};
use crate::{DmaBuf, DrmFormat, Error, Size};

fn err(message: impl std::fmt::Display) -> Error {
	Error::Render(anyhow::anyhow!("{message}"))
}

/// Alias one packed DMA-BUF allocation as a sampled Vulkan texture.
pub(super) fn import(device: &wgpu::Device, buffer: &DmaBuf) -> Result<Option<Source>, Error> {
	if !device
		.features()
		.contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
	{
		return Ok(None);
	}

	let format = match buffer.format() {
		DrmFormat::XRGB8888 | DrmFormat::ARGB8888 => wgpu::TextureFormat::Bgra8Unorm,
		DrmFormat::XBGR8888 | DrmFormat::ABGR8888 => wgpu::TextureFormat::Rgba8Unorm,
		format => return Err(err(format!("cannot import DMA-BUF format {:#x}", format.as_raw()))),
	};
	let [plane] = buffer.planes() else {
		return Err(err("packed DMA-BUF must have exactly one plane"));
	};
	let size = Size::new(buffer.width(), buffer.height());
	let extent = wgpu::Extent3d {
		width: size.width,
		height: size.height,
		depth_or_array_layers: 1,
	};
	let descriptor = wgpu::TextureDescriptor {
		label: Some("moq-video imported DMA-BUF"),
		size: extent,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format,
		usage: wgpu::TextureUsages::TEXTURE_BINDING,
		view_formats: &[],
	};
	let hal_descriptor = wgpu::hal::TextureDescriptor {
		label: descriptor.label,
		size: extent,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format,
		usage: wgpu::TextureUses::RESOURCE,
		memory_flags: MemoryFlags::empty(),
		view_formats: Vec::new(),
	};

	// SAFETY: the guard is only used to import a descriptor into the same
	// Vulkan device. It drops before the resulting HAL texture is wrapped.
	let Some(hal) = (unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }) else {
		return Ok(None);
	};
	let export = buffer
		.export()
		.map_err(|e| Error::Render(anyhow::Error::new(e).context("export DMA-BUF")))?;
	let (fd, keepalive) = export.into_parts();
	// SAFETY: `fd` is a fresh duplicate of this live DMA-BUF. Export waited for
	// producer writes, and the format, modifier, extent, stride, and offset come
	// from PipeWire's buffer metadata. Vulkan consumes the duplicate on success
	// and wgpu-hal closes it on error.
	let texture = unsafe {
		hal.texture_from_dmabuf_fd(
			fd,
			&hal_descriptor,
			buffer.modifier(),
			plane.stride() as u64,
			plane.offset() as u64,
		)
	}
	.map_err(|e| err(format!("Vulkan DMA-BUF import: {e:?}")))?;
	drop(hal);

	// SAFETY: wgpu-hal created `texture` on this device from `hal_descriptor`,
	// which exactly matches the public descriptor. Imported pixels are already
	// initialized and will first be used as a sampled resource.
	let texture = unsafe {
		device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(texture, &descriptor, wgpu::TextureUses::RESOURCE)
	};
	let view = texture.create_view(&Default::default());

	Ok(Some(Source {
		layout: Layout::Rgba,
		color: None,
		plane0: view.clone(),
		plane1: view.clone(),
		plane2: view,
		keepalive: Some(Box::new(keepalive)),
	}))
}

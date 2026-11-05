//! Open a `wgpu` device ready for this module's zero-copy paths.

use crate::Error;

/// Open `adapter`'s device as `descriptor` asks, plus what this module's
/// zero-copy paths need where the adapter has it.
///
/// On Linux that is DMA-BUF import for [`Renderer`](super::Renderer), and the
/// Vulkan extension that shares a timeline semaphore, which
/// [`Exporter`](super::export::Exporter) hands an encoder. wgpu enables neither on its
/// own. Elsewhere this is [`wgpu::Adapter::request_device`].
pub async fn request_device(
	adapter: &wgpu::Adapter,
	descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue), Error> {
	#[cfg_attr(not(target_os = "linux"), allow(unused_mut, reason = "only Linux adds to it"))]
	let mut descriptor = descriptor.clone();
	#[cfg(target_os = "linux")]
	{
		descriptor.required_features |= adapter.features() & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
		if let Some(opened) = vulkan(adapter, &descriptor)? {
			return Ok(opened);
		}
	}
	adapter
		.request_device(&descriptor)
		.await
		.map_err(|e| Error::Render(anyhow::Error::new(e).context("open the GPU")))
}

/// Open a Vulkan device with `VK_KHR_external_semaphore_fd` added, or `None`
/// when the adapter is not Vulkan, lacks the extension, or cannot satisfy
/// `descriptor` (so wgpu's own request reports why).
#[cfg(target_os = "linux")]
fn vulkan(
	adapter: &wgpu::Adapter,
	descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<Option<(wgpu::Device, wgpu::Queue)>, Error> {
	let extension = ash::khr::external_semaphore_fd::NAME;
	if !adapter.features().contains(descriptor.required_features)
		|| !descriptor.required_limits.check_limits(&adapter.limits())
	{
		return Ok(None);
	}
	// SAFETY: the guard is only used to open a device, and drops before wgpu
	// adopts it.
	let Some(hal) = (unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }) else {
		return Ok(None);
	};
	if !hal.physical_device_capabilities().supports_extension(extension) {
		return Ok(None);
	}
	// SAFETY: the callback only adds an extension the physical device supports,
	// and removes nothing.
	let open = unsafe {
		hal.open_with_callback(
			descriptor.required_features,
			&descriptor.required_limits,
			&descriptor.memory_hints,
			Some(Box::new(move |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs| {
				if !args.extensions.contains(&extension) {
					args.extensions.push(extension);
				}
			})),
		)
	}
	.map_err(|e| Error::Render(anyhow::Error::new(e).context("open the Vulkan device")))?;
	drop(hal);
	// SAFETY: `open` came from this adapter with exactly the descriptor's
	// features.
	let opened = unsafe { adapter.create_device_from_hal(open, descriptor) }
		.map_err(|e| Error::Render(anyhow::Error::new(e).context("open the GPU")))?;
	Ok(Some(opened))
}

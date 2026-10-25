//! Export through external Vulkan images allocated on wgpu's own device.
//!
//! Each slot is a dedicated `R8G8B8A8_UNORM` image whose memory and timeline
//! semaphore are exported as opaque fds, in the [`frame::vulkan`](crate::frame::vulkan)
//! contract any importer reads. wgpu adopts the image as a texture, and a
//! compute pass writes the picture into it as a storage image, which is what
//! leaves it in the `GENERAL` layout the contract hands over.
//!
//! The ready value is signalled from the host once the copy's submission has
//! finished, rather than by the submission itself: wgpu signals an extra
//! semaphore on whatever submission comes next on the queue, which another
//! thread drawing a window may make before this one. Waiting on the submission
//! costs the encoding thread the same wait a readback pays, without the copy.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use ash::vk;

use super::{err, extent, finish};
use crate::frame::vulkan::{self, Completion, Format, Handles, Image, Memory, Slot, Timeline};
use crate::{Error, Size, Surface};

const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
	vk::ImageUsageFlags::TRANSFER_DST.as_raw()
		| vk::ImageUsageFlags::SAMPLED.as_raw()
		| vk::ImageUsageFlags::STORAGE.as_raw(),
);

const SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
	if (any(id.xy >= textureDimensions(output))) {
		return;
	}
	textureStore(output, id.xy, textureLoad(source, id.xy, 0));
}
"#;

pub(super) struct Export {
	device: wgpu::Device,
	queue: wgpu::Queue,
	raw: Arc<ash::Device>,
	memory_fd: ash::khr::external_memory_fd::Device,
	semaphore_fd: ash::khr::external_semaphore_fd::Device,
	memory: vk::PhysicalDeviceMemoryProperties,
	identity: vulkan::Device,
	size: Size,
	pipeline: wgpu::ComputePipeline,
	layout: wgpu::BindGroupLayout,
	pool: Arc<Mutex<Pool>>,
}

/// Slots not held by a [`Copy`].
#[derive(Default)]
struct Pool {
	free: Vec<Slot<Target>>,
	/// Slots an encoder holds, back once it is done reading.
	busy: Vec<Completion<Target>>,
	/// Slots in existence, up to [`Exporter::DEPTH`](super::Exporter::DEPTH).
	allocated: usize,
}

/// What a slot keeps alive on the producer side.
struct Target {
	/// Holds the texture, which owns the image and its memory; wgpu frees them
	/// once no submission uses them.
	view: wgpu::TextureView,
	raw: Arc<ash::Device>,
	semaphore: vk::Semaphore,
	/// The last timeline value handed out.
	value: u64,
}

impl Drop for Target {
	fn drop(&mut self) {
		// SAFETY: the semaphore is this target's own and only ever signalled from
		// the host, so no queue operation is waiting on it. The view still holds
		// wgpu's device, and so the raw device, alive.
		unsafe { self.raw.destroy_semaphore(self.semaphore, None) };
	}
}

impl Export {
	/// Export from `device`, failing with what it lacks when it cannot share
	/// images and semaphores this way.
	pub fn open(device: &wgpu::Device, queue: &wgpu::Queue, size: Size) -> Result<Self, Error> {
		// SAFETY: the guard is only read for handles, and drops before this returns.
		let Some(hal) = (unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }) else {
			return Err(err("it is not a Vulkan device; open wgpu on its Vulkan backend"));
		};
		let extensions = hal.enabled_device_extensions();
		for extension in [
			ash::khr::external_memory_fd::NAME,
			ash::khr::external_semaphore_fd::NAME,
		] {
			if !extensions.contains(&extension) {
				return Err(err(format!(
					"it was opened without {}; open it with moq_video::render::request_device, \
					 which enables it where the driver has it",
					extension.to_string_lossy()
				)));
			}
		}
		let shared = hal.shared_instance();
		if shared.instance_api_version() < vk::API_VERSION_1_2 {
			return Err(err("its Vulkan instance is older than 1.2"));
		}
		let instance = shared.raw_instance().clone();
		let physical = hal.raw_physical_device();
		let raw = Arc::new(hal.raw_device().clone());
		drop(hal);

		// SAFETY: `physical` is the live physical device behind `device`, and
		// every chained structure outlives the queries.
		let identity = unsafe {
			if instance.get_physical_device_properties(physical).api_version < vk::API_VERSION_1_2 {
				return Err(err("its Vulkan device is older than 1.2"));
			}

			let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
			let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut timeline);
			instance.get_physical_device_features2(physical, &mut features);
			// wgpu enables the feature whenever the device has it.
			if timeline.timeline_semaphore == vk::FALSE {
				return Err(err("its driver has no timeline semaphores"));
			}

			let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
				.handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
			let info = vk::PhysicalDeviceImageFormatInfo2::default()
				.format(vk::Format::R8G8B8A8_UNORM)
				.ty(vk::ImageType::TYPE_2D)
				.tiling(vk::ImageTiling::OPTIMAL)
				.usage(USAGE)
				.push_next(&mut external);
			let mut exportable = vk::ExternalImageFormatProperties::default();
			let mut properties = vk::ImageFormatProperties2::default().push_next(&mut exportable);
			if let Err(e) = instance.get_physical_device_image_format_properties2(physical, &info, &mut properties) {
				return Err(err(format!("its driver cannot export RGBA images as fds: {e}")));
			}
			let limit = properties.image_format_properties.max_extent;
			if !exportable
				.external_memory_properties
				.external_memory_features
				.contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
			{
				return Err(err("its driver cannot export RGBA image memory as an fd"));
			}
			if size.width > limit.width || size.height > limit.height {
				return Err(err(format!(
					"{size} is larger than its exportable images ({}x{})",
					limit.width, limit.height
				)));
			}

			let mut kind = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
			let info = vk::PhysicalDeviceExternalSemaphoreInfo::default()
				.handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
				.push_next(&mut kind);
			let mut semaphore = vk::ExternalSemaphoreProperties::default();
			instance.get_physical_device_external_semaphore_properties(physical, &info, &mut semaphore);
			if !semaphore
				.external_semaphore_features
				.contains(vk::ExternalSemaphoreFeatureFlags::EXPORTABLE)
			{
				return Err(err("its driver cannot export timeline semaphores as fds"));
			}

			identity(&instance, physical)?
		};

		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-video export"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::COMPUTE,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: false },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::COMPUTE,
					ty: wgpu::BindingType::StorageTexture {
						access: wgpu::StorageTextureAccess::WriteOnly,
						format: wgpu::TextureFormat::Rgba8Unorm,
						view_dimension: wgpu::TextureViewDimension::D2,
					},
					count: None,
				},
			],
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq-video export"),
			source: wgpu::ShaderSource::Wgsl(SHADER.into()),
		});
		let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
			label: Some("moq-video export"),
			layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("moq-video export"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			})),
			module: &shader,
			entry_point: Some("main"),
			compilation_options: Default::default(),
			cache: None,
		});

		Ok(Self {
			device: device.clone(),
			queue: queue.clone(),
			memory_fd: ash::khr::external_memory_fd::Device::new(&instance, &raw),
			semaphore_fd: ash::khr::external_semaphore_fd::Device::new(&instance, &raw),
			// SAFETY: as above.
			memory: unsafe { instance.get_physical_device_memory_properties(physical) },
			raw,
			identity,
			size,
			pipeline,
			layout,
			pool: Arc::default(),
		})
	}

	/// The device an importer must open on.
	pub fn device(&self) -> vulkan::Device {
		self.identity
	}

	pub fn copy(&mut self, texture: &wgpu::Texture) -> Result<Option<Copy>, Error> {
		let Some(slot) = self.reserve()? else {
			return Ok(None);
		};

		let view = texture.create_view(&Default::default());
		let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("moq-video export"),
			layout: &self.layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: wgpu::BindingResource::TextureView(&view),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: wgpu::BindingResource::TextureView(&slot.owner().view),
				},
			],
		});
		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-video export"),
		});
		{
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
				label: Some("moq-video export"),
				timestamp_writes: None,
			});
			pass.set_pipeline(&self.pipeline);
			pass.set_bind_group(0, &bind, &[]);
			pass.dispatch_workgroups(self.size.width.div_ceil(8), self.size.height.div_ceil(8), 1);
		}
		let submission = self.queue.submit([encoder.finish()]);

		Ok(Some(Copy {
			slot: Some(slot),
			submission,
			pool: self.pool.clone(),
		}))
	}

	/// A slot to copy into: one the encoder gave back, or a new one while the
	/// pool has room.
	fn reserve(&mut self) -> Result<Option<Slot<Target>>, Error> {
		let mut pool = self.pool.lock().unwrap();
		let mut index = 0;
		while index < pool.busy.len() {
			match pool.busy[index].try_wait() {
				Ok(Some(slot)) => {
					pool.busy.swap_remove(index);
					pool.free.push(slot);
				}
				Ok(None) => index += 1,
				// Nothing read the frame, so nothing signalled its completion and
				// the image is not safe to write again. Let it go and make another.
				Err(_) => {
					pool.busy.swap_remove(index);
					pool.allocated -= 1;
				}
			}
		}
		if let Some(slot) = pool.free.pop() {
			return Ok(Some(slot));
		}
		if pool.allocated >= super::Exporter::DEPTH {
			return Ok(None);
		}
		let slot = self.allocate()?;
		pool.allocated += 1;
		Ok(Some(slot))
	}

	/// Wait for every slot the encoder holds to come back, so a test can expect
	/// the next copy to reuse one instead of racing the completion worker.
	#[cfg(all(test, feature = "nvidia"))]
	async fn settle(&self) {
		let busy = std::mem::take(&mut self.pool.lock().unwrap().busy);
		for completion in busy {
			let returned = completion.wait().await;
			let mut pool = self.pool.lock().unwrap();
			match returned {
				Ok(slot) => pool.free.push(slot),
				Err(_) => pool.allocated -= 1,
			}
		}
	}

	/// Allocate one exportable image and its timeline semaphore.
	fn allocate(&self) -> Result<Slot<Target>, Error> {
		let vk_err = |what: &str| {
			let what = what.to_string();
			move |e: vk::Result| err(format!("Vulkan export {what}: {e}"))
		};

		let mut external =
			vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
		let info = vk::ImageCreateInfo::default()
			.push_next(&mut external)
			.image_type(vk::ImageType::TYPE_2D)
			.format(vk::Format::R8G8B8A8_UNORM)
			.extent(vk::Extent3D {
				width: self.size.width,
				height: self.size.height,
				depth: 1,
			})
			.mip_levels(1)
			.array_layers(1)
			.samples(vk::SampleCountFlags::TYPE_1)
			.tiling(vk::ImageTiling::OPTIMAL)
			.usage(USAGE)
			.sharing_mode(vk::SharingMode::EXCLUSIVE)
			.initial_layout(vk::ImageLayout::UNDEFINED);
		// SAFETY: the create info chains only locals that outlive the call.
		let image = unsafe { self.raw.create_image(&info, None) }.map_err(vk_err("image"))?;
		// Destroys what exists so far if a later step fails.
		let mut cleanup = Cleanup {
			raw: &self.raw,
			image,
			memory: vk::DeviceMemory::null(),
			semaphore: vk::Semaphore::null(),
		};

		// SAFETY: `image` is live and unbound.
		let requirements = unsafe { self.raw.get_image_memory_requirements(image) };
		let memory_type = (0..self.memory.memory_type_count)
			.find(|&index| {
				requirements.memory_type_bits & (1 << index) != 0
					&& self.memory.memory_types[index as usize]
						.property_flags
						.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
			})
			.ok_or_else(|| err("no device-local memory type for an exportable image"))?;
		let mut export =
			vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
		let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
		let allocation = vk::MemoryAllocateInfo::default()
			.allocation_size(requirements.size)
			.memory_type_index(memory_type)
			.push_next(&mut export)
			.push_next(&mut dedicated);
		// SAFETY: the allocate info chains only locals, and the memory is bound
		// once, at offset zero, to the image it is dedicated to.
		cleanup.memory = unsafe { self.raw.allocate_memory(&allocation, None) }.map_err(vk_err("allocation"))?;
		unsafe { self.raw.bind_image_memory(image, cleanup.memory, 0) }.map_err(vk_err("bind"))?;

		let mut kind = vk::SemaphoreTypeCreateInfo::default()
			.semaphore_type(vk::SemaphoreType::TIMELINE)
			.initial_value(0);
		let mut shared =
			vk::ExportSemaphoreCreateInfo::default().handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
		let info = vk::SemaphoreCreateInfo::default()
			.push_next(&mut kind)
			.push_next(&mut shared);
		// SAFETY: as above.
		cleanup.semaphore = unsafe { self.raw.create_semaphore(&info, None) }.map_err(vk_err("semaphore"))?;

		// SAFETY: both objects were created exportable as opaque fds, and each
		// call returns a new descriptor the caller owns.
		let handles = unsafe {
			let memory = self
				.memory_fd
				.get_memory_fd(
					&vk::MemoryGetFdInfoKHR::default()
						.memory(cleanup.memory)
						.handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
				)
				.map_err(vk_err("memory fd"))?;
			let memory = OwnedFd::from_raw_fd(memory);
			let timeline = self
				.semaphore_fd
				.get_semaphore_fd(
					&vk::SemaphoreGetFdInfoKHR::default()
						.semaphore(cleanup.semaphore)
						.handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
				)
				.map_err(vk_err("semaphore fd"))?;
			Handles {
				memory,
				timeline: OwnedFd::from_raw_fd(timeline),
			}
		};

		let format = wgpu::TextureFormat::Rgba8Unorm;
		let usage =
			wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING;
		let descriptor = wgpu::TextureDescriptor {
			label: Some("moq-video export"),
			size: extent(self.size),
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage,
			view_formats: &[],
		};
		let hal_descriptor = wgpu::hal::TextureDescriptor {
			label: descriptor.label,
			size: descriptor.size,
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUses::COPY_DST | wgpu::TextureUses::RESOURCE | wgpu::TextureUses::STORAGE_WRITE_ONLY,
			memory_flags: wgpu::hal::MemoryFlags::empty(),
			view_formats: Vec::new(),
		};
		let (image, memory, semaphore) = cleanup.release();
		// SAFETY: the guard is only used to wrap an image created on its device,
		// and drops before the texture is adopted.
		let hal = unsafe { self.device.as_hal::<wgpu::hal::api::Vulkan>() }
			.ok_or_else(|| err("wgpu device is no longer Vulkan"))?;
		// SAFETY: `image` was created on this device exactly as `hal_descriptor`
		// describes (2D, one mip and layer, RGBA8, usage matching the descriptor's),
		// and wgpu takes ownership of it and its dedicated memory.
		let texture = unsafe {
			hal.texture_from_raw(
				image,
				&hal_descriptor,
				None,
				wgpu::hal::vulkan::TextureMemory::Dedicated(memory),
			)
		};
		drop(hal);
		// SAFETY: as above; the image starts in `UNDEFINED`, which is the
		// uninitialized state.
		let texture = unsafe {
			self.device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
				texture,
				&descriptor,
				wgpu::TextureUses::UNINITIALIZED,
			)
		};
		let target = Target {
			view: texture.create_view(&Default::default()),
			raw: self.raw.clone(),
			semaphore,
			value: 0,
		};

		let image = Image {
			device: self.identity,
			memory: Memory::OpaqueFd { memory_type },
			size: self.size,
			allocation_size: requirements.size,
			format: Format::Rgba8,
		};
		Slot::new(handles, image, target)
	}
}

/// Destroys a partly built slot's objects unless released.
struct Cleanup<'a> {
	raw: &'a ash::Device,
	image: vk::Image,
	memory: vk::DeviceMemory,
	semaphore: vk::Semaphore,
}

impl Cleanup<'_> {
	fn release(mut self) -> (vk::Image, vk::DeviceMemory, vk::Semaphore) {
		let image = std::mem::take(&mut self.image);
		let memory = std::mem::take(&mut self.memory);
		let semaphore = std::mem::take(&mut self.semaphore);
		(image, memory, semaphore)
	}
}

impl Drop for Cleanup<'_> {
	fn drop(&mut self) {
		// SAFETY: these objects are this slot's own, never submitted, and
		// destroying a null handle is a no-op.
		unsafe {
			self.raw.destroy_semaphore(self.semaphore, None);
			self.raw.destroy_image(self.image, None);
			self.raw.free_memory(self.memory, None);
		}
	}
}

/// One copy into a slot, not yet handed to the encoder.
pub(super) struct Copy {
	slot: Option<Slot<Target>>,
	submission: wgpu::SubmissionIndex,
	pool: Arc<Mutex<Pool>>,
}

impl Copy {
	pub fn wait(mut self, device: &wgpu::Device) -> Result<Surface, Error> {
		finish(device, self.submission.clone())?;
		let mut slot = self.slot.take().expect("held until wait");
		let target = slot.owner_mut();
		let ready = target.value + 1;
		target.value += 2;
		let timeline = Timeline::new(ready, target.value)?;
		let signal = vk::SemaphoreSignalInfo::default()
			.semaphore(target.semaphore)
			.value(ready);
		// SAFETY: the semaphore is a live timeline semaphore of `raw`'s, and
		// `ready` exceeds every value signalled on it before.
		let signalled = unsafe { target.raw.signal_semaphore(&signal) };
		if let Err(e) = signalled {
			// Still idle, so the slot goes back for the next copy.
			self.pool.lock().unwrap().free.push(slot);
			return Err(err(format!("Vulkan export signal: {e}")));
		}

		let (frame, completion) = match slot.publish(timeline) {
			Ok(published) => published,
			Err(e) => {
				let (e, slot) = e.into_parts();
				self.pool.lock().unwrap().free.push(slot);
				return Err(e);
			}
		};
		self.pool.lock().unwrap().busy.push(completion);
		Ok(Surface::Vulkan(frame))
	}
}

impl Drop for Copy {
	fn drop(&mut self) {
		// Never published, so the slot is still the producer's: a later copy on
		// the same queue runs after this one.
		if let Some(slot) = self.slot.take() {
			self.pool.lock().unwrap().free.push(slot);
		}
	}
}

/// The UUIDs, and the render node where the driver names one, that tell an
/// importer which GPU and driver own the memory.
///
/// # Safety
///
/// `physical` belongs to `instance`.
unsafe fn identity(instance: &ash::Instance, physical: vk::PhysicalDevice) -> Result<vulkan::Device, Error> {
	// SAFETY: the caller's contract.
	let drm = unsafe { instance.enumerate_device_extension_properties(physical) }
		.map_err(|e| err(format!("Vulkan device extensions: {e}")))?
		.iter()
		.any(|extension| extension.extension_name_as_c_str() == Ok(ash::ext::physical_device_drm::NAME));

	let mut id = vk::PhysicalDeviceIDProperties::default();
	let mut node = vk::PhysicalDeviceDrmPropertiesEXT::default();
	let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
	if drm {
		properties = properties.push_next(&mut node);
	}
	// SAFETY: the caller's contract; the DRM structure is chained only when the
	// device has the extension that defines it.
	unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
	Ok(vulkan::Device {
		device_uuid: id.device_uuid,
		driver_uuid: id.driver_uuid,
		render_node: (node.has_render == vk::TRUE)
			.then(|| libc::makedev(node.render_major as libc::c_uint, node.render_minor as libc::c_uint) as u64),
	})
}

#[cfg(test)]
mod tests {
	use std::os::fd::IntoRawFd;

	use moq_net::Timestamp;

	use super::super::tests::{gpu, pixels, texture};
	use super::super::{Exporter, Route};
	use super::*;

	/// The exported memory, imported again as a second image the way an
	/// importer on the same GPU would, holds each picture once its ready value
	/// is reached.
	#[tokio::test]
	#[ignore = "needs a Vulkan device that exports memory; run by just rs gpu"]
	async fn vulkan_export_round_trips() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let size = Size::new(64, 32);
		let mut exporter = Exporter::open(&device, &queue, size, Route::Gpu).unwrap();
		let identity = exporter.input().expect("a Vulkan exporter names its device");

		let mut held = Vec::new();
		for rgba in [[255, 0, 0, 255], [0, 255, 0, 255], [9, 99, 199, 255]] {
			let source = texture(&device, &queue, size, rgba);
			let pending = exporter.copy(&source, Timestamp::ZERO).unwrap().expect("a free slot");
			let frame = pending.wait().unwrap();
			let Surface::Vulkan(image) = &frame.surface else {
				panic!("exported a CPU frame");
			};
			assert_eq!(image.image().device, identity);
			assert_eq!(image.size(), size);
			assert_eq!(image.format(), Format::Rgba8);
			assert_eq!(
				import(&device, &queue, image),
				rgba.repeat((size.width * size.height) as usize)
			);
			held.push(frame);
		}

		// The encoder holds every slot, so the next picture is dropped.
		let source = texture(&device, &queue, size, [0; 4]);
		assert!(exporter.copy(&source, Timestamp::ZERO).unwrap().is_none());
		// A frame dropped unread never completes its slot, so it is replaced
		// rather than written while an importer might still read it.
		held.pop();
		assert!(exporter.copy(&source, Timestamp::ZERO).unwrap().is_some());
	}

	/// NVENC opens on the exported image's GPU, encodes each copy in place, and
	/// gives every slot back once it has read it, so the pool never grows past
	/// its depth.
	#[cfg(feature = "nvidia")]
	#[tokio::test]
	#[ignore = "requires a Linux NVIDIA GPU with NVENC; run by just rs gpu"]
	async fn nvenc_encodes_the_export_in_place() {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.enumerate_adapters(wgpu::Backends::VULKAN)
			.await
			.into_iter()
			.find(|adapter| adapter.get_info().vendor == 0x10de)
			.expect("an NVIDIA Vulkan adapter");
		let (device, queue) = super::super::super::request_device(&adapter, &Default::default())
			.await
			.unwrap();
		let size = Size::new(320, 240);
		let mut config = crate::encode::Config::new(size.width, size.height, crate::FrameRate::new(30, 1).unwrap());
		let (mut exporter, _) = Exporter::new(&device, &queue, &mut config).await.unwrap();
		assert_eq!(exporter.route(), Route::Gpu);
		assert_eq!(config.input, exporter.input());
		let mut encoder = crate::encode::Encoder::new(&config).unwrap();
		assert_eq!(encoder.name(), "nvenc");

		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let mut encoded = Vec::new();
		for index in 0..Exporter::DEPTH as u64 * 4 {
			let timestamp = Timestamp::from_micros(index * 33_333).unwrap();
			let pending = exporter.copy(&red, timestamp).unwrap().expect("a slot came back");
			let frame = pending.wait().unwrap();
			assert!(matches!(frame.surface, Surface::Vulkan(_)));
			encoded.extend(encoder.encode(&frame).unwrap());
			drop(frame);
			let super::super::Slots::Vulkan(export) = &exporter.slots else {
				unreachable!("checked the route above");
			};
			export.settle().await;
			assert!(
				export.pool.lock().unwrap().allocated <= 1,
				"a slot was replaced rather than reused"
			);
		}
		encoded.extend(encoder.finish().unwrap());
		assert_eq!(encoded.len(), Exporter::DEPTH * 4);
		assert!(encoded[0].keyframe);
	}

	/// Import `image` into `device` as its contract describes, check its ready
	/// value was reached, and read its pixels.
	fn import(device: &wgpu::Device, queue: &wgpu::Queue, image: &vulkan::Frame) -> Vec<u8> {
		// SAFETY: test-only interop on the device the image was exported from;
		// every object is created from the contract and the exported handles.
		unsafe {
			let hal = device.as_hal::<wgpu::hal::api::Vulkan>().unwrap();
			let raw = hal.raw_device().clone();
			let instance = hal.shared_instance().raw_instance().clone();
			drop(hal);
			let handles = image.handles().unwrap();
			let contract = image.image();

			let mut kind = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
			let semaphore = raw
				.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut kind), None)
				.unwrap();
			ash::khr::external_semaphore_fd::Device::new(&instance, &raw)
				.import_semaphore_fd(
					&vk::ImportSemaphoreFdInfoKHR::default()
						.semaphore(semaphore)
						.handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
						.fd(handles.timeline.into_raw_fd()),
				)
				.unwrap();
			let value = raw.get_semaphore_counter_value(semaphore).unwrap();
			raw.destroy_semaphore(semaphore, None);
			assert!(value >= image.timeline().ready, "handed out before ready");

			let Memory::OpaqueFd { memory_type } = contract.memory else {
				panic!("exported DMA-BUF memory");
			};
			let mut external =
				vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
			let vk_image = raw
				.create_image(
					&vk::ImageCreateInfo::default()
						.push_next(&mut external)
						.image_type(vk::ImageType::TYPE_2D)
						.format(vk::Format::R8G8B8A8_UNORM)
						.extent(vk::Extent3D {
							width: contract.size.width,
							height: contract.size.height,
							depth: 1,
						})
						.mip_levels(1)
						.array_layers(1)
						.samples(vk::SampleCountFlags::TYPE_1)
						.tiling(vk::ImageTiling::OPTIMAL)
						.usage(USAGE)
						.sharing_mode(vk::SharingMode::EXCLUSIVE)
						.initial_layout(vk::ImageLayout::UNDEFINED),
					None,
				)
				.unwrap();
			let mut import = vk::ImportMemoryFdInfoKHR::default()
				.handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
				.fd(handles.memory.into_raw_fd());
			let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
			let memory = raw
				.allocate_memory(
					&vk::MemoryAllocateInfo::default()
						.allocation_size(contract.allocation_size)
						.memory_type_index(memory_type)
						.push_next(&mut import)
						.push_next(&mut dedicated),
					None,
				)
				.unwrap();
			raw.bind_image_memory(vk_image, memory, 0).unwrap();

			let format = wgpu::TextureFormat::Rgba8Unorm;
			let descriptor = wgpu::TextureDescriptor {
				label: Some("moq-video export import"),
				size: extent(contract.size),
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format,
				usage: wgpu::TextureUsages::COPY_DST
					| wgpu::TextureUsages::TEXTURE_BINDING
					| wgpu::TextureUsages::STORAGE_BINDING,
				view_formats: &[],
			};
			let hal = device.as_hal::<wgpu::hal::api::Vulkan>().unwrap();
			let hal_texture = hal.texture_from_raw(
				vk_image,
				&wgpu::hal::TextureDescriptor {
					label: descriptor.label,
					size: descriptor.size,
					mip_level_count: 1,
					sample_count: 1,
					dimension: wgpu::TextureDimension::D2,
					format,
					usage: wgpu::TextureUses::COPY_DST
						| wgpu::TextureUses::RESOURCE
						| wgpu::TextureUses::STORAGE_WRITE_ONLY,
					memory_flags: wgpu::hal::MemoryFlags::empty(),
					view_formats: Vec::new(),
				},
				None,
				wgpu::hal::vulkan::TextureMemory::Dedicated(memory),
			);
			drop(hal);
			// The producer left the image in `GENERAL`, the layout wgpu tracks a
			// storage texture in.
			let imported = device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
				hal_texture,
				&descriptor,
				wgpu::TextureUses::STORAGE_WRITE_ONLY,
			);

			// The contract has no `TRANSFER_SRC`, so copy it with the exporter's own
			// pipeline into a texture that can be read out.
			let plain = device.create_texture(&wgpu::TextureDescriptor {
				label: Some("moq-video export import copy"),
				usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::STORAGE_BINDING,
				..descriptor
			});
			let export = Export::open(device, queue, contract.size).unwrap();
			let source = imported.create_view(&Default::default());
			let target = plain.create_view(&Default::default());
			let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: None,
				layout: &export.layout,
				entries: &[
					wgpu::BindGroupEntry {
						binding: 0,
						resource: wgpu::BindingResource::TextureView(&source),
					},
					wgpu::BindGroupEntry {
						binding: 1,
						resource: wgpu::BindingResource::TextureView(&target),
					},
				],
			});
			let mut encoder = device.create_command_encoder(&Default::default());
			{
				let mut pass = encoder.begin_compute_pass(&Default::default());
				pass.set_pipeline(&export.pipeline);
				pass.set_bind_group(0, &bind, &[]);
				pass.dispatch_workgroups(contract.size.width.div_ceil(8), contract.size.height.div_ceil(8), 1);
			}
			queue.submit([encoder.finish()]);
			pixels(device, queue, &plain)
		}
	}
}
